//! Deterministic input snapshots for long, headless gameplay runs.
use crate::input::{AsyncKeys, ControlState};
use anyhow::{Result, ensure};
use serde::Deserialize;
use shiinario_scenario::MouseButtonMapping;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFrame {
    pub at_ms: u32,
    pub cursor: [i32; 2],
    #[serde(default)]
    pub mouse_buttons: u8,
    /// Windows virtual-key codes. Keyboard control masks are not synthesized.
    #[serde(default)]
    pub keys: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct Replay {
    frames: Vec<InputFrame>,
    next: usize,
    pub cursor: [i32; 2],
    mouse: u8,
    pub keys: AsyncKeys,
    mapping: MouseButtonMapping,
}
impl Replay {
    pub fn new(frames: Vec<InputFrame>) -> Result<Self> {
        ensure!(
            frames.windows(2).all(|w| w[0].at_ms < w[1].at_ms),
            "replay timestamps must be strictly increasing"
        );
        ensure!(
            frames.iter().all(|f| f.mouse_buttons & !7 == 0),
            "replay mouse buttons exceed mask 7"
        );
        Ok(Self {
            frames,
            ..Default::default()
        })
    }
    pub fn advance(&mut self, now: u32) {
        while let Some(frame) = self.frames.get(self.next).filter(|f| f.at_ms <= now) {
            self.cursor = frame.cursor;
            self.mouse = frame.mouse_buttons;
            for key in 0..256 {
                self.keys.set(key, frame.keys.contains(&(key as u8)));
            }
            for (button, key) in [(1, 1), (2, 2), (4, 4)] {
                self.keys.set(key, self.mouse & button != 0);
            }
            self.next += 1;
        }
    }
    pub fn mapping(&mut self, value: u32) {
        self.mapping.value = value;
    }
    pub fn controls(&self) -> u32 {
        ControlState {
            mouse_buttons: self.mapping.map_buttons(self.mouse),
            ..Default::default()
        }
        .mask()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_preserve_mouse_mapping_and_consumed_key_edges() {
        let mut replay = Replay::new(vec![
            InputFrame {
                at_ms: 10,
                cursor: [690, 65],
                mouse_buttons: 1,
                keys: vec![],
            },
            InputFrame {
                at_ms: 20,
                cursor: [400, 500],
                mouse_buttons: 0,
                keys: vec![],
            },
        ])
        .unwrap();
        replay.advance(9);
        assert_eq!(replay.controls(), 0);
        replay.advance(10);
        assert_eq!(replay.controls(), 0x20);
        assert_eq!(replay.cursor, [690, 65]);
        assert_eq!(replay.keys.query(1, true), 0xffff8001);
        assert_eq!(replay.keys.query(1, true), 0xffff8000);
        replay.mapping(1);
        assert_eq!(replay.controls(), 0x10);
        replay.advance(20);
        assert_eq!(replay.controls(), 0);
        assert_eq!(replay.keys.query(1, true), 0);
    }
}
