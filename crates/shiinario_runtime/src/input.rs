//! Control masks recovered from the original v2.47 input functions.
//! Device acquisition and event timing belong to the platform host.

/// Opcode 03e8 sign-extends GetAsyncKeyState's SHORT, then applies the
/// engine's activation flag. The query itself still occurs when inactive.
pub fn key_state_reply(raw: i16, active: bool) -> u32 {
    if active { raw as i32 as u32 } else { 0 }
}

/// Host-side asynchronous key flags. Reading consumes the recent-press bit,
/// including when opcode 03e8 subsequently masks the reply for inactivity.
pub struct AsyncKeys([u16; 256]);
impl Default for AsyncKeys {
    fn default() -> Self {
        Self([0; 256])
    }
}
impl AsyncKeys {
    pub fn is_down(&self, key: usize) -> bool {
        self.0.get(key).is_some_and(|bits| bits & 0x8000 != 0)
    }
    pub fn set(&mut self, key: usize, down: bool) {
        if let Some(bits) = self.0.get_mut(key) {
            if down {
                if *bits & 0x8000 == 0 {
                    *bits |= 0x8001;
                }
            } else {
                *bits &= 1;
            }
        }
    }
    pub fn query(&mut self, key: u32, active: bool) -> u32 {
        let raw = self.0.get_mut(key as usize).map_or(0, |bits| {
            let raw = *bits;
            *bits &= 0x8000;
            raw
        });
        key_state_reply(raw as i16, active)
    }
    pub fn clear(&mut self) {
        self.0.fill(0);
    }
}

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
    fn asynchronous_press_latches_survive_release_and_are_consumed_once() {
        let mut keys = AsyncKeys::default();
        keys.set(0x70, true);
        assert_eq!(keys.query(0x70, true), 0xffff8001);
        keys.set(0x70, true); // OS repeats must not create another press edge.
        assert_eq!(keys.query(0x70, true), 0xffff8000);
        keys.set(0x70, false);
        assert_eq!(keys.query(0x70, true), 0);
        keys.set(0x70, true);
        keys.set(0x70, false);
        assert_eq!(keys.query(0x70, true), 1);
        assert_eq!(keys.query(0x70, true), 0);
        keys.set(0x70, true);
        assert_eq!(keys.query(0x70, false), 0);
        assert_eq!(keys.query(0x70, true), 0xffff8000);
        keys.clear();
        assert!(!keys.is_down(0x70));
        assert_eq!(keys.query(u32::MAX, true), 0);
    }
}
