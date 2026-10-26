//! Control masks recovered from the original v2.47 input functions.
//! Device acquisition and event timing belong to the platform host.

/// Opcode 03e8 sign-extends GetAsyncKeyState's SHORT, then applies the
/// engine's activation flag. The query itself still occurs when inactive.
pub fn key_state_reply(raw: i16, active: bool) -> u32 {
    if active { raw as i32 as u32 } else { 0 }
}

/// Alt modifies ordinary key messages to WM_SYSKEYDOWN/WM_SYSKEYUP.
pub fn keyboard_message(key: u32, pressed: bool, alt_down: bool) -> [u32; 3] {
    let system = alt_down || matches!(key, 0x12 | 0xa4 | 0xa5);
    [
        if system { 0x104 } else { 0x100 } + u32::from(!pressed),
        key,
        0,
    ]
}

/// Original 414190, repeat slot 256: emit a new combination immediately.
/// A held combination arms a repeat after 300 ms, then every 50 ms.
#[derive(Default)]
pub(crate) struct ControlRepeat {
    mask: u32,
    phase: u8,
    epoch: u32,
    delay: u32,
}
impl ControlRepeat {
    pub fn query(&mut self, mask: u32, now: u32) -> u32 {
        if mask == 0 {
            self.mask = 0;
            self.phase = 0;
            return 0;
        }
        let previous = std::mem::replace(&mut self.mask, mask);
        if previous & mask != mask || self.phase == 0 {
            self.epoch = now;
            self.delay = 300;
            self.phase = 2;
            return mask;
        }
        if self.phase == 1 {
            self.epoch = now;
            self.delay = 50;
            self.phase = 2;
            return mask;
        }
        // The native helper emits on the query after the deadline check.
        if now.wrapping_sub(self.epoch) >= self.delay {
            self.phase = 1;
        }
        0
    }
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
    #[test]
    fn control_repeat_waits_rearms_and_resets_across_clock_wrap() {
        for epoch in [0u32, u32::MAX - 100] {
            let mut repeat = ControlRepeat::default();
            assert_eq!(repeat.query(0x20, epoch), 0x20);
            for elapsed in [0, 1, 299, 300] {
                assert_eq!(repeat.query(0x20, epoch.wrapping_add(elapsed)), 0);
            }
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(300)), 0x20);
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(349)), 0);
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(350)), 0);
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(350)), 0x20);
            assert_eq!(repeat.query(0x21, epoch.wrapping_add(351)), 0x21);
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(352)), 0);
            assert_eq!(repeat.query(0, epoch.wrapping_add(353)), 0);
            assert_eq!(repeat.query(0x20, epoch.wrapping_add(354)), 0x20);
        }
    }
    #[test]
    fn alt_key_events_use_system_messages() {
        assert_eq!(keyboard_message(0xd, true, true), [0x104, 0xd, 0]);
        assert_eq!(keyboard_message(0xd, false, true), [0x105, 0xd, 0]);
        assert_eq!(keyboard_message(0xd, true, false), [0x100, 0xd, 0]);
        assert_eq!(keyboard_message(0x12, false, false), [0x105, 0x12, 0]);
    }
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
