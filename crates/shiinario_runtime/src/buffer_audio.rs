//! Static DirectSound-style buffers, separate from the engine's OGV streams.
use crate::resources::Sound;
use anyhow::{Result, ensure};
use shiinario_scenario::SoundCommand;
use std::sync::Arc;

pub(crate) struct BufferVoice {
    sound: Arc<Sound>,
    frame: u64,
    fraction: u64,
    playing: bool,
    looping: bool,
    frequency: u32,
    volume: i32,
    pan: i32,
    gain: [f32; 2],
}
impl BufferVoice {
    pub fn new(sound: Arc<Sound>) -> Self {
        Self {
            frequency: sound.sample_rate,
            sound,
            frame: 0,
            fraction: 0,
            playing: false,
            looping: false,
            volume: 0,
            pan: 0,
            gain: [1.0; 2],
        }
    }
    fn frames(&self) -> u64 {
        self.sound.samples.len() as u64 / u64::from(self.sound.channels)
    }
    pub fn status(&self) -> u32 {
        if self.playing && self.frames() > 0 && (self.looping || self.frame < self.frames()) {
            1 | if self.looping { 4 } else { 0 }
        } else {
            0
        }
    }
    fn gains(&mut self) {
        let attenuation = |db: i32| {
            if db <= -10000 {
                0.0
            } else {
                10.0f32.powf(db as f32 / 2000.0)
            }
        };
        let volume = attenuation(self.volume);
        self.gain = [
            volume * attenuation(-self.pan.max(0)),
            volume * attenuation(self.pan.min(0)),
        ];
    }
    pub fn command(&mut self, command: &SoundCommand) -> Result<u32> {
        match *command {
            SoundCommand::Status => return Ok(self.status()),
            SoundCommand::Play { flags } => {
                ensure!(flags <= 1, "unsupported sound playback flags {flags:#x}");
                self.frame = 0;
                self.fraction = 0;
                self.playing = true;
                self.looping = flags == 1;
            }
            SoundCommand::Stop => {
                self.playing = false;
                self.frame = 0;
                self.fraction = 0;
            }
            SoundCommand::Volume { attenuation } => {
                ensure!(
                    (-10000..=0).contains(&attenuation),
                    "sound volume outside -10000..0"
                );
                self.volume = attenuation;
                self.gains();
            }
            SoundCommand::Pan { attenuation } => {
                ensure!(
                    (-10000..=10000).contains(&attenuation),
                    "sound pan outside -10000..10000"
                );
                self.pan = attenuation;
                self.gains();
            }
            SoundCommand::Frequency { hz } => {
                ensure!(
                    hz == 0 || (100..=200000).contains(&hz),
                    "sound frequency outside 100..200000"
                );
                self.frequency = if hz == 0 { self.sound.sample_rate } else { hz };
            }
        }
        Ok(1)
    }
    pub fn advance(&mut self, frames: u64, rate: u32) {
        if self.status() == 0 {
            return;
        }
        let total = u128::from(self.fraction) + u128::from(frames) * u128::from(self.frequency);
        let position = u128::from(self.frame) + total / u128::from(rate);
        self.fraction = (total % u128::from(rate)) as u64;
        self.frame = if self.looping {
            (position % u128::from(self.frames())) as u64
        } else {
            position.min(u128::from(self.frames())) as u64
        };
    }
    fn at(&self, frame: u64) -> [f32; 2] {
        let index = if self.looping {
            frame % self.frames()
        } else {
            frame.min(self.frames() - 1)
        } as usize;
        let channels = usize::from(self.sound.channels);
        let left = f32::from(self.sound.samples[index * channels]) / 32768.0;
        let right = if channels == 2 {
            f32::from(self.sound.samples[index * channels + 1]) / 32768.0
        } else {
            left
        };
        [left, right]
    }
    pub fn next(&mut self, rate: u32) -> Option<[f32; 2]> {
        if self.status() == 0 {
            return None;
        }
        let a = self.at(self.frame);
        let b = self.at(self.frame + 1);
        let fraction = self.fraction as f32 / rate as f32;
        let sample = std::array::from_fn(|i| (a[i] + (b[i] - a[i]) * fraction) * self.gain[i]);
        self.advance(1, rate);
        Some(sample)
    }
}
