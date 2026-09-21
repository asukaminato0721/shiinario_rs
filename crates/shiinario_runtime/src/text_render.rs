//! Native outline rasterization. Fontconfig selects an installed Japanese font;
//! script advances remain independent of font metrics. Windows font pixels are
//! not assumed identical to the selected Linux fallback.
use ab_glyph::{Font, FontArc, FontVec, PxScale, ScaleFont, point};
use anyhow::{Context, Result, ensure};
use shiinario_scenario::{SharedMemory, TextStyle};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct Canvas<'a> {
    pub pixels: &'a SharedMemory,
    pub size: [u32; 2],
    pub stride: usize,
    pub rgba: bool,
}
struct Mask {
    offset: [i32; 2],
    size: [usize; 2],
    coverage: Vec<u8>,
}
struct Face {
    name: Vec<u8>,
    weight: u32,
    font: FontArc,
}
#[derive(Default)]
pub(crate) struct TextRenderer {
    face: Option<Face>,
    masks: BTreeMap<(u32, char), Arc<Mask>>,
    cached: usize,
}
impl TextRenderer {
    fn mask(&mut self, character: char, style: &TextStyle) -> Result<Arc<Mask>> {
        ensure!(
            (1..=256).contains(&style.font_height),
            "text font height exceeds 1..256"
        );
        ensure!(style.font_weight <= 1000, "text font weight exceeds 1000");
        if self
            .face
            .as_ref()
            .is_none_or(|f| f.name != style.font_face || f.weight != style.font_weight)
        {
            let (name, _, bad) = encoding_rs::SHIFT_JIS.decode(&style.font_face);
            ensure!(!bad, "invalid CP932 font name");
            let font = load_font(&name, style.font_weight)?;
            self.face = Some(Face {
                name: style.font_face.clone(),
                weight: style.font_weight,
                font,
            });
            self.masks.clear();
            self.cached = 0;
        }
        let key = (style.font_height, character);
        if let Some(mask) = self.masks.get(&key) {
            return Ok(mask.clone());
        }
        let font = &self.face.as_ref().unwrap().font;
        let id = font.glyph_id(character);
        ensure!(id.0 != 0, "selected font has no glyph for {character:?}");
        let scale = PxScale::from(style.font_height as f32);
        let glyph = id.with_scale_and_position(scale, point(0.0, font.as_scaled(scale).ascent()));
        let mask = if let Some(outline) = font.outline_glyph(glyph) {
            let bounds = outline.px_bounds();
            let width = bounds.width() as usize;
            let height = bounds.height() as usize;
            ensure!(
                width <= 1024 && height <= 1024,
                "glyph outline exceeds bounds"
            );
            let mut coverage = vec![0; width * height];
            outline.draw(|x, y, value| {
                coverage[y as usize * width + x as usize] =
                    (value * 64.0).round().clamp(0.0, 64.0) as u8
            });
            Mask {
                offset: [bounds.min.x as i32, bounds.min.y as i32],
                size: [width, height],
                coverage,
            }
        } else {
            Mask {
                offset: [0, 0],
                size: [0, 0],
                coverage: Vec::new(),
            }
        };
        if self.cached + mask.coverage.len() > 8 * 1024 * 1024 || self.masks.len() >= 512 {
            self.masks.clear();
            self.cached = 0;
        }
        self.cached += mask.coverage.len();
        let mask = Arc::new(mask);
        self.masks.insert(key, mask.clone());
        Ok(mask)
    }

    pub fn draw(
        &mut self,
        target: Canvas<'_>,
        position: [i32; 2],
        character: char,
        style: &TextStyle,
    ) -> Result<Option<[i32; 4]>> {
        ensure!(
            style.background_mode == 1,
            "opaque text backgrounds are unresolved"
        );
        ensure!(style.opacity <= 256, "text opacity exceeds 256");
        ensure!(
            style.effects & !0xdff == 0,
            "unsupported text effects {:#x}",
            style.effects
        );
        ensure!(
            style.outline_rasterizer,
            "GDI TextOut text rasterization is unresolved"
        );
        ensure!(
            style.shadow_opacity <= 256,
            "text shadow opacity exceeds 256"
        );
        let edge = style.edge_offset.map(|v| v as i32);
        ensure!(
            edge.iter().all(|v| (-16..=16).contains(v)),
            "text edge exceeds 16 pixels"
        );
        let mask = self.mask(character, style)?;
        draw_mask(target, position, style, &mask, edge)
    }
}

fn draw_mask(
    target: Canvas<'_>,
    position: [i32; 2],
    style: &TextStyle,
    mask: &Mask,
    edge: [i32; 2],
) -> Result<Option<[i32; 4]>> {
    let opacity = if style.effects & 1 != 0 {
        style.shadow_opacity
    } else {
        style.opacity
    };
    let mut pixels = target.pixels.read(0, target.pixels.len())?;
    let origin = [
        i64::from(position[0]) + i64::from(mask.offset[0]),
        i64::from(position[1]) + i64::from(mask.offset[1]),
    ];
    let mut damage: Option<[i32; 4]> = None;
    let mut blend = |x: i64, y: i64, coverage: u8, color: [u8; 3]| {
        if x < 0
            || y < 0
            || x >= i64::from(target.size[0])
            || y >= i64::from(target.size[1])
            || coverage == 0
        {
            return;
        }
        let rect = damage.get_or_insert([x as i32, y as i32, x as i32 + 1, y as i32 + 1]);
        rect[0] = rect[0].min(x as i32);
        rect[1] = rect[1].min(y as i32);
        rect[2] = rect[2].max(x as i32 + 1);
        rect[3] = rect[3].max(y as i32 + 1);
        let alpha = ((((u32::from(coverage) * 255) >> 6) * opacity) >> 8) as i32;
        if target.rgba {
            let offset = y as usize * target.stride + x as usize * 4;
            let old_alpha = u32::from(pixels[offset + 3]);
            let source_alpha = alpha as u32;
            // Native 408eb0 tables and 432cfe image-glyph kernel. The color
            // coefficient is truncated to a byte before blending with a /256 shift.
            if source_alpha == 255 {
                pixels[offset..offset + 3].copy_from_slice(&color);
                pixels[offset + 3] = 255;
            } else {
                // Generated by the original x87 table initializer. Its
                // truncation differs by one from exact rational division at
                // some boundaries, including source alpha 127 over opaque.
                const WEIGHTS: &[u8; 65536] = include_bytes!("../data/image_text_weights.bin");
                let weight = i32::from(WEIGHTS[(source_alpha * 256 + old_alpha) as usize]);
                for channel in 0..3 {
                    let old = i32::from(pixels[offset + channel]);
                    pixels[offset + channel] =
                        (old + (((i32::from(color[channel]) - old) * weight) >> 8)) as u8;
                }
                pixels[offset + 3] = (255 - (255 - old_alpha) * (255 - source_alpha) / 255) as u8;
            }
            return;
        }
        let offset = y as usize * target.stride + x as usize * 3;
        for channel in 0..3 {
            let old = i32::from(pixels[offset + channel]);
            pixels[offset + channel] =
                (old + (((i32::from(color[2 - channel]) - old) * alpha) >> 8)) as u8;
        }
    };
    let mut stamp = |dx: i32, dy: i32, color: [u8; 3]| {
        for y in 0..mask.size[1] {
            for x in 0..mask.size[0] {
                blend(
                    origin[0] + x as i64 + i64::from(dx),
                    origin[1] + y as i64 + i64::from(dy),
                    mask.coverage[y * mask.size[0] + x],
                    color,
                );
            }
        }
    };
    if style.effects & 1 != 0 {
        stamp(
            style.shadow_offset[0] as i32,
            style.shadow_offset[1] as i32,
            style.shadow_color,
        );
    }
    for (flag, x, y) in [
        (0x40, -edge[0], -edge[1]),
        (0x80, 0, -edge[1]),
        (0x100, edge[0], -edge[1]),
        (0x10, -edge[0], 0),
        (0x20, edge[0], 0),
        (2, -edge[0], edge[1]),
        (4, 0, edge[1]),
        (8, edge[0], edge[1]),
    ] {
        if style.effects & flag != 0 {
            stamp(x, y, style.edge_color);
        }
    }
    if style.effects & 0x400 != 0 {
        for x in -edge[0]..=edge[0] {
            for y in -edge[1]..=edge[1] {
                stamp(x, y, style.edge_color);
            }
        }
    }
    // 0x800 expands each coverage sample, in source-pixel order.
    if style.effects & 0x800 != 0 {
        for y in 0..mask.size[1] {
            for x in 0..mask.size[0] {
                for dy in -edge[1]..=edge[1] {
                    for dx in -edge[0]..=edge[0] {
                        blend(
                            origin[0] + x as i64 + i64::from(dx),
                            origin[1] + y as i64 + i64::from(dy),
                            mask.coverage[y * mask.size[0] + x],
                            style.edge_color,
                        );
                    }
                }
            }
        }
    }
    // The original leaves the edge color selected when 0x800 is the only effect.
    let foreground = if style.effects & 0x800 != 0 && style.effects & 0x5ff == 0 {
        style.edge_color
    } else {
        style.color
    };
    for y in 0..mask.size[1] {
        for x in 0..mask.size[0] {
            blend(
                origin[0] + x as i64,
                origin[1] + y as i64,
                mask.coverage[y * mask.size[0] + x],
                foreground,
            );
        }
    }
    target.pixels.write(0, &pixels)?;
    Ok(damage)
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
fn load_font(name: &str, font_weight: u32) -> Result<FontArc> {
    let weight = if font_weight >= 600 { 200 } else { 80 };
    let pattern = format!("{name}:lang=ja:weight={weight}");
    let output = std::process::Command::new("fc-match")
        .args(["--format=%{file}\n%{index}\n", &pattern])
        .output()
        .context("Japanese font lookup requires Fontconfig (fc-match)")?;
    ensure!(output.status.success(), "Fontconfig font lookup failed");
    let result = std::str::from_utf8(&output.stdout).context("font path is not UTF-8")?;
    let mut lines = result.lines();
    let path = lines
        .next()
        .filter(|s| !s.is_empty())
        .context("Fontconfig returned no font")?;
    let index: u32 = lines
        .next()
        .context("Fontconfig returned no face index")?
        .parse()?;
    let length = std::fs::metadata(path)?.len();
    ensure!(length <= 32 * 1024 * 1024, "font exceeds 32 MiB");
    let font: FontArc = FontVec::try_from_vec_and_index(std::fs::read(path)?, index)
        .context("invalid font face")?
        .into();
    eprintln!("Text font: {name:?} -> {path} (face {index})");
    Ok(font)
}

#[cfg(target_os = "android")]
fn load_font(_name: &str, _weight: u32) -> Result<FontArc> {
    for path in [
        "/system/fonts/NotoSansCJK-Regular.ttc",
        "/system/fonts/NotoSansJP-Regular.otf",
        "/system/fonts/DroidSansFallback.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Ok(FontVec::try_from_vec_and_index(bytes, 0)
                .context("invalid Android Japanese font")?
                .into());
        }
    }
    anyhow::bail!("no Japanese font found in /system/fonts")
}

#[cfg(target_arch = "wasm32")]
thread_local! { static BROWSER_FONT: std::cell::RefCell<Option<FontArc>> = const { std::cell::RefCell::new(None) }; }

#[cfg(target_arch = "wasm32")]
pub(crate) fn set_browser_font(bytes: Vec<u8>) -> Result<()> {
    ensure!(bytes.len() <= 32 * 1024 * 1024, "font exceeds 32 MiB");
    let font = FontVec::try_from_vec_and_index(bytes, 0)
        .context("invalid browser font")?
        .into();
    BROWSER_FONT.with(|value| *value.borrow_mut() = Some(font));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn load_font(_name: &str, _weight: u32) -> Result<FontArc> {
    BROWSER_FONT
        .with(|value| value.borrow().clone())
        .context("select a Japanese TTF/OTF/TTC font before starting")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn glyph_damage_includes_effects_clips_and_ignores_empty_masks() {
        let pixels = SharedMemory::zeroed(8 * 8 * 3).unwrap();
        let mask = Mask {
            offset: [1, -1],
            size: [1, 1],
            coverage: vec![64],
        };
        let style = TextStyle {
            effects: 1 | 0x400,
            shadow_offset: [4, 3],
            ..Default::default()
        };
        let target = || Canvas {
            pixels: &pixels,
            size: [8, 8],
            stride: 24,
            rgba: false,
        };
        // Foreground at (3, 1), a 2-pixel outline, shadow at (7, 4).
        assert_eq!(
            draw_mask(target(), [2, 2], &style, &mask, [2, 2]).unwrap(),
            Some([1, 0, 8, 5])
        );
        assert_eq!(
            draw_mask(target(), [i32::MAX, 2], &style, &mask, [2, 2]).unwrap(),
            None
        );
        let empty = Mask {
            coverage: vec![0],
            ..mask
        };
        assert_eq!(
            draw_mask(target(), [2, 2], &style, &empty, [2, 2]).unwrap(),
            None
        );
    }
    #[test]
    fn image_glyph_preserves_native_color_rounding_and_transparency() {
        let pixels = SharedMemory::zeroed(8).unwrap();
        let mask = Mask {
            offset: [0, 0],
            size: [2, 1],
            coverage: vec![32, 0],
        };
        let mut style = TextStyle {
            color: [220, 80, 30],
            effects: 0,
            opacity: 256,
            ..Default::default()
        };
        let target = || Canvas {
            pixels: &pixels,
            size: [2, 1],
            stride: 8,
            rgba: true,
        };
        draw_mask(target(), [0, 0], &style, &mask, [1, 1]).unwrap();
        assert_eq!(pixels.read(0, 8).unwrap(), [219, 79, 29, 127, 0, 0, 0, 0]);
        style.color = [0, 0, 255];
        draw_mask(target(), [0, 0], &style, &mask, [1, 1]).unwrap();
        let result = pixels.read(0, 8).unwrap();
        assert_eq!(result[3], 191);
        assert!(result[0] < 220 && result[2] > 30);
        assert_eq!(&result[4..], &[0; 4]);
    }

    #[test]
    fn image_glyph_blends_partial_full_and_zero_opacity() {
        let pixels = SharedMemory::zeroed(4).unwrap();
        let mask = Mask {
            offset: [0, 0],
            size: [1, 1],
            coverage: vec![64],
        };
        for (initial, opacity, expected) in [
            ([0, 0, 0, 255], 128, [108, 39, 14, 255]),
            ([30, 40, 50, 127], 256, [220, 80, 30, 255]),
            ([30, 40, 50, 127], 0, [30, 40, 50, 127]),
            // The native zero/zero entry is 255: RGB changes but stays invisible.
            ([30, 40, 50, 0], 0, [219, 79, 30, 0]),
        ] {
            pixels.write(0, &initial).unwrap();
            let style = TextStyle {
                color: [220, 80, 30],
                effects: 0,
                opacity,
                ..Default::default()
            };
            draw_mask(
                Canvas {
                    pixels: &pixels,
                    size: [1, 1],
                    stride: 4,
                    rgba: true,
                },
                [0, 0],
                &style,
                &mask,
                [1, 1],
            )
            .unwrap();
            assert_eq!(pixels.read(0, 4).unwrap(), expected);
        }
    }
}
