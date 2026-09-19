//! Stream playback, independent of SCN and windowing. Device output uses CPAL.
use crate::{
    buffer_audio::BufferVoice,
    resources::{AudioStream, Sound, StreamVolume},
};
use anyhow::{Context, Result, bail, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use shiinario_scenario::SoundCommand;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

struct Voice {
    stream: Arc<AudioStream>,
    frame: u64,
    fraction: u64,
    looping: bool,
    volume: u32,
    gain: f32,
    fade: Option<Fade>,
}
struct Fade {
    period_ms: u64,
    elapsed: u64,
    step: i32,
    target: u32,
    stop: bool,
}
impl Voice {
    /// Advance on the output clock, independently of SCN scheduling. Returns
    /// true when the fade has stopped this voice.
    fn advance_fade(&mut self, frames: u64, rate: u32) -> bool {
        let Some(fade) = &mut self.fade else {
            return false;
        };
        fade.elapsed += frames * 1000;
        let period = fade.period_ms * u64::from(rate);
        while fade.elapsed >= period {
            fade.elapsed -= period;
            let next = i64::from(self.stream.volume.percent() as i32) + i64::from(fade.step);
            let done = if fade.step > 0 {
                next >= i64::from(fade.target)
            } else {
                next <= i64::from(fade.target)
            };
            if done {
                let stop = fade.step < 0 && fade.stop;
                self.stream
                    .volume
                    .set_fade_percent(if stop { next as u32 } else { fade.target });
                self.fade = None;
                return stop;
            }
            self.stream.volume.set_fade_percent(next as u32);
        }
        false
    }
    fn frame_at(&self, frame: u64) -> Option<[f32; 2]> {
        let sound = &self.stream.sound;
        let channels = usize::from(sound.channels);
        let count = sound.samples.len() / channels;
        if count == 0 {
            return None;
        }
        let index = if frame < count as u64 {
            frame as usize
        } else if self.looping {
            // Original 454790 leaves the decoded-byte counter at EOF. After
            // seeking to zero, it emits the first block and seeks again. Keep
            // that extra prefix: removing it changes the original loop audio.
            let prefix = self.stream.first_block_frames;
            let index = ((frame - count as u64) % (count + prefix) as u64) as usize;
            if index < prefix {
                index
            } else {
                index - prefix
            }
        } else {
            return None;
        };
        let left = f32::from(sound.samples[index * channels]) / 32768.0;
        let right = if channels == 2 {
            f32::from(sound.samples[index * channels + 1]) / 32768.0
        } else {
            left
        };
        Some([left, right])
    }
    fn next(&mut self, rate: u32) -> Option<[f32; 2]> {
        let a = self.frame_at(self.frame)?;
        let b = self.frame_at(self.frame + 1).unwrap_or(a);
        let fraction = self.fraction as f32 / rate as f32;
        let volume = (self.stream.volume.percent() as i32).clamp(0, 100) as u32;
        if volume != self.volume {
            self.volume = volume;
            // DirectSound attenuation is in hundredths of a decibel.
            self.gain = if volume == 0 {
                0.0
            } else {
                10.0f32.powf(
                    StreamVolume::attenuation(volume).expect("validated stream volume") as f32
                        / 2000.0,
                )
            };
        }
        let sample = [
            (a[0] + (b[0] - a[0]) * fraction) * self.gain,
            (a[1] + (b[1] - a[1]) * fraction) * self.gain,
        ];
        self.fraction += u64::from(self.stream.sound.sample_rate);
        self.frame += self.fraction / u64::from(rate);
        self.fraction %= u64::from(rate);
        Some(sample)
    }
}

/// Deterministic mixer. Headless hosts advance it only by requesting frames.
#[derive(Default)]
pub struct Mixer {
    voices: BTreeMap<u32, Voice>,
    buffers: BTreeMap<u32, BufferVoice>,
}
impl Mixer {
    pub fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()> {
        ensure!(
            id < 256
                && matches!(sound.channels, 1 | 2)
                && sound.sample_rate > 0
                && sound
                    .samples
                    .len()
                    .is_multiple_of(usize::from(sound.channels)),
            "invalid sound buffer"
        );
        self.buffers.insert(id, BufferVoice::new(sound));
        Ok(())
    }
    pub fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32> {
        ensure!(id < 256, "sound slot out of bounds");
        match self.buffers.get_mut(&id) {
            Some(buffer) => buffer.command(command),
            None => Ok(if matches!(command, SoundCommand::Status) {
                u32::MAX
            } else {
                1
            }),
        }
    }
    /// Advance a synthetic host using a fixed 48 kHz output clock.
    /// Do not mix this with rendering at a different output rate.
    pub fn advance_ms(&mut self, ms: u32) {
        let frames = u64::from(ms) * 48;
        for buffer in self.buffers.values_mut() {
            buffer.advance(frames, 48000);
        }
        self.voices.retain(|_, voice| {
            if voice.frame_at(voice.frame).is_none() {
                return false;
            }
            let frames = if voice.looping {
                frames
            } else {
                let count =
                    voice.stream.sound.samples.len() / usize::from(voice.stream.sound.channels);
                let remaining = ((count as u64 - voice.frame) * 48000 - voice.fraction)
                    .div_ceil(u64::from(voice.stream.sound.sample_rate));
                frames.min(remaining)
            };
            if voice.advance_fade(frames, 48000) {
                return false;
            }
            let total = u128::from(voice.fraction)
                + u128::from(frames) * u128::from(voice.stream.sound.sample_rate);
            voice.frame =
                (u128::from(voice.frame) + total / 48000).min(u128::from(u64::MAX)) as u64;
            voice.fraction = (total % 48000) as u64;
            voice.frame_at(voice.frame).is_some()
        });
    }
    /// Original 2.36/2.47 workers sleep 5 ms and compare the counter before
    /// incrementing it: interval 30 means one volume step every 35 ms.
    pub fn fade(&mut self, handle: u32, interval: u32, step: i32, target: u32) -> Result<()> {
        ensure!(target & 0x7fffffff <= 100, "audio fade target exceeds 100");
        if let Some(voice) = self.voices.get_mut(&handle) {
            // Replacing a fade cancels the old worker even at the endpoint.
            voice.fade = None;
            if voice.frame_at(voice.frame).is_some()
                && voice.stream.volume.percent() != target & 0x7fffffff
            {
                voice.fade = Some(Fade {
                    period_ms: (u64::from(interval / 5).max(1) + 1) * 5,
                    elapsed: 0,
                    step: if step == 0 { -1 } else { step },
                    target: target & 0x7fffffff,
                    stop: target & 0x80000000 == 0,
                });
            }
        }
        Ok(())
    }
    pub fn play(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()> {
        ensure!(
            matches!(flags, 0 | 2),
            "unresolved audio playback flags {flags:#x}"
        );
        let sound = &stream.sound;
        ensure!(
            matches!(sound.channels, 1 | 2) && sound.sample_rate > 0,
            "invalid stream format"
        );
        ensure!(
            sound
                .samples
                .len()
                .is_multiple_of(usize::from(sound.channels)),
            "incomplete audio frame"
        );
        let frames = sound.samples.len() / usize::from(sound.channels);
        ensure!(
            stream.loop_start_frame == 0 && stream.first_block_frames <= frames,
            "unresolved stream loop range"
        );
        ensure!(
            frames == 0 || stream.first_block_frames > 0,
            "missing stream block size"
        );
        ensure!(
            self.voices.contains_key(&handle) || self.voices.len() < 256,
            "audio voice limit exceeded"
        );
        self.voices.insert(
            handle,
            Voice {
                stream,
                frame: 0,
                fraction: 0,
                looping: flags & 2 != 0,
                volume: u32::MAX,
                gain: 1.0,
                fade: None,
            },
        );
        Ok(())
    }
    pub fn stop(&mut self, handle: u32) {
        self.voices.remove(&handle);
    }
    pub fn is_playing(&self, handle: u32) -> bool {
        self.voices
            .get(&handle)
            .is_some_and(|voice| voice.frame_at(voice.frame).is_some())
    }
    fn next_frame(&mut self, rate: u32) -> [f32; 2] {
        let mut sum = [0.0; 2];
        self.voices.retain(|_, voice| {
            if let Some(frame) = voice.next(rate) {
                sum[0] += frame[0];
                sum[1] += frame[1];
                !voice.advance_fade(1, rate)
            } else {
                false
            }
        });
        for buffer in self.buffers.values_mut() {
            if let Some(frame) = buffer.next(rate) {
                sum[0] += frame[0];
                sum[1] += frame[1];
            }
        }
        [sum[0].clamp(-1.0, 1.0), sum[1].clamp(-1.0, 1.0)]
    }
    pub fn render(&mut self, output: &mut [f32], rate: u32, channels: u16) -> Result<()> {
        ensure!(
            rate > 0 && matches!(channels, 1 | 2),
            "unsupported output format"
        );
        ensure!(
            output.len().is_multiple_of(usize::from(channels)),
            "incomplete output frame"
        );
        for frame in output.chunks_exact_mut(usize::from(channels)) {
            let sample = self.next_frame(rate);
            if channels == 1 {
                frame[0] = (sample[0] + sample[1]) * 0.5;
            } else {
                frame.copy_from_slice(&sample);
            }
        }
        Ok(())
    }
}

/// Owns the device stream; dropping it stops playback. Async errors remain
/// observable by the host instead of being printed and forgotten.
pub struct AudioOutput {
    _stream: cpal::Stream,
    mixer: Arc<Mutex<Mixer>>,
    error: Arc<Mutex<Option<String>>>,
    frames: Arc<AtomicU64>,
}
impl AudioOutput {
    pub fn fade(&self, handle: u32, interval: u32, step: i32, target: u32) -> Result<()> {
        self.check()?;
        self.mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .fade(handle, interval, step, target)
    }
    pub fn install_sound(&self, id: u32, sound: Arc<Sound>) -> Result<()> {
        self.check()?;
        self.mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .install_sound(id, sound)
    }
    pub fn sound_command(&self, id: u32, command: &SoundCommand) -> Result<u32> {
        self.check()?;
        self.mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .sound_command(id, command)
    }
    pub fn open() -> Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device")?;
        let config = device
            .default_output_config()
            .context("audio output configuration")?;
        ensure!(
            matches!(config.channels(), 1 | 2),
            "audio device requires mono or stereo output"
        );
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let error = Arc::new(Mutex::new(None));
        let frames = Arc::new(AtomicU64::new(0));
        let build = |format| -> Result<cpal::Stream> {
            match format {
                cpal::SampleFormat::F32 => build::<f32>(
                    &device,
                    &config.clone().into(),
                    mixer.clone(),
                    error.clone(),
                    frames.clone(),
                ),
                cpal::SampleFormat::I16 => build::<i16>(
                    &device,
                    &config.clone().into(),
                    mixer.clone(),
                    error.clone(),
                    frames.clone(),
                ),
                cpal::SampleFormat::U16 => build::<u16>(
                    &device,
                    &config.clone().into(),
                    mixer.clone(),
                    error.clone(),
                    frames.clone(),
                ),
                _ => bail!("unsupported audio device sample format {format}"),
            }
        };
        let stream = build(config.sample_format())?;
        stream.play().context("start audio output")?;
        Ok(Self {
            _stream: stream,
            mixer,
            error,
            frames,
        })
    }
    pub fn check(&self) -> Result<()> {
        let error = self
            .error
            .lock()
            .map_err(|_| anyhow::anyhow!("audio error lock poisoned"))?;
        if let Some(error) = error.as_ref() {
            bail!("audio output: {error}");
        }
        Ok(())
    }
    pub fn stop(&self, handle: u32) -> Result<()> {
        self.check()?;
        self.mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .stop(handle);
        Ok(())
    }
    pub fn play(&self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()> {
        self.check()?;
        self.mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .play(handle, stream, flags)
    }
    pub fn rendered_frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    pub fn is_playing(&self, handle: u32) -> Result<bool> {
        self.check()?;
        Ok(self
            .mixer
            .lock()
            .map_err(|_| anyhow::anyhow!("audio mixer lock poisoned"))?
            .is_playing(handle))
    }
}
fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mixer: Arc<Mutex<Mixer>>,
    error: Arc<Mutex<Option<String>>>,
    frames: Arc<AtomicU64>,
) -> Result<cpal::Stream> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate.0;
    let callback_error = error.clone();
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let Ok(mut mixer) = mixer.lock() else {
                data.fill(T::from_sample(0.0));
                if let Ok(mut error) = callback_error.lock() {
                    *error = Some("mixer lock poisoned".into());
                }
                return;
            };
            for frame in data.chunks_exact_mut(channels) {
                let sample = mixer.next_frame(rate);
                if channels == 1 {
                    frame[0] = T::from_sample((sample[0] + sample[1]) * 0.5);
                } else {
                    frame[0] = T::from_sample(sample[0]);
                    frame[1] = T::from_sample(sample[1]);
                }
            }
            frames.fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
        },
        move |failure| {
            if let Ok(mut error) = error.lock() {
                *error = Some(failure.to_string());
            }
        },
        None,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{Resources, Sound};
    use std::io::Read;
    fn synthetic() -> Arc<AudioStream> {
        let ogg = include_bytes!("../../shiinario_assets/tests/fixtures/sine.ogg");
        let mut data = b"OGV\0".to_vec();
        data.extend(836u32.to_le_bytes());
        data.extend((ogg.len() as u32).to_le_bytes());
        data.extend(b"fmt ");
        data.extend(16u32.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(8000u32.to_le_bytes());
        data.extend(16000u32.to_le_bytes());
        data.extend(2u16.to_le_bytes());
        data.extend(16u16.to_le_bytes());
        data.extend(b"data");
        data.extend(800u32.to_le_bytes());
        data.extend(ogg);
        data.extend([0; 8]);
        let mut resources = Resources::default();
        let handle = resources.create_audio_stream(&data, 0x21).unwrap();
        resources.audio_stream(handle).unwrap().clone()
    }
    #[test]
    fn static_buffers_restart_loop_stop_replace_and_mix_with_streams() {
        let sound = Arc::new(Sound {
            sample_rate: 1000,
            channels: 1,
            samples: vec![8192, 16384, -8192],
        });
        let mut mixer = Mixer::default();
        assert_eq!(
            mixer.sound_command(23, &SoundCommand::Status).unwrap(),
            u32::MAX
        );
        assert_eq!(
            mixer
                .sound_command(23, &SoundCommand::Play { flags: 1 })
                .unwrap(),
            1
        );
        mixer.install_sound(23, sound.clone()).unwrap();
        assert_eq!(mixer.sound_command(23, &SoundCommand::Status).unwrap(), 0);
        mixer
            .sound_command(23, &SoundCommand::Play { flags: 0 })
            .unwrap();
        let mut pcm = [0.0; 4];
        mixer.render(&mut pcm, 1000, 1).unwrap();
        assert_eq!(pcm, [0.25, 0.5, -0.25, 0.0]);
        assert_eq!(mixer.sound_command(23, &SoundCommand::Status).unwrap(), 0);
        mixer
            .sound_command(23, &SoundCommand::Play { flags: 1 })
            .unwrap();
        mixer.render(&mut pcm, 1000, 1).unwrap();
        assert_eq!(pcm, [0.25, 0.5, -0.25, 0.25]); // no OGV repeated-prefix quirk
        assert_eq!(mixer.sound_command(23, &SoundCommand::Status).unwrap(), 5);
        mixer
            .sound_command(23, &SoundCommand::Play { flags: 0 })
            .unwrap();
        mixer.render(&mut pcm[..1], 1000, 1).unwrap();
        assert_eq!(pcm[0], 0.25);
        mixer.sound_command(23, &SoundCommand::Stop).unwrap();
        assert_eq!(mixer.sound_command(23, &SoundCommand::Status).unwrap(), 0);
        mixer
            .sound_command(23, &SoundCommand::Play { flags: 1 })
            .unwrap();
        mixer.install_sound(23, sound).unwrap();
        mixer.render(&mut pcm, 1000, 1).unwrap();
        assert_eq!(pcm, [0.0; 4]);
        let stream = Arc::new(AudioStream {
            sound: Sound {
                sample_rate: 1000,
                channels: 1,
                samples: vec![8192; 4],
            },
            loop_start_frame: 0,
            first_block_frames: 1,
            volume: StreamVolume::default(),
        });
        mixer.play(23, stream, 0).unwrap(); // same numeric handle uses separate channel
        mixer
            .sound_command(23, &SoundCommand::Play { flags: 0 })
            .unwrap();
        mixer.render(&mut pcm, 1000, 1).unwrap();
        assert_eq!(pcm, [0.5, 0.75, 0.0, 0.25]);
    }

    #[test]
    fn static_buffer_controls_and_headless_clock() {
        let sound = Arc::new(Sound {
            sample_rate: 22050,
            channels: 2,
            samples: vec![16384; 882],
        });
        let make = || {
            let mut mixer = Mixer::default();
            mixer.install_sound(1, sound.clone()).unwrap();
            mixer
                .sound_command(1, &SoundCommand::Play { flags: 1 })
                .unwrap();
            mixer
        };
        let mut mixer = make();
        mixer
            .sound_command(1, &SoundCommand::Volume { attenuation: -2000 })
            .unwrap();
        mixer
            .sound_command(1, &SoundCommand::Pan { attenuation: 2000 })
            .unwrap();
        let mut pcm = [0.0; 2];
        mixer.render(&mut pcm, 48000, 2).unwrap();
        assert!((pcm[0] - 0.005).abs() < 1e-6 && (pcm[1] - 0.05).abs() < 1e-6);
        mixer
            .sound_command(
                1,
                &SoundCommand::Pan {
                    attenuation: -10000,
                },
            )
            .unwrap();
        mixer.render(&mut pcm, 48000, 2).unwrap();
        assert!((pcm[0] - 0.05).abs() < 1e-6 && pcm[1] == 0.0);
        mixer
            .sound_command(
                1,
                &SoundCommand::Volume {
                    attenuation: -10000,
                },
            )
            .unwrap();
        mixer.render(&mut pcm, 48000, 2).unwrap();
        assert_eq!(pcm, [0.0; 2]);
        for hz in [100, 44100, 200000, 0] {
            let mut fast = make();
            let mut rendered = make();
            let varying = Arc::new(Sound {
                sample_rate: 22050,
                channels: 1,
                samples: (0..441).map(|i| (i * 73 - 16000) as i16).collect(),
            });
            for m in [&mut fast, &mut rendered] {
                m.install_sound(1, varying.clone()).unwrap();
                m.sound_command(1, &SoundCommand::Play { flags: 1 })
                    .unwrap();
                m.sound_command(1, &SoundCommand::Frequency { hz }).unwrap();
            }
            fast.advance_ms(37);
            rendered
                .render(&mut vec![0.0; 37 * 48 * 2], 48000, 2)
                .unwrap();
            let mut a = [0.0; 40];
            let mut b = a;
            fast.render(&mut a, 48000, 2).unwrap();
            rendered.render(&mut b, 48000, 2).unwrap();
            assert_eq!(a, b);
        }
        mixer
            .sound_command(1, &SoundCommand::Play { flags: 0 })
            .unwrap();
        mixer
            .sound_command(1, &SoundCommand::Frequency { hz: 44100 })
            .unwrap();
        mixer.advance_ms(9);
        assert_eq!(mixer.sound_command(1, &SoundCommand::Status).unwrap(), 1);
        mixer.advance_ms(1);
        assert_eq!(mixer.sound_command(1, &SoundCommand::Status).unwrap(), 0);
        assert!(
            mixer
                .sound_command(1, &SoundCommand::Frequency { hz: 99 })
                .is_err()
        );
        assert!(
            mixer
                .sound_command(1, &SoundCommand::Pan { attenuation: 10001 })
                .is_err()
        );
        assert!(
            mixer
                .sound_command(1, &SoundCommand::Volume { attenuation: 1 })
                .is_err()
        );
        assert!(
            mixer
                .sound_command(1, &SoundCommand::Play { flags: 2 })
                .is_err()
        );
    }

    #[test]
    fn resampling_chunking_restart_failure_and_channel_mix() {
        let stream = Arc::new(AudioStream {
            sound: Sound {
                sample_rate: 2,
                channels: 2,
                samples: vec![8192, -8192, 16384, -16384, 8192, -8192],
            },
            loop_start_frame: 0,
            first_block_frames: 1,
            volume: StreamVolume::default(),
        });
        let mut mixer = Mixer::default();
        mixer.play(1, stream.clone(), 0).unwrap();
        let mut first = [0.0; 8];
        mixer.render(&mut first, 4, 2).unwrap();
        assert_eq!(
            first,
            [0.25, -0.25, 0.375, -0.375, 0.5, -0.5, 0.375, -0.375]
        );
        // Rejected requests must not replace or rewind a playing voice.
        assert!(mixer.play(1, stream.clone(), 0x30002).is_err());
        let mut end = [0.0; 6];
        mixer.render(&mut end, 4, 2).unwrap();
        assert_eq!(end, [0.25, -0.25, 0.25, -0.25, 0.0, 0.0]);
        assert!(!mixer.is_playing(1));
        mixer.play(1, stream.clone(), 2).unwrap();
        let mut whole = [0.0; 62];
        mixer.render(&mut whole, 7, 2).unwrap();
        mixer.play(1, stream.clone(), 2).unwrap();
        let mut chunks = [0.0; 62];
        for chunk in chunks.chunks_mut(6) {
            mixer.render(chunk, 7, 2).unwrap();
        }
        assert_eq!(whole, chunks);
        mixer.play(2, stream, 2).unwrap();
        let mut mono = [1.0; 32];
        mixer.render(&mut mono, 2, 1).unwrap();
        assert_eq!(mono, [0.0; 32]);
        mixer.stop(1);
        mixer.stop(2);
        assert!(!mixer.is_playing(1));
        assert!(mixer.render(&mut mono, 0, 1).is_err());
        assert!(mixer.render(&mut mono[..3], 8000, 2).is_err());
    }
    #[test]
    #[ignore = "requires a live audio output device; plays a short synthetic sine"]
    fn native_audio_device_callback() {
        let output = AudioOutput::open().unwrap();
        output.play(1, synthetic(), 0).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while (output.rendered_frames() == 0 || output.is_playing(1).unwrap())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
            output.check().unwrap();
        }
        assert!(
            output.rendered_frames() > 0,
            "audio device did not request frames"
        );
        assert!(
            !output.is_playing(1).unwrap(),
            "device did not consume the test voice"
        );
        std::thread::sleep(std::time::Duration::from_millis(150));
        output.check().unwrap();
    }
}
