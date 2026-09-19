//! Stream playback, independent of SCN and windowing. Device output uses CPAL.
use crate::resources::{AudioStream, StreamVolume};
use anyhow::{Context, Result, bail, ensure};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
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
}
impl Voice {
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
        let volume = self.stream.volume.percent();
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
}
impl Mixer {
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
                true
            } else {
                false
            }
        });
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
    fn matches_original_stream_pcm_including_loop_prefix() {
        let stream = synthetic();
        for (flags, compressed) in [
            (
                0,
                include_bytes!("../../shiinario_assets/tests/fixtures/sine-play-0.pcm.gz")
                    .as_slice(),
            ),
            (
                2,
                include_bytes!("../../shiinario_assets/tests/fixtures/sine-play-loop.pcm.gz")
                    .as_slice(),
            ),
        ] {
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(compressed)
                .take(256001)
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes.len(), 256000);
            let mut mixer = Mixer::default();
            mixer.play(7, stream.clone(), flags).unwrap();
            let mut actual = vec![0.0; bytes.len() / 2];
            mixer.render(&mut actual, 8000, 1).unwrap();
            for (index, (actual, expected)) in
                actual.iter().zip(bytes.as_chunks::<2>().0).enumerate()
            {
                let expected = i16::from_le_bytes(*expected);
                let actual = (actual * 32768.0).round() as i32;
                assert!(
                    (actual - i32::from(expected)).abs() <= 1,
                    "sample {index}: {actual} != {expected}"
                );
            }
            assert_eq!(mixer.is_playing(7), flags == 2);
        }
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
    fn volume_matches_native_and_changes_without_restarting() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/stream-volume-probe.json"
        ))
        .unwrap();
        for case in probe["cases"].as_array().unwrap() {
            let percent = case["percent"].as_u64().unwrap() as u32;
            assert_eq!(
                StreamVolume::attenuation(percent).unwrap(),
                case["attenuation"].as_i64().unwrap() as i32
            );
            let mut code = 0x06dfu16.to_le_bytes().to_vec();
            for value in [7u32, percent] {
                code.push(4);
                code.extend(value.to_le_bytes());
            }
            code.extend(0x049du16.to_le_bytes());
            code.push(4);
            code.extend(99u32.to_le_bytes());
            let mut vm = shiinario_scenario::BinaryVm::new("volume.SCN", code).unwrap();
            let event = vm.step().unwrap();
            assert!(matches!(&event, shiinario_scenario::Event::Platform {
                request: shiinario_scenario::PlatformRequest::SetAudioStreamVolume {handle:7,percent:value},..
            } if *value==percent));
            assert_eq!(vm.step().unwrap(), event);
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset,
                case["next_offset"].as_u64().unwrap() as usize
            );
            assert!(matches!(
                vm.step().unwrap(),
                shiinario_scenario::Event::MouseButtonMapping { value: 99, .. }
            ));
        }
        let stream = synthetic();
        let mut mixer = Mixer::default();
        stream.volume.set_percent(50).unwrap();
        mixer.play(1, stream.clone(), 2).unwrap();
        let mut quiet = [0.0; 12];
        mixer.render(&mut quiet, 8000, 1).unwrap();
        let gain = 10.0f32.powf(-0.5);
        for (actual, expected) in quiet.iter().zip(&stream.sound.samples) {
            assert!((actual - f32::from(*expected) / 32768.0 * gain).abs() < 1e-6);
        }
        stream.volume.set_percent(0).unwrap();
        let mut silent = [1.0; 16];
        mixer.render(&mut silent, 8000, 1).unwrap();
        assert_eq!(silent, [0.0; 16]);
        assert!(mixer.is_playing(1));
        stream.volume.set_percent(100).unwrap();
        assert!(stream.volume.set_percent(101).is_err());
        assert!(StreamVolume::attenuation(u32::MAX).is_err());
        assert_eq!(stream.volume.percent(), 100);
        let mut restored = [0.0; 8];
        mixer.render(&mut restored, 8000, 1).unwrap();
        for (actual, expected) in restored.iter().zip(&stream.sound.samples[28..]) {
            assert_eq!(*actual, f32::from(*expected) / 32768.0);
        }
        // Volume belongs to the stream, and survives stopping and replaying it.
        stream.volume.set_percent(50).unwrap();
        mixer.stop(1);
        mixer.play(1, stream.clone(), 0).unwrap();
        let mut replay = [0.0; 12];
        mixer.render(&mut replay, 8000, 1).unwrap();
        assert_eq!(quiet, replay);
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
