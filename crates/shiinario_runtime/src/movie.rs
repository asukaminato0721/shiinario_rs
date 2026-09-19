//! Legacy movie slots. MPEG video is decoded incrementally; only compressed
//! input, PCM audio and a small look-ahead of video frames remain resident.
use crate::{
    resources::{AudioStream, Resources, Sound, StreamVolume, Surface},
    session::Host,
};
use anyhow::{Context, Result, ensure};
use na_mpeg2_decoder::{Frame, MpegAudioPipeline, MpegVideoPipeline, frame_to_rgba_bt601_limited};
use shiinario_scenario::{MovieCommand, PlatformRequest, SurfacePoint, SurfaceStretch};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
const CHUNK: usize = 2048;
// Resource audio handles live in the VM heap address range, below this range.
const AUDIO_HANDLE: u32 = 0xf000_0000;
const STOPPED: u32 = 0x20d;
const PLAYING: u32 = 0x20e;
const PAUSED: u32 = 0x211;

struct Video {
    bytes: Arc<[u8]>,
    decoder: MpegVideoPipeline,
    offset: usize,
    frames: VecDeque<Arc<Frame>>,
    eof: bool,
    interval_us: u64,
    origin_us: i64,
    last_us: Option<u64>,
    next: Option<(u64, Arc<Frame>)>,
    end_us: u64,
}
impl Video {
    fn new(bytes: Arc<[u8]>, interval_us: u64, origin_us: i64) -> Self {
        Self {
            bytes,
            decoder: MpegVideoPipeline::new(),
            offset: 0,
            frames: VecDeque::new(),
            eof: false,
            interval_us,
            origin_us,
            last_us: None,
            next: None,
            end_us: 0,
        }
    }
    fn next_frame(&mut self) -> Result<Option<(u64, Arc<Frame>)>> {
        while self.frames.is_empty() && !self.eof {
            if self.offset == self.bytes.len() {
                self.decoder.flush_with(|f| self.frames.push_back(f))?;
                self.eof = true;
            } else {
                let end = (self.offset + CHUNK).min(self.bytes.len());
                self.decoder
                    .push_with(&self.bytes[self.offset..end], None, |f| {
                        self.frames.push_back(f)
                    })?;
                self.offset = end;
            }
            let buffered: usize = self.frames.iter().map(|f| f.width * f.height * 4).sum();
            ensure!(
                buffered <= 64 * 1024 * 1024,
                "movie frame look-ahead exceeds 64 MiB"
            );
        }
        let Some(frame) = self.frames.pop_front() else {
            return Ok(None);
        };
        let pts = frame
            .pts_90k
            .map(|pts| (pts * 100 / 9 - self.origin_us).max(0) as u64);
        // PES packets need not timestamp every picture. Repeated timestamps
        // use the sequence frame rate, as in the Siglus streaming player.
        let time = match self.last_us {
            None => pts.unwrap_or(0),
            Some(last) => pts
                .filter(|pts| *pts > last)
                .unwrap_or(last + self.interval_us),
        };
        self.last_us = Some(time);
        self.end_us = time + self.interval_us;
        Ok(Some((time, frame)))
    }
    fn frame_at(&mut self, time_us: u64) -> Result<Option<Arc<Frame>>> {
        let mut latest = None;
        loop {
            if self.next.is_none() {
                self.next = self.next_frame()?;
            }
            match &self.next {
                Some((time, _)) if *time <= time_us => latest = self.next.take().map(|(_, f)| f),
                _ => break,
            }
        }
        Ok(latest)
    }
    fn finished(&self, time_us: u64) -> bool {
        self.eof && self.next.is_none() && self.frames.is_empty() && time_us >= self.end_us
    }
}

struct Movie {
    video: Video,
    audio: Option<Sound>,
    audio_delay_ms: u64,
    active_audio: Option<Arc<AudioStream>>,
    dimensions: [u32; 2],
    surface: u32,
    rect: [i32; 4],
    state: u32,
    looping: bool,
    loops: u32,
    volume: u32,
    position_ms: u64,
    clock_ms: u32,
}
impl Movie {
    fn open(bytes: Vec<u8>, surface: u32, flags: u32) -> Result<Self> {
        ensure!(
            bytes.len() <= MEMORY_LIMIT,
            "compressed movie exceeds 256 MiB"
        );
        let header = bytes
            .windows(8)
            .find(|b| b[..4] == [0, 0, 1, 0xb3])
            .context("unsupported movie format: expected MPEG-1/2 video")?;
        let dimensions = [
            u32::from(header[4]) * 16 + u32::from(header[5] >> 4),
            u32::from(header[5] & 15) * 256 + u32::from(header[6]),
        ];
        ensure!(
            dimensions.iter().all(|v| *v > 0 && *v <= 4096),
            "invalid movie dimensions"
        );
        let (numerator, denominator) = match header[7] & 15 {
            1 => (24000, 1001),
            2 => (24, 1),
            3 => (25, 1),
            4 => (30000, 1001),
            5 => (30, 1),
            6 => (50, 1),
            7 => (60000, 1001),
            8 => (60, 1),
            _ => anyhow::bail!("invalid MPEG frame rate"),
        };
        let interval_us = 1_000_000 * denominator / numerator;
        let mut pipeline = MpegAudioPipeline::new();
        let mut audio = None::<Sound>;
        let mut audio_pts = None;
        let mut audio_error = None;
        for chunk in bytes.chunks(64 * 1024) {
            pipeline.push_with(chunk, None, |a| {
                if audio_error.is_some() {
                    return;
                }
                if !matches!(a.channels, 1 | 2) || a.sample_rate == 0 {
                    audio_error = Some("unsupported movie audio format");
                    return;
                }
                let sound = audio.get_or_insert_with(|| {
                    audio_pts = Some(a.pts_ms);
                    Sound {
                        channels: a.channels as u8,
                        sample_rate: a.sample_rate,
                        samples: Vec::new(),
                    }
                });
                if sound.channels != a.channels as u8 || sound.sample_rate != a.sample_rate {
                    audio_error = Some("movie audio format changes during playback");
                    return;
                }
                if (sound.samples.len() + a.samples.len()) * 2 + bytes.len() > MEMORY_LIMIT {
                    audio_error = Some("movie input and PCM exceed 256 MiB");
                    return;
                }
                sound.samples.extend(
                    a.samples
                        .iter()
                        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16),
                );
            })?;
            if let Some(error) = audio_error {
                anyhow::bail!("{error}");
            }
        }
        let mut video = Video::new(bytes.into(), interval_us, 0);
        let (_, first) = video
            .next_frame()?
            .context("movie contains no decoded video frames")?;
        let video_origin = first.pts_90k.map_or(0, |pts| pts * 100 / 9);
        let origin = audio_pts.map_or(video_origin, |pts| (pts * 1000).min(video_origin));
        let first_time = (video_origin - origin).max(0) as u64;
        video.origin_us = origin;
        video.last_us = Some(first_time);
        video.end_us = first_time + interval_us;
        video.next = Some((first_time, first));
        Ok(Self {
            video,
            audio,
            audio_delay_ms: audio_pts.map_or(0, |pts| (pts - origin / 1000).max(0) as u64),
            active_audio: None,
            dimensions,
            surface,
            rect: [0, 0, dimensions[0] as i32, dimensions[1] as i32],
            state: STOPPED,
            looping: flags != 0,
            loops: 0,
            volume: 100,
            position_ms: 0,
            clock_ms: 0,
        })
    }
    fn resident(&self) -> usize {
        // Reserve both the decoded track and the mixer-owned playback copy,
        // including any leading silence used to align audio and video PTS.
        self.video.bytes.len()
            + self.audio.as_ref().map_or(0, |a| {
                let silence =
                    self.audio_delay_ms * u64::from(a.sample_rate) / 1000 * u64::from(a.channels);
                a.samples.len() * 4 + silence as usize * 2
            })
    }
    fn rewind_video(&mut self) {
        self.video = Video::new(
            self.video.bytes.clone(),
            self.video.interval_us,
            self.video.origin_us,
        );
    }
    fn audio_end_ms(&self) -> u64 {
        self.audio.as_ref().map_or(0, |a| {
            self.audio_delay_ms
                + (a.samples.len() as u64 * 1000)
                    .div_ceil(u64::from(a.sample_rate) * u64::from(a.channels))
        })
    }
    fn stop_audio(&mut self, id: u32, host: &mut impl Host) -> Result<()> {
        if self.active_audio.take().is_some() {
            host.stop_stream(AUDIO_HANDLE + id)?;
        }
        Ok(())
    }
    fn start_audio(&mut self, id: u32, host: &mut impl Host) -> Result<()> {
        self.stop_audio(id, host)?;
        let Some(audio) = &self.audio else {
            return Ok(());
        };
        let channels = usize::from(audio.channels);
        let start = (self.position_ms.saturating_sub(self.audio_delay_ms)
            * u64::from(audio.sample_rate)
            / 1000) as usize
            * channels;
        if start >= audio.samples.len() {
            return Ok(());
        }
        let silence = (self.audio_delay_ms.saturating_sub(self.position_ms)
            * u64::from(audio.sample_rate)
            / 1000) as usize
            * channels;
        ensure!(
            silence * 2 + audio.samples.len() * 2 <= MEMORY_LIMIT,
            "movie audio delay exceeds memory limit"
        );
        let mut samples = vec![0; silence];
        samples.extend_from_slice(&audio.samples[start..]);
        let stream = Arc::new(AudioStream {
            first_block_frames: (samples.len() / channels).min(1024),
            sound: Sound {
                sample_rate: audio.sample_rate,
                channels: audio.channels,
                samples,
            },
            loop_start_frame: 0,
            volume: StreamVolume::default(),
        });
        stream.volume.set_percent(self.volume)?;
        host.play_stream(AUDIO_HANDLE + id, stream.clone(), 0)?;
        self.active_audio = Some(stream);
        Ok(())
    }
    fn update(
        &mut self,
        id: u32,
        now: u32,
        resources: &mut Resources,
        host: &mut impl Host,
    ) -> Result<()> {
        if self.state != PLAYING {
            return Ok(());
        }
        self.position_ms += u64::from(now.wrapping_sub(self.clock_ms));
        self.clock_ms = now;
        if let Some(frame) = self.video.frame_at(self.position_ms * 1000)? {
            let mut rgba = vec![0; frame.width * frame.height * 4];
            frame_to_rgba_bt601_limited(&frame, &mut rgba);
            resources.write_movie_frame(
                self.surface,
                self.rect,
                &Surface {
                    width: frame.width as u32,
                    height: frame.height as u32,
                    rgba,
                },
            )?;
            if self.surface == 0 {
                invalidate(host)?;
            }
        }
        if self.video.finished(self.position_ms * 1000) && self.position_ms >= self.audio_end_ms() {
            self.stop_audio(id, host)?;
            if self.looping {
                let duration = self
                    .video
                    .end_us
                    .div_ceil(1000)
                    .max(self.audio_end_ms())
                    .max(1);
                self.loops = self
                    .loops
                    .wrapping_add((self.position_ms / duration) as u32);
                self.position_ms %= duration;
                self.rewind_video();
                self.start_audio(id, host)?;
                self.update(id, now, resources, host)?;
            } else {
                self.state = STOPPED;
            }
        }
        Ok(())
    }
}

fn invalidate(host: &mut impl Host) -> Result<()> {
    host.respond(&PlatformRequest::InvalidateRect {
        rect: [0, 0, 800, 600],
    })?;
    Ok(())
}

#[derive(Default)]
pub struct Movies {
    slots: BTreeMap<u32, Movie>,
}
impl Movies {
    pub fn update(&mut self, host: &mut impl Host, resources: &mut Resources) -> Result<()> {
        if !self.slots.values().any(|m| m.state == PLAYING) {
            return Ok(());
        }
        let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
        for (&id, movie) in &mut self.slots {
            movie.update(id, now, resources, host)?;
        }
        Ok(())
    }
    pub fn dimensions(&self, id: u32) -> Result<[i32; 2]> {
        Ok(self
            .slots
            .get(&id)
            .context("unknown movie slot")?
            .dimensions
            .map(|v| v as i32))
    }
    pub fn stop_all(&mut self, host: &mut impl Host) -> Result<()> {
        for (&id, movie) in &mut self.slots {
            movie.stop_audio(id, host)?;
        }
        self.slots.clear();
        Ok(())
    }
    pub fn open(
        &mut self,
        id: u32,
        command: &MovieCommand,
        bytes: Vec<u8>,
        resources: &mut Resources,
        host: &mut impl Host,
    ) -> Result<u32> {
        let MovieCommand::Open {
            flags,
            surface,
            play,
            ..
        } = command
        else {
            anyhow::bail!("expected movie open command");
        };
        ensure!(id < 16, "movie slot out of bounds");
        let surface = if *surface == u32::MAX { 0 } else { *surface };
        ensure!(
            resources.surface_memory(surface).is_some(),
            "missing movie surface {surface}"
        );
        let movie = Movie::open(bytes, surface, *flags)?;
        let resident: usize = self
            .slots
            .iter()
            .filter(|(slot, _)| **slot != id)
            .map(|(_, m)| m.resident())
            .sum();
        ensure!(
            resident + movie.resident() <= MEMORY_LIMIT,
            "movie slots exceed 256 MiB"
        );
        if let Some(old) = self.slots.get_mut(&id) {
            old.stop_audio(id, host)?;
        }
        self.slots.insert(id, movie);
        if *play {
            return self.command(id, &MovieCommand::Play, resources, host);
        }
        Ok(1)
    }
    pub fn command(
        &mut self,
        id: u32,
        command: &MovieCommand,
        resources: &mut Resources,
        host: &mut impl Host,
    ) -> Result<u32> {
        ensure!(id < 16, "movie slot out of bounds");
        let movie = self.slots.get_mut(&id).context("unknown movie slot")?;
        let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
        movie.update(id, now, resources, host)?;
        match command {
            MovieCommand::Stop => {
                movie.stop_audio(id, host)?;
                movie.state = STOPPED;
                movie.loops = u32::MAX;
            }
            MovieCommand::Pause => {
                if movie.state == PLAYING {
                    movie.stop_audio(id, host)?;
                    movie.state = PAUSED;
                }
            }
            MovieCommand::Play => {
                if movie.state != PLAYING {
                    if movie.state == STOPPED {
                        movie.position_ms = 0;
                        movie.rewind_video();
                    }
                    movie.start_audio(id, host)?;
                    movie.clock_ms = host.respond(&PlatformRequest::ClockMilliseconds)?;
                    movie.state = PLAYING;
                    movie.update(id, movie.clock_ms, resources, host)?;
                }
            }
            MovieCommand::Status => return Ok(movie.state),
            MovieCommand::Position => {
                return Ok(if movie.state == STOPPED {
                    u32::MAX
                } else {
                    movie.position_ms as u32
                });
            }
            MovieCommand::Seek { milliseconds } => {
                if movie.state != STOPPED {
                    movie.position_ms = u64::from(*milliseconds);
                    movie.rewind_video();
                    if movie.state == PLAYING {
                        movie.start_audio(id, host)?;
                        movie.clock_ms = host.respond(&PlatformRequest::ClockMilliseconds)?;
                        movie.update(id, movie.clock_ms, resources, host)?;
                    }
                }
            }
            MovieCommand::LoopCount => return Ok(movie.loops),
            MovieCommand::Rect { rect } => {
                ensure!(
                    (0..=16384).contains(&rect[2]) && (0..=16384).contains(&rect[3]),
                    "invalid movie rectangle"
                );
                movie.rect = *rect;
            }
            MovieCommand::Update { present } => {
                if *present {
                    let [x, y, w, h] = movie.rect;
                    resources.present_movie(&SurfaceStretch {
                        destination: SurfacePoint { id: 0, x, y },
                        destination_size: [w, h],
                        source: SurfacePoint {
                            id: movie.surface,
                            x: 0,
                            y: 0,
                        },
                        source_size: resources.surface_dimensions(movie.surface)?,
                        mode: 0xcc0020,
                    })?;
                    invalidate(host)?;
                }
            }
            MovieCommand::Volume => return Ok(movie.volume),
            MovieCommand::SetVolume { percent } => {
                ensure!(*percent <= 100, "movie volume exceeds 100");
                movie.volume = *percent;
                if let Some(audio) = &movie.active_audio {
                    audio.volume.set_percent(*percent)?;
                }
            }
            MovieCommand::Dimensions | MovieCommand::Open { .. } => unreachable!(),
        }
        Ok(1)
    }
}
