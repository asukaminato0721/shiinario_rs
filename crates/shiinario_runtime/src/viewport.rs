//! Logical/window coordinate conversion with the original f32 scale factors.
use anyhow::{Result, ensure};
use shiinario_scenario::PlatformRequest;
#[derive(Clone, Copy)]
pub struct ViewportTransform {
    scale: [f32; 2],
    offset: [i32; 2],
}
impl Default for ViewportTransform {
    fn default() -> Self {
        Self {
            scale: [1.0; 2],
            offset: [0; 2],
        }
    }
}
impl ViewportTransform {
    /// Native SCN client coordinates belong to the logical canvas. Window
    /// resizing is presentation-only, regardless of the original Windows INI
    /// flags. Scripts may explicitly map a queried client point with 0492;
    /// that point is already logical and must not be scaled a second time.
    pub fn cursor_reply(self, request: &PlatformRequest, physical: [i32; 2]) -> Result<[i32; 2]> {
        match request {
            PlatformRequest::CursorPosition => self.to_logical(physical),
            PlatformRequest::MapCursor { point } => Ok(*point),
            _ => anyhow::bail!("unsupported cursor request: {request:?}"),
        }
    }
    /// Center a logical canvas inside a nonempty physical window.
    pub fn fit(logical: [u32; 2], physical: [u32; 2]) -> Result<Self> {
        ensure!(
            logical
                .into_iter()
                .chain(physical)
                .all(|n| n > 0 && n <= i32::MAX as u32),
            "invalid viewport dimensions"
        );
        let scale =
            (physical[0] as f32 / logical[0] as f32).min(physical[1] as f32 / logical[1] as f32);
        let offset = std::array::from_fn(|i| {
            ((physical[i] as f64 - f64::from(scale) * f64::from(logical[i])) / 2.0).max(0.0) as i32
        });
        Self::new([scale; 2], offset)
    }
    pub fn new(scale: [f32; 2], offset: [i32; 2]) -> Result<Self> {
        ensure!(
            scale.iter().all(|v| v.is_finite() && *v > 0.0),
            "invalid viewport scale"
        );
        Ok(Self { scale, offset })
    }
    pub fn to_logical(self, point: [i32; 2]) -> Result<[i32; 2]> {
        let mut result = [0; 2];
        for axis in 0..2 {
            let value = f64::from(point[axis].wrapping_sub(self.offset[axis]))
                / f64::from(self.scale[axis]);
            result[axis] = integer(value)?;
        }
        Ok(result)
    }
    pub fn to_window_rect(self, rect: [i32; 4]) -> Result<[i32; 4]> {
        let mut result = [0; 4];
        for i in 0..4 {
            result[i] = integer(f64::from(rect[i]) * f64::from(self.scale[i % 2]))?
                .wrapping_add(self.offset[i % 2]);
        }
        Ok(result)
    }
}
fn integer(value: f64) -> Result<i32> {
    let value = value.trunc();
    ensure!(
        value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX),
        "viewport coordinate overflow"
    );
    Ok(value as i32)
}
#[cfg(test)]
mod tests {
    use super::*;
    use shiinario_scenario::{BinaryVm, Event, PlatformRequest};
    #[test]
    fn fitted_canvas_and_input_share_letterbox_coordinates() {
        for (physical, rect, center) in [
            ([800, 600], [0, 0, 800, 600], [400, 300]),
            ([1000, 600], [100, 0, 900, 600], [500, 300]),
            ([800, 800], [0, 100, 800, 700], [400, 400]),
            ([1600, 1200], [0, 0, 1600, 1200], [800, 600]),
            ([400, 300], [0, 0, 400, 300], [200, 150]),
        ] {
            let transform = ViewportTransform::fit([800, 600], physical).unwrap();
            assert_eq!(transform.to_window_rect([0, 0, 800, 600]).unwrap(), rect);
            assert_eq!(transform.to_logical(center).unwrap(), [400, 300]);
            assert_eq!(
                transform.to_logical([rect[0] - 2, rect[1] - 2]).unwrap(),
                [-1600 / (rect[2] - rect[0]), -1200 / (rect[3] - rect[1])]
            );
        }
        assert!(ViewportTransform::fit([0, 600], [800, 600]).is_err());
        assert!(ViewportTransform::fit([800, 600], [800, 0]).is_err());
    }
}
