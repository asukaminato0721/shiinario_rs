//! Poll device zero without controller mappings, dead zones or input messages.
use crate::input::JoystickState;

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    #[repr(C)]
    struct JoyInfoEx {
        size: u32,
        flags: u32,
        position_and_buttons: [u32; 11],
    }
    const _: () = assert!(std::mem::size_of::<JoyInfoEx>() == 52);

    #[link(name = "winmm")]
    unsafe extern "system" {
        #[link_name = "joyGetPosEx"]
        fn joy_get_pos_ex(id: u32, state: *mut JoyInfoEx) -> u32;
    }

    pub(crate) struct Joystick;
    impl Joystick {
        pub fn new() -> Self {
            Self
        }

        pub fn query(&mut self) -> Option<JoystickState> {
            let mut state = JoyInfoEx {
                size: 52,
                flags: 0x83, // JOY_RETURNX | JOY_RETURNY | JOY_RETURNBUTTONS.
                position_and_buttons: [0; 11],
            };
            // SAFETY: WinMM receives a writable, correctly sized JOYINFOEX.
            (unsafe { joy_get_pos_ex(0, &mut state) } == 0).then_some(JoystickState {
                x: state.position_and_buttons[0],
                y: state.position_and_buttons[1],
                buttons: state.position_and_buttons[6],
            })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn axis_to_winmm(value: i32, min: i32, max: i32) -> u32 {
    let span = i64::from(max) - i64::from(min);
    if span > 0 {
        ((i64::from(value.clamp(min, max)) - i64::from(min)) * 65535 / span) as u32
    } else {
        0x8000
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::{
        fs::{File, OpenOptions},
        io::{ErrorKind, Read},
        os::unix::fs::OpenOptionsExt,
    };

    struct Device {
        file: File,
        axes: [Option<u8>; 2],
        state: JoystickState,
    }
    impl Device {
        fn open() -> Option<Self> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open("/dev/input/js0")
                .ok()?;
            use std::os::fd::AsRawFd;
            let mut count = 0u8;
            let mut map = [0u8; 64];
            // SAFETY: Both buffers match the joystick ioctl ABI and remain writable.
            let axes_ok = unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    libc::_IOR::<u8>(u32::from(b'j'), 0x11),
                    &mut count,
                )
            };
            let map_ok = unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    libc::_IOR::<[u8; 64]>(u32::from(b'j'), 0x32),
                    &mut map,
                )
            };
            if axes_ok < 0 || map_ok < 0 {
                return None;
            }
            let axes = [0u8, 1].map(|code| {
                map[..usize::from(count).min(map.len())]
                    .iter()
                    .position(|axis| *axis == code)
                    .map(|index| index as u8)
            });
            Some(Self {
                file,
                axes,
                state: JoystickState {
                    x: 0x8000,
                    y: 0x8000,
                    buttons: 0,
                },
            })
        }

        fn query(&mut self) -> Option<JoystickState> {
            let mut pending = true;
            while pending {
                let mut event = [0u8; 8];
                match self.file.read(&mut event) {
                    Ok(8) => {
                        let value = i16::from_ne_bytes([event[4], event[5]]);
                        let number = event[7];
                        // Initial-state events use the same values as later events.
                        match event[6] & !0x80 {
                            1 => {
                                if let Some(bit) = 1u32.checked_shl(u32::from(number)) {
                                    if value != 0 {
                                        self.state.buttons |= bit;
                                    } else {
                                        self.state.buttons &= !bit;
                                    }
                                }
                            }
                            2 => {
                                let value = axis_to_winmm(i32::from(value), -32767, 32767);
                                if self.axes[0] == Some(number) {
                                    self.state.x = value;
                                }
                                if self.axes[1] == Some(number) {
                                    self.state.y = value;
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => pending = false,
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    _ => return None,
                }
            }
            Some(self.state)
        }
    }

    pub(crate) struct Joystick(Option<Device>);
    impl Joystick {
        pub fn new() -> Self {
            Self(None)
        }
        pub fn query(&mut self) -> Option<JoystickState> {
            if self.0.is_none() {
                self.0 = Device::open();
            }
            let reply = self.0.as_mut().and_then(Device::query);
            if reply.is_none() {
                self.0 = None;
            }
            reply
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            io::Write,
            os::{fd::OwnedFd, unix::net::UnixStream},
        };

        #[test]
        fn initial_state_release_hat_isolation_and_disconnect_match_polling() {
            let (reader, mut writer) = UnixStream::pair().unwrap();
            reader.set_nonblocking(true).unwrap();
            let mut device = Device {
                file: File::from(OwnedFd::from(reader)),
                axes: [Some(0), Some(1)],
                state: JoystickState {
                    x: 0x8000,
                    y: 0x8000,
                    buttons: 0,
                },
            };
            let mut send = |value: i16, kind: u8, number: u8| {
                let mut bytes = [0; 8];
                bytes[4..6].copy_from_slice(&value.to_ne_bytes());
                bytes[6] = kind;
                bytes[7] = number;
                writer.write_all(&bytes).unwrap();
            };
            for (value, kind, number) in [
                (-32767, 0x82, 0),
                (32767, 0x82, 1),
                (1, 0x81, 0),
                (1, 0x81, 31),
            ] {
                send(value, kind, number);
            }
            assert_eq!(
                device.query(),
                Some(JoystickState {
                    x: 0,
                    y: 65535,
                    buttons: 0x8000_0001
                })
            );
            send(0, 1, 0);
            send(-32767, 2, 6); // A POV axis cannot replace X/Y.
            assert_eq!(
                device.query(),
                Some(JoystickState {
                    x: 0,
                    y: 65535,
                    buttons: 0x8000_0000
                })
            );
            drop(writer);
            assert_eq!(device.query(), None);
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use gilrs_core::{
        EventType, Gilrs,
        native_ev_codes::{AXIS_LSTICKX, AXIS_LSTICKY},
    };

    pub(crate) struct Joystick {
        backend: Option<Gilrs>,
        state: Option<JoystickState>,
    }
    impl Joystick {
        pub fn new() -> Self {
            let backend = match Gilrs::new() {
                Ok(backend) => Some(backend),
                Err(error) => {
                    eprintln!("Joystick input unavailable: {error}");
                    None
                }
            };
            Self {
                backend,
                state: None,
            }
        }

        pub fn query(&mut self) -> Option<JoystickState> {
            let backend = self.backend.as_mut()?;
            if self.state.is_none() && backend.gamepad(0).is_some_and(|pad| pad.is_connected()) {
                self.state = Some(Self::centered());
            }
            while let Some(event) = backend.next_event() {
                if event.id == 0 {
                    match event.event {
                        EventType::Connected => self.state = Some(Self::centered()),
                        EventType::Disconnected => self.state = None,
                        EventType::AxisValueChanged(value, code) => {
                            if let Some(state) = &mut self.state
                                && let Some(info) =
                                    backend.gamepad(0).and_then(|pad| pad.axis_info(code))
                            {
                                let value = axis_to_winmm(value, info.min, info.max);
                                if code == AXIS_LSTICKX {
                                    state.x = value;
                                }
                                if code == AXIS_LSTICKY {
                                    state.y = value;
                                }
                            }
                        }
                        EventType::ButtonPressed(code) | EventType::ButtonReleased(code) => {
                            if let Some(state) = &mut self.state
                                && let Some(index) = backend.gamepad(0).and_then(|pad| {
                                    pad.buttons().iter().position(|button| *button == code)
                                })
                                && let Some(bit) = 1u32.checked_shl(index as u32)
                            {
                                if matches!(event.event, EventType::ButtonPressed(_)) {
                                    state.buttons |= bit;
                                } else {
                                    state.buttons &= !bit;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            self.state
                .filter(|_| backend.gamepad(0).is_some_and(|pad| pad.is_connected()))
        }

        fn centered() -> JoystickState {
            JoystickState {
                x: 0x8000,
                y: 0x8000,
                buttons: 0,
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod platform {
    use super::*;
    pub(crate) struct Joystick;
    impl Joystick {
        pub fn new() -> Self {
            Self
        }
        pub fn query(&mut self) -> Option<JoystickState> {
            None
        }
    }
}

pub(crate) use platform::Joystick;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_axis_ranges_preserve_endpoints_without_a_dead_zone() {
        for (value, min, max, expected) in [
            (-32768, -32768, 32767, 0),
            (0, -32768, 32767, 32768),
            (32767, -32768, 32767, 65535),
            (8191, 0, 65535, 8191),
            (8192, 0, 65535, 8192),
            (16383, 0, 65535, 16383),
            (57343, 0, 65535, 57343),
            (57344, 0, 65535, 57344),
            (0, 1, 1, 32768),
            (i32::MAX, i32::MIN, i32::MAX, 65535),
        ] {
            assert_eq!(axis_to_winmm(value, min, max), expected);
        }
    }
}
