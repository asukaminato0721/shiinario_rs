//! Logical/window coordinate conversion with the original f32 scale factors.
use anyhow::{Result, ensure};
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
    fn cursor_queries_and_transforms_match_original_signed_coordinates() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/cursor-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let scale = std::array::from_fn(|i| case["scale"][i].as_f64().unwrap() as f32);
            let offset = std::array::from_fn(|i| case["offset"][i].as_i64().unwrap() as i32);
            let transform = ViewportTransform::new(scale, offset).unwrap();
            let client = std::array::from_fn(|i| case["client"][i].as_i64().unwrap() as i32);
            let hex = case["code"].as_str().unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("cursor.scn", bytes).unwrap();
            for state in case["states"].as_array().unwrap() {
                let event = vm.step().unwrap();
                let point = match event {
                    Event::Platform {
                        request: PlatformRequest::CursorPosition,
                        ..
                    } => {
                        if case["adjust"].as_bool().unwrap() {
                            transform.to_logical(client).unwrap()
                        } else {
                            client
                        }
                    }
                    Event::Platform {
                        request: PlatformRequest::MapCursor { point },
                        ..
                    } => transform.to_logical(point).unwrap(),
                    _ => panic!("expected point request"),
                };
                assert_eq!(serde_json::json!(point), state["point"]);
                assert!(vm.respond(1).is_err());
                vm.respond_point(point).unwrap();
                assert!(vm.respond_point(point).is_err());
                assert_eq!(
                    vm.location().offset,
                    state["next_offset"].as_u64().unwrap() as usize
                );
            }
        }
    }
    #[test]
    fn repaint_transform_matches_original_and_rejects_invalid_scales() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/redraw-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let scale = std::array::from_fn(|i| case["scale"][i].as_f64().unwrap() as f32);
            let offset = std::array::from_fn(|i| case["offset"][i].as_i64().unwrap() as i32);
            let rect = std::array::from_fn(|i| case["rect"][i].as_i64().unwrap() as i32);
            assert_eq!(
                serde_json::json!(
                    ViewportTransform::new(scale, offset)
                        .unwrap()
                        .to_window_rect(rect)
                        .unwrap()
                ),
                case["window_rect"]
            );
        }
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(ViewportTransform::new([bad, 1.0], [0; 2]).is_err());
        }
        assert!(
            ViewportTransform::new([0.5; 2], [0; 2])
                .unwrap()
                .to_logical([i32::MAX; 2])
                .is_err()
        );
    }
}
