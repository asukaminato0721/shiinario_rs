//! Control masks recovered from the original v2.47 input functions.
//! Device acquisition and event timing belong to the platform host.

#[derive(Debug, Clone, Copy)]
pub struct JoystickState {
    pub x: u32,
    pub y: u32,
    pub buttons: u32,
}
impl JoystickState {
    pub fn mask(self) -> u32 {
        let mut mask = 0;
        if self.x < 0x2000 {
            mask |= 4;
        }
        if self.x > 0xdfff {
            mask |= 8;
        }
        if self.y < 0x4000 {
            mask |= 1;
        }
        if self.y > 0xdfff {
            mask |= 2;
        }
        if self.buttons & 1 != 0 {
            mask |= 0x20;
        }
        if self.buttons & 2 != 0 {
            mask |= 0x10;
        }
        mask
    }
}

pub struct ControlState {
    pub focused: bool,
    /// DirectInput scan-code indices; true means the physical key is down.
    pub keys: [bool; 256],
    /// Logical mouse buttons after the engine's left/right swap mapping.
    pub mouse_buttons: u8,
    pub joystick: Option<JoystickState>,
}
impl Default for ControlState {
    fn default() -> Self {
        Self {
            focused: true,
            keys: [false; 256],
            mouse_buttons: 0,
            joystick: None,
        }
    }
}
impl ControlState {
    pub fn mask(&self) -> u32 {
        let mut mask = self.joystick.map_or(0, JoystickState::mask);
        if !self.focused {
            return mask;
        }
        for (code, bits) in [
            (0x01, 0x40),
            (0x0f, 0x200),
            (0x1c, 0x20),
            (0x1d, 0x100),
            (0x2c, 0x20),
            (0x2d, 0x10),
            (0x39, 0x20),
            (0x48, 1),
            (0x4b, 4),
            (0x4d, 8),
            (0x50, 2),
            (0x52, 0x40),
            (0x9c, 0x20),
            (0x9d, 0x100),
            (0xc7, 0x40),
            (0xc8, 1),
            (0xcb, 4),
            (0xcd, 8),
            (0xcf, 0x80),
            (0xd0, 2),
        ] {
            if self.keys[code] {
                mask |= bits;
            }
        }
        for (button, bits) in [(1, 0x20), (2, 0x10), (4, 0x800)] {
            if self.mouse_buttons & button != 0 {
                mask |= bits;
            }
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiinario_scenario::{BinaryVm, Event, PlatformRequest};
    #[test]
    fn control_masks_match_original_keyboard_focus_mouse_and_joystick() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/input-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut controls = ControlState {
                focused: case["focused"].as_bool().unwrap(),
                mouse_buttons: case["buttons"].as_u64().unwrap() as u8,
                ..Default::default()
            };
            for code in case["keys"].as_array().unwrap() {
                controls.keys[code.as_u64().unwrap() as usize] = true;
            }
            if let Some(j) = case["joystick"].as_array() {
                controls.joystick = Some(JoystickState {
                    x: j[0].as_u64().unwrap() as u32,
                    y: j[1].as_u64().unwrap() as u32,
                    buttons: j[2].as_u64().unwrap() as u32,
                });
            }
            let mask = controls.mask();
            assert_eq!(serde_json::json!(mask), case["result"]);
            let mut vm =
                BinaryVm::new("input.scn", vec![0xe9, 3, 12, 0, 0, 0x9d, 4, 12, 0, 0]).unwrap();
            let event = vm.step().unwrap();
            assert!(matches!(
                event,
                Event::Platform {
                    request: PlatformRequest::ReadControls,
                    ..
                }
            ));
            assert_eq!(vm.step().unwrap(), event);
            vm.respond(mask).unwrap();
            assert_eq!(
                vm.location().offset,
                case["next_offset"].as_u64().unwrap() as usize
            );
            assert!(
                matches!(vm.step().unwrap(),Event::MouseButtonMapping {value,..} if value==mask)
            );
        }
    }
}
