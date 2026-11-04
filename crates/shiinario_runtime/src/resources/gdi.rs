//! Memory DCs and solid drawing objects for the SCN's bounded GDI calls.
use super::*;
use shiinario_scenario::GdiCommand;

const DC_TAG: u32 = 0xe000_0000;
const WHITE_BRUSH: ObjectId = ObjectId(0xd000_0001);
const BLACK_PEN: ObjectId = ObjectId(0xd000_0002);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ObjectId(u32);
#[derive(Clone, Copy)]
enum Object {
    Brush([u8; 3]),
    Pen([u8; 3]),
}
struct ContextObjects {
    brush: ObjectId,
    pen: ObjectId,
}
impl Default for ContextObjects {
    fn default() -> Self {
        Self {
            brush: WHITE_BRUSH,
            pen: BLACK_PEN,
        }
    }
}
#[derive(Default)]
pub(super) struct Gdi {
    objects: BTreeMap<ObjectId, Object>,
    selected: BTreeMap<u32, ContextObjects>,
    serial: u32,
}

pub(super) fn drawing_context(surface: u32) -> Result<u32> {
    ensure!(
        surface < 256,
        "drawing context surface is outside the slot table"
    );
    Ok(DC_TAG | surface)
}

impl Gdi {
    pub(super) fn forget_surface(&mut self, id: u32) {
        self.selected.remove(&id);
    }
    fn object(&self, id: ObjectId) -> Option<Object> {
        match id {
            WHITE_BRUSH => Some(Object::Brush([255; 3])),
            BLACK_PEN => Some(Object::Pen([0; 3])),
            _ => self.objects.get(&id).copied(),
        }
    }
    fn create(&mut self, object: Object) -> Result<u32> {
        ensure!(self.objects.len() < 65536, "GDI object limit exceeded");
        self.serial = self
            .serial
            .checked_add(1)
            .context("GDI object ID overflow")?;
        let id = 0xd001_0000u32
            .checked_add(self.serial)
            .context("GDI handle overflow")?;
        ensure!(id < DC_TAG, "GDI object handle range exhausted");
        self.objects.insert(ObjectId(id), object);
        Ok(id)
    }
    pub(super) fn respond(
        &mut self,
        command: &GdiCommand,
        surfaces: &BTreeMap<u32, DrawingSurface>,
    ) -> Result<u32> {
        let rgb = |color: u32| {
            let [r, g, b, _] = color.to_le_bytes();
            [r, g, b]
        };
        let surface_id = |dc: u32| -> Result<u32> {
            ensure!(
                dc & !255 == DC_TAG && surfaces.contains_key(&(dc & 255)),
                "GDI drawing context is not valid"
            );
            Ok(dc & 255)
        };
        match *command {
            GdiCommand::CreateSolidBrush { color } => self.create(Object::Brush(rgb(color))),
            GdiCommand::CreatePen {
                style,
                width,
                color,
            } => {
                ensure!(
                    style == 0 && (0..=1).contains(&width),
                    "only solid one-pixel GDI pens are supported"
                );
                self.create(Object::Pen(rgb(color)))
            }
            GdiCommand::SelectObject { dc, object } => {
                let surface = surface_id(dc)?;
                let Some(value) = self.object(ObjectId(object)) else {
                    return Ok(0);
                };
                let selected = self.selected.entry(surface).or_default();
                let slot = match value {
                    Object::Brush(_) => &mut selected.brush,
                    Object::Pen(_) => &mut selected.pen,
                };
                Ok(std::mem::replace(slot, ObjectId(object)).0)
            }
            GdiCommand::DeleteObject { object } => {
                let object = ObjectId(object);
                if self
                    .selected
                    .values()
                    .any(|s| s.brush == object || s.pen == object)
                {
                    return Ok(0);
                }
                if matches!(object, WHITE_BRUSH | BLACK_PEN) {
                    return Ok(1);
                }
                Ok(u32::from(self.objects.remove(&object).is_some()))
            }
            GdiCommand::Ellipse { dc, rect } => {
                let surface = surface_id(dc)?;
                let selected = self.selected.entry(surface).or_default();
                let (brush, pen) = (selected.brush, selected.pen);
                let Some(Object::Brush(fill)) = self.object(brush) else {
                    bail!("selected GDI brush is missing");
                };
                let Some(Object::Pen(outline)) = self.object(pen) else {
                    bail!("selected GDI pen is missing");
                };
                draw_ellipse(&surfaces[&surface], rect, fill, outline)?;
                Ok(1)
            }
        }
    }
}

struct Ellipse {
    spans: Vec<Option<[usize; 2]>>,
    edge: Vec<[usize; 2]>,
}
impl Ellipse {
    fn point(&mut self, x: i64, y: i64, width: usize) {
        if (0..width as i64).contains(&x) && (0..self.spans.len() as i64).contains(&y) {
            let [x, y] = [x as usize, y as usize];
            let span = self.spans[y].get_or_insert([x, x]);
            span[0] = span[0].min(x);
            span[1] = span[1].max(x);
            self.edge.push([x, y]);
        }
    }
    fn new(width: usize, height: usize) -> Self {
        let mut shape = Self {
            spans: vec![None; height],
            edge: Vec::new(),
        };
        if width == 1 && height == 1 {
            return shape;
        }
        if width <= 2 || height <= 2 {
            for y in 0..height {
                for x in 0..width {
                    shape.point(x as i64, y as i64, width);
                }
            }
        } else {
            // Step an integer conic and reflect its boundary in both axes.
            // Use the error at the start of each step for both decisions.
            let a = width as i64 - 1;
            let b = height as i64 - 1;
            let mut dx = 4 * (1 - a) * b * b;
            let mut dy = 4 * (1 + b % 2) * a * a;
            let mut error = dx + dy + b % 2 * a * a;
            let mut left = 0;
            let mut right = a;
            let mut lower = height as i64 / 2;
            let mut upper = lower - b % 2;
            while left <= right {
                for [x, y] in [[left, lower], [right, lower], [left, upper], [right, upper]] {
                    shape.point(x, y, width);
                }
                let decision = error * 2;
                if decision >= dx {
                    left += 1;
                    right -= 1;
                    dx += 8 * b * b;
                    error += dx;
                }
                if decision <= dy {
                    lower += 1;
                    upper -= 1;
                    dy += 8 * a * a;
                    error += dy;
                }
            }
            while lower - upper < b {
                for [x, y] in [
                    [left - 1, lower],
                    [right + 1, lower],
                    [left - 1, upper],
                    [right + 1, upper],
                ] {
                    shape.point(x, y, width);
                }
                lower += 1;
                upper -= 1;
            }
        }
        shape
    }
}

fn draw_ellipse(
    surface: &DrawingSurface,
    [x0, y0, x1, y1]: [i32; 4],
    fill: [u8; 3],
    outline: [u8; 3],
) -> Result<()> {
    let left = i64::from(x0.min(x1));
    let top = i64::from(y0.min(y1));
    let width = (i64::from(x1) - i64::from(x0)).abs();
    let height = (i64::from(y1) - i64::from(y0)).abs();
    ensure!(
        width <= 16384 && height <= 16384,
        "GDI ellipse dimensions exceed 16384"
    );
    if width == 0 || height == 0 {
        return Ok(());
    }
    let shape = Ellipse::new(width as usize, height as usize);
    let mut pixels = surface.pixels.read(0, surface.pixels.len())?;
    let mut paint = |x: usize, y: usize, [r, g, b]: [u8; 3]| {
        let x = left + x as i64;
        let y = top + y as i64;
        if (0..i64::from(surface.width)).contains(&x) && (0..i64::from(surface.height)).contains(&y)
        {
            let at = y as usize * surface.stride + x as usize * 3;
            pixels[at..at + 3].copy_from_slice(&[b, g, r]);
        }
    };
    for (y, span) in shape.spans.iter().enumerate() {
        if let Some([left, right]) = span {
            for x in *left..=*right {
                paint(x, y, fill);
            }
        }
    }
    for [x, y] in shape.edge {
        paint(x, y, outline);
    }
    surface.pixels.write(0, &pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_ellipses_match_wine_gdi_fixtures() {
        // Wine GDI is an independent compatibility check, not a Windows oracle.
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/gdi-ellipses.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let w = case["width"].as_u64().unwrap() as usize;
            let h = case["height"].as_u64().unwrap() as usize;
            let shape = Ellipse::new(w, h);
            let actual: Vec<u32> = shape
                .spans
                .iter()
                .map(|span| span.map_or(0, |[l, r]| ((1 << (r - l + 1)) - 1) << l))
                .collect();
            let expected: Vec<u32> = case["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u32)
                .collect();
            assert_eq!(actual, expected, "{w}x{h}");
        }
    }

    #[test]
    fn selected_objects_restore_and_cannot_be_deleted_while_in_use() -> Result<()> {
        let mut resources = Resources::default();
        resources.create_surface(3, 12, 12, 0)?;
        let dc = drawing_context(3)?;
        let mut call = |command| resources.gdi.respond(&command, &resources.surfaces);
        let brush = call(GdiCommand::CreateSolidBrush { color: 0x0000ff })?;
        let pen = call(GdiCommand::CreatePen {
            style: 0,
            width: 1,
            color: 0x0000ff,
        })?;
        let old_brush = call(GdiCommand::SelectObject { dc, object: brush })?;
        let old_pen = call(GdiCommand::SelectObject { dc, object: pen })?;
        assert_eq!(call(GdiCommand::DeleteObject { object: brush })?, 0);
        call(GdiCommand::Ellipse {
            dc,
            rect: [-2, -2, 8, 8],
        })?;
        assert_eq!(
            call(GdiCommand::SelectObject {
                dc,
                object: old_brush
            })?,
            brush
        );
        assert_eq!(
            call(GdiCommand::SelectObject {
                dc,
                object: old_pen
            })?,
            pen
        );
        assert_eq!(call(GdiCommand::DeleteObject { object: brush })?, 1);
        assert_eq!(call(GdiCommand::DeleteObject { object: pen })?, 1);
        assert!(
            call(GdiCommand::Ellipse {
                dc: 123,
                rect: [0, 0, 5, 5]
            })
            .is_err()
        );
        assert!(
            resources
                .surface(3)
                .unwrap()
                .rgba
                .as_chunks::<4>()
                .0
                .contains(&[255, 0, 0, 255])
        );
        Ok(())
    }
}
