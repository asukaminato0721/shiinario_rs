//! Window-independent image slots and drawing buffers used by binary SCN.
use anyhow::{Context, Result, bail, ensure};
use shiinario_assets::{audio, image, project::Project};
use shiinario_scenario::{
    ImageAffine, ImageDraw, MaskTransition, PlatformRequest, SharedMemory, SurfaceBlend,
    SurfaceCapture, SurfaceCopy, SurfaceStretch,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

mod graphics;

const LIMIT: usize = 256 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
struct DrawingSurface {
    width: u32,
    height: u32,
    stride: usize,
    pixels: SharedMemory,
}
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub surface: Surface,
    methods: Vec<u8>,
}
pub struct Sound {
    pub sample_rate: u32,
    pub channels: u8,
    pub samples: Vec<i16>,
}
pub struct AudioStream {
    pub sound: Sound,
    pub loop_start_frame: u64,
    /// First ov_read block after seeking to zero (at most 4096 PCM bytes).
    pub first_block_frames: usize,
    pub volume: StreamVolume,
}
/// Shared with the audio callback so volume changes preserve playback position.
pub struct StreamVolume(AtomicU32);
impl Default for StreamVolume {
    fn default() -> Self {
        Self(AtomicU32::new(100))
    }
}
impl StreamVolume {
    // The original fade worker retains the signed overshoot when it stops
    // playback. Keep that value observable through opcode 06e3.
    pub(crate) fn set_fade_percent(&self, percent: u32) {
        self.0.store(percent, Ordering::Relaxed);
    }
    pub fn percent(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }
    pub fn set_percent(&self, percent: u32) -> Result<()> {
        ensure!(
            percent <= 100,
            "audio volume percentage out of bounds: {percent}"
        );
        self.0.store(percent, Ordering::Relaxed);
        Ok(())
    }
    /// Original 101-entry table, verified by executing every valid index.
    pub fn attenuation(percent: u32) -> Result<i32> {
        ensure!(
            percent <= 100,
            "audio volume percentage out of bounds: {percent}"
        );
        Ok(if percent == 0 {
            -10000
        } else {
            (1000.0 * (f64::from(percent) / 100.0).log2()) as i32
        })
    }
}
enum Image {
    Encoded(Vec<u8>),
    Mutable {
        bytes_per_pixel: u32,
        frames: Vec<Surface>,
    },
}
impl Image {
    fn size(&self) -> usize {
        match self {
            Self::Encoded(bytes) => bytes.len(),
            Self::Mutable { frames, .. } => frames.iter().map(|frame| frame.rgba.len()).sum(),
        }
    }
}
struct OpenFile {
    name: String,
    bytes: Vec<u8>,
    position: usize,
    access: u32,
}
#[derive(Default)]
pub struct Resources {
    text_renderer: crate::text_render::TextRenderer,
    surfaces: BTreeMap<u32, DrawingSurface>,
    display: Option<Surface>,
    text_damage: Vec<[i32; 4]>,
    images: BTreeMap<u32, Image>,
    sounds: BTreeMap<u32, Arc<Sound>>,
    streams: BTreeMap<u32, Arc<AudioStream>>,
    resident: usize,
    archives: Vec<String>,
    transient_files: BTreeMap<String, Vec<u8>>,
    transient_directories: std::collections::BTreeSet<String>,
    files: BTreeMap<u32, OpenFile>,
}
impl Resources {
    /// Write into the existing allocation so SCN pointers retain their identity.
    pub(crate) fn write_movie_frame(
        &mut self,
        id: u32,
        rect: [i32; 4],
        frame: &Surface,
    ) -> Result<()> {
        let dst = self.surfaces.get(&id).context("missing movie surface")?;
        let [x, y, width, height] = rect.map(i64::from);
        if width == 0 || height == 0 {
            return Ok(());
        }
        ensure!(
            width > 0 && height > 0 && frame.width > 0 && frame.height > 0,
            "invalid movie frame dimensions"
        );
        ensure!(
            frame.rgba.len() == frame.width as usize * frame.height as usize * 4,
            "invalid movie frame buffer"
        );
        let mut pixels = dst.pixels.read(0, dst.pixels.len())?;
        for dy in y.max(0)..(y + height).min(i64::from(dst.height)) {
            let sy = ((dy - y) * i64::from(frame.height) / height) as usize;
            for dx in x.max(0)..(x + width).min(i64::from(dst.width)) {
                let sx = ((dx - x) * i64::from(frame.width) / width) as usize;
                let src = (sy * frame.width as usize + sx) * 4;
                let dest = dy as usize * dst.stride + dx as usize * 3;
                pixels[dest..dest + 3].copy_from_slice(&[
                    frame.rgba[src + 2],
                    frame.rgba[src + 1],
                    frame.rgba[src],
                ]);
            }
        }
        dst.pixels.write(0, &pixels)
    }
    pub(crate) fn present_movie(&mut self, stretch: &SurfaceStretch) -> Result<()> {
        self.stretch_surface(stretch)
    }
    pub(crate) fn surface_dimensions(&self, id: u32) -> Result<[i32; 2]> {
        let surface = self.surfaces.get(&id).context("missing movie surface")?;
        Ok([surface.width as i32, surface.height as i32])
    }
    /// The native pre-SCN initializer allocates Vram buffers (default two).
    pub fn for_project(project: &Project) -> Result<Self> {
        let count: i32 = project
            .config
            .values
            .get("vram")
            .map(|value| value.parse())
            .transpose()
            .context("invalid Vram configuration")?
            .unwrap_or(-1);
        ensure!(count >= -1, "invalid negative Vram configuration");
        let count = if count == -1 { 2 } else { count.max(1) };
        ensure!(count <= 256, "initial surface count exceeds 256");
        let mut resources = Self::default();
        for id in 0..count as u32 {
            resources.create_surface(id, project.config.width, project.config.height, 0)?;
        }
        resources.commit_display([
            0,
            0,
            project.config.width as i32,
            project.config.height as i32,
        ]);
        Ok(resources)
    }
    pub fn asset_sizes(&self, project: &Project, name: &str) -> Result<[u32; 2]> {
        if let Some(bytes) = self
            .transient_files
            .get(&shiinario_assets::project::normalize(name)?)
        {
            return Ok([0, bytes.len() as u32]);
        }
        project.sizes_with_archives(name, &self.archives)
    }
    pub fn read_asset(&self, project: &Project, name: &str) -> Result<Vec<u8>> {
        // SCN asset reads also load saves. Match asset_sizes: simulated writes
        // override disk files, which take precedence over registered archives.
        if let Some(bytes) = self
            .transient_files
            .get(&shiinario_assets::project::normalize(name)?)
        {
            return Ok(bytes.clone());
        }
        if let Some(bytes) = project.read_loose(name)? {
            return Ok(bytes);
        }
        project.read_with_archives(name, &self.archives)
    }
    pub fn read_file_contents(&self, project: &Project, name: &str) -> Result<Vec<u8>> {
        if let Some(bytes) = self
            .transient_files
            .get(&shiinario_assets::project::normalize(name)?)
        {
            return Ok(bytes.clone());
        }
        project.read_loose(name)?.context("file not found")
    }
    pub fn open_file(&mut self, project: &Project, name: &str, access: u32) -> Result<[u32; 4]> {
        ensure!(
            access & !0xc0000000 == 0 && access != 0,
            "unsupported file access {access:#x}"
        );
        ensure!(self.files.len() < 256, "too many open files");
        let key = shiinario_assets::project::normalize(name)?;
        let bytes = match self.transient_files.get(&key) {
            Some(bytes) => bytes.clone(),
            None => project.read_loose(name)?.context("file not found")?,
        };
        let total: usize = self.files.values().map(|file| file.bytes.len()).sum();
        ensure!(
            bytes.len() <= 16 * 1024 * 1024 && total + bytes.len() <= 64 * 1024 * 1024,
            "open files exceed memory cap"
        );
        let handle = (1..=256).find(|id| !self.files.contains_key(id)).unwrap();
        let length = bytes.len() as u32;
        self.files.insert(
            handle,
            OpenFile {
                name: key,
                bytes,
                position: 0,
                access,
            },
        );
        Ok([0, length, 0, handle])
    }
    pub fn close_file(&mut self, handle: u32) -> Result<()> {
        self.files.remove(&handle).context("invalid file handle")?;
        Ok(())
    }
    pub fn read_file(&mut self, handle: u32, length: u32) -> Result<Vec<u8>> {
        ensure!(length <= 16 * 1024 * 1024, "file read exceeds cap");
        let file = self.files.get_mut(&handle).context("invalid file handle")?;
        ensure!(
            file.access & 0x80000000 != 0,
            "file is not open for reading"
        );
        let start = file.position.min(file.bytes.len());
        let end = file
            .position
            .saturating_add(length as usize)
            .min(file.bytes.len());
        let bytes = file.bytes[start..end].to_vec();
        file.position += bytes.len();
        Ok(bytes)
    }
    pub fn seek_file(&mut self, handle: u32, distance: i32, origin: u32) -> Result<u32> {
        let file = self.files.get_mut(&handle).context("invalid file handle")?;
        let base = match origin {
            0 => 0,
            1 => file.position,
            2 => file.bytes.len(),
            _ => bail!("invalid file seek origin"),
        };
        let position = base as i64 + i64::from(distance);
        ensure!(
            (0..=u32::MAX as i64 - 1).contains(&position),
            "invalid file seek position"
        );
        file.position = position as usize;
        Ok(position as u32)
    }
    pub fn write_file_handle(
        &mut self,
        project: &Project,
        handle: u32,
        bytes: Vec<u8>,
        simulated: bool,
    ) -> Result<u32> {
        let file = self.files.get(&handle).context("invalid file handle")?;
        ensure!(
            file.access & 0x40000000 != 0,
            "file is not open for writing"
        );
        if bytes.is_empty() {
            return Ok(0);
        }
        let end = file
            .position
            .checked_add(bytes.len())
            .context("file size overflow")?;
        ensure!(end <= 16 * 1024 * 1024, "file write exceeds cap");
        let total: usize = self
            .files
            .iter()
            .filter(|(id, _)| **id != handle)
            .map(|(_, file)| file.bytes.len())
            .sum();
        ensure!(
            total + end.max(file.bytes.len()) <= 64 * 1024 * 1024,
            "open files exceed memory cap"
        );
        let mut contents = file.bytes.clone();
        contents.resize(end.max(contents.len()), 0);
        contents[file.position..end].copy_from_slice(&bytes);
        let name = file.name.clone();
        self.write_file(project, &name, contents.clone(), simulated)?;
        let file = self.files.get_mut(&handle).unwrap();
        file.bytes = contents;
        file.position = end;
        Ok(bytes.len() as u32)
    }
    pub fn file_exists(&self, project: &Project, name: &str) -> Result<bool> {
        Ok(self
            .transient_files
            .contains_key(&shiinario_assets::project::normalize(name)?)
            || self
                .transient_directories
                .contains(&shiinario_assets::project::normalize(name)?)
            || project.loose_path_exists(name)?)
    }
    pub fn create_directory(
        &mut self,
        project: &Project,
        name: &str,
        simulated: bool,
    ) -> Result<bool> {
        if !simulated {
            return project.create_directory(name);
        }
        let key = shiinario_assets::project::normalize(name)?;
        if self.file_exists(project, &key)? {
            return Ok(false);
        }
        if let Some((parent, _)) = key.rsplit_once('/')
            && !self.transient_directories.contains(parent)
            && !project.directory_exists(parent)?
        {
            return Ok(false);
        }
        ensure!(
            self.transient_directories.len() < 256,
            "too many transient directories"
        );
        Ok(self.transient_directories.insert(key))
    }
    pub fn write_file(
        &mut self,
        project: &Project,
        name: &str,
        bytes: Vec<u8>,
        simulated: bool,
    ) -> Result<()> {
        if simulated {
            let key = shiinario_assets::project::normalize(name)?;
            let total: usize = self
                .transient_files
                .iter()
                .filter(|(name, _)| **name != key)
                .map(|(_, bytes)| bytes.len())
                .sum();
            ensure!(
                total + bytes.len() <= 64 * 1024 * 1024,
                "transient files exceed 64 MiB"
            );
            self.transient_files.insert(key, bytes);
            Ok(())
        } else {
            project.write_file(name, &bytes)
        }
    }
    pub fn register_archive(&mut self, name: &str) {
        if !self
            .archives
            .iter()
            .any(|item| item.eq_ignore_ascii_case(name))
        {
            self.archives.push(name.to_owned());
        }
    }
    /// Original 432f00 invalidates each glyph's bounds (434060 -> 417560).
    /// Paint after that text scheduler invocation returns: a delayed character
    /// is visible before the next wait, while instant text finishes as a batch.
    /// Keep separate rectangles so wrapping cannot expose unfinished pixels
    /// between lines. The host font's raster bounds include shadow and edges.
    /// Return the union for the host's paint notification. Only the individual
    /// glyph rectangles are copied from the working DIB into the display.
    pub(crate) fn commit_text_display(&mut self) -> Option<[i32; 4]> {
        let mut damage = std::mem::take(&mut self.text_damage);
        let mut invalidated: Option<[i32; 4]> = None;
        for rect in damage.drain(..) {
            self.commit_display(rect);
            let union = invalidated.get_or_insert(rect);
            union[0] = union[0].min(rect[0]);
            union[1] = union[1].min(rect[1]);
            union[2] = union[2].max(rect[2]);
            union[3] = union[3].max(rect[3]);
        }
        self.text_damage = damage;
        invalidated
    }
    /// Copy a submitted Windows RECT into the retained display. Pixels outside
    /// the RECT can already contain an unfinished background/text composition.
    pub(crate) fn commit_display(&mut self, rect: [i32; 4]) {
        let Some(surface) = self.surfaces.get(&0) else {
            return;
        };
        let [left, top, right, bottom] = rect;
        let left = left.clamp(0, surface.width as i32) as usize;
        let right = right.clamp(0, surface.width as i32) as usize;
        let top = top.clamp(0, surface.height as i32) as usize;
        let bottom = bottom.clamp(0, surface.height as i32) as usize;
        if left >= right || top >= bottom {
            return;
        }
        if self.display.as_ref().is_none_or(|display| {
            display.width != surface.width || display.height != surface.height
        }) {
            let mut rgba = vec![0; surface.width as usize * surface.height as usize * 4];
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            self.display = Some(Surface {
                width: surface.width,
                height: surface.height,
                rgba,
            });
        }
        let display = self.display.as_mut().unwrap();
        let stride = surface.width as usize * 4;
        let bytes = surface
            .pixels
            .read(top * surface.stride, (bottom - top) * surface.stride)
            .expect("clipped display rows are inside the drawing surface");
        for (y, row) in (top..bottom).zip(bytes.chunks_exact(surface.stride)) {
            let target = &mut display.rgba[y * stride + left * 4..y * stride + right * 4];
            for (rgba, bgr) in target
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(row[left * 3..right * 3].as_chunks::<3>().0)
            {
                rgba.copy_from_slice(&[bgr[2], bgr[1], bgr[0], 255]);
            }
        }
    }
    pub fn display(&self) -> Option<&Surface> {
        self.display.as_ref()
    }
    /// Snapshot a working top-down BGR24 DIB, including unfinished drawing.
    pub fn surface(&self, id: u32) -> Option<Surface> {
        let surface = self.surfaces.get(&id)?;
        let bytes = surface.pixels.read(0, surface.pixels.len()).ok()?;
        let mut rgba = Vec::with_capacity(surface.width as usize * surface.height as usize * 4);
        for row in bytes.chunks_exact(surface.stride) {
            for bgr in row[..surface.width as usize * 3].as_chunks::<3>().0 {
                rgba.extend_from_slice(&[bgr[2], bgr[1], bgr[0], 255]);
            }
        }
        Some(Surface {
            width: surface.width,
            height: surface.height,
            rgba,
        })
    }
    pub fn surface_memory(&self, id: u32) -> Option<SharedMemory> {
        self.surfaces.get(&id).map(|surface| surface.pixels.clone())
    }
    pub fn sound(&self, id: u32) -> Option<&Sound> {
        self.sounds.get(&id).map(Arc::as_ref)
    }
    pub fn sound_buffer(&self, id: u32) -> Option<Arc<Sound>> {
        self.sounds.get(&id).cloned()
    }
    pub fn audio_stream(&self, handle: u32) -> Option<&Arc<AudioStream>> {
        self.streams.get(&handle)
    }
    pub fn release_audio_stream(&mut self, handle: u32) -> Result<()> {
        if handle != 0 {
            let stream = self
                .streams
                .remove(&handle)
                .context("unknown audio stream handle")?;
            self.resident -= stream.sound.samples.len() * 2;
        }
        Ok(())
    }
    /// Open a memory-backed OGV stream. PCM is decoded under the shared budget;
    /// the eventual audio host can consume it without access to VM memory.
    pub fn create_audio_stream(&mut self, data: &[u8], flags: u32) -> Result<u32> {
        ensure!(
            matches!(flags, 0x21 | 0x29),
            "unsupported audio stream flags {flags:#x}"
        );
        ensure!(data.starts_with(b"OGV\0"), "audio stream requires OGV");
        ensure!(self.streams.len() < 256, "audio stream slots exhausted");
        let length = u32::from_le_bytes(
            data.get(8..12)
                .context("truncated OGV length")?
                .try_into()?,
        ) as usize;
        let ogg = audio::ogg_stream(data)?;
        let loop_bytes = ogg
            .get(length..length + 4)
            .context("missing OGV loop metadata")?;
        let loop_start_ms = u32::from_le_bytes(loop_bytes.try_into()?);
        // The supplied installation has zero loop metadata. Nonzero trailers
        // require decoder support and independent seek-point validation.
        ensure!(
            loop_start_ms == 0,
            "nonzero OGV loop metadata is unresolved"
        );
        let retained = self.resident;
        let mut samples = Vec::new();
        let mut first_block_samples = 0;
        let info = audio::decode(data, |packet| {
            if samples.is_empty() {
                first_block_samples = packet.len().min(2048);
            }
            let bytes = samples
                .len()
                .checked_add(packet.len())
                .and_then(|n| n.checked_mul(2))
                .context("audio stream size overflow")?;
            ensure!(
                bytes <= LIMIT - retained,
                "drawing and audio resources exceed 256 MiB"
            );
            samples.extend_from_slice(packet);
            Ok(())
        })?;
        ensure!(
            matches!(info.channels, 1 | 2),
            "unsupported audio stream channels"
        );
        let handle = (1..=256)
            .map(|id| 0x7000_0000 + id)
            .find(|id| !self.streams.contains_key(id))
            .context("audio stream slots exhausted")?;
        self.resident += samples.len() * 2;
        self.streams.insert(
            handle,
            Arc::new(AudioStream {
                sound: Sound {
                    sample_rate: info.sample_rate,
                    channels: info.channels,
                    samples,
                },
                loop_start_frame: 0,
                first_block_frames: first_block_samples / usize::from(info.channels),
                volume: StreamVolume::default(),
            }),
        );
        Ok(handle)
    }
    /// Decode only the requested frame. The caller owns the decoded pixels.
    pub fn frame(&self, id: u32, index: usize) -> Result<Frame> {
        match self.images.get(&id).context("image slot is not loaded")? {
            Image::Encoded(bytes) => {
                let frame = image::decode(bytes, index)?;
                Ok(Frame {
                    x: frame.info.x,
                    y: frame.info.y,
                    methods: frame.methods,
                    surface: Surface {
                        width: frame.info.width,
                        height: frame.info.height,
                        rgba: frame.rgba,
                    },
                })
            }
            Image::Mutable {
                frames,
                bytes_per_pixel,
            } => Ok(Frame {
                x: 0,
                y: 0,
                methods: vec![
                    if *bytes_per_pixel == 3 { 2 } else { 4 };
                    frames
                        .get(index)
                        .context("image frame out of bounds")?
                        .rgba
                        .len()
                        / 4
                ],
                surface: frames
                    .get(index)
                    .context("image frame out of bounds")?
                    .clone(),
            }),
        }
    }
    pub fn resident_bytes(&self) -> usize {
        self.resident
    }
    pub fn image_bounds(&self, id: u32, index: u32) -> Result<[i32; 4]> {
        let (x, y, width, height) =
            match self.images.get(&id).context("image slot is not loaded")? {
                Image::Encoded(bytes) => {
                    let frames = image::frames(bytes)?;
                    let info = frames
                        .iter()
                        .find(|f| f.index == index as usize)
                        .context("image frame not present")?;
                    (info.x, info.y, info.width, info.height)
                }
                Image::Mutable { frames, .. } => {
                    let surface = frames
                        .get(index as usize)
                        .context("image frame not present")?;
                    (0, 0, surface.width, surface.height)
                }
            };
        Ok([
            x,
            y,
            x.wrapping_add(width as i32),
            y.wrapping_add(height as i32),
        ])
    }
    fn hit_test_images(&self, x: i32, y: i32, items: &[ImageDraw]) -> Result<u32> {
        // Highest layer and latest entry win. Unlike painting, the native hit
        // tester includes disabled items and the maximum layer sentinel.
        let mut ordered: Vec<_> = items.iter().rev().collect();
        ordered.sort_by_key(|item| std::cmp::Reverse(item.layer));
        for item in ordered {
            let present = match self.images.get(&item.image) {
                None => false,
                Some(Image::Encoded(bytes)) => image::frames(bytes)?
                    .iter()
                    .any(|f| f.index == item.frame as usize),
                Some(Image::Mutable { frames, .. }) => (item.frame as usize) < frames.len(),
            };
            if !present {
                // The original aborts this search at a missing image/frame.
                return Ok(u32::MAX);
            }
            let [left, top, right, bottom] = self.image_bounds(item.image, item.frame)?;
            let left = left.wrapping_add(item.x);
            let top = top.wrapping_add(item.y);
            let right = right.wrapping_add(item.x);
            let bottom = bottom.wrapping_add(item.y);
            if x < left || x >= right || y < top || y >= bottom {
                continue;
            }
            let hit = match &self.images[&item.image] {
                Image::Encoded(bytes) => image::hit_test(
                    bytes,
                    item.frame as usize,
                    x.wrapping_sub(left) as u32,
                    y.wrapping_sub(top) as u32,
                )?,
                Image::Mutable { .. } => true,
            };
            if hit {
                return Ok(item.extra[0]);
            }
        }
        Ok(u32::MAX)
    }
    fn draw_images(&mut self, id: u32, items: &[ImageDraw]) -> Result<()> {
        let destination = self
            .surfaces
            .get(&id)
            .context("drawing surface is not allocated")?;
        let mut ordered: Vec<_> = items
            .iter()
            .filter(|item| item.flags & 0x8000_0000 != 0 && item.layer != u32::MAX)
            .collect();
        ordered.sort_by_key(|item| item.layer);
        // Work in a private copy so a malformed later item cannot partially draw.
        let mut pixels = destination.pixels.read(0, destination.pixels.len())?;
        for item in ordered {
            ensure!(
                (item.flags == 0x8000_0000 || item.flags & !0x1ff == 0xc000_0000)
                    && item.extra[1] == 0,
                "unsupported image drawing flags or extra operands"
            );
            let weight = if item.flags & 0x4000_0000 != 0 {
                (item.flags & 0x1ff).min(256) as i32
            } else {
                256
            };
            if weight == 0 {
                continue;
            }
            let exists = match self
                .images
                .get(&item.image)
                .context("draw image slot is not loaded")?
            {
                Image::Encoded(bytes) => image::frames(bytes)?
                    .iter()
                    .any(|f| f.index == item.frame as usize),
                Image::Mutable { frames, .. } => (item.frame as usize) < frames.len(),
            };
            if !exists {
                continue;
            }
            let frame = self.frame(item.image, item.frame as usize)?;
            let left = i64::from(item.x) + i64::from(frame.x);
            let top = i64::from(item.y) + i64::from(frame.y);
            let right = (left + i64::from(frame.surface.width)).min(i64::from(destination.width));
            let bottom = (top + i64::from(frame.surface.height)).min(i64::from(destination.height));
            for y in top.max(0)..bottom {
                for x in left.max(0)..right {
                    let src = ((y - top) as usize * frame.surface.width as usize
                        + (x - left) as usize)
                        * 4;
                    let rgba = &frame.surface.rgba[src..src + 4];
                    let method = frame.methods[src / 4];
                    if !matches!(method, 2..=5) {
                        continue;
                    }
                    let dst = y as usize * destination.stride + x as usize * 3;
                    // The original scalar lookup table rounds to nearest,
                    // with exact halves rounded down (4435a4).
                    let alpha = (i32::from(rgba[3]) * weight + 127) >> 8;
                    for channel in 0..3 {
                        let source = rgba[2 - channel];
                        let old = pixels[dst + channel];
                        pixels[dst + channel] = if method <= 3 && weight < 256 {
                            (((i32::from(source) * weight + 127) >> 8)
                                + ((i32::from(old) * (256 - weight) + 127) >> 8))
                                as u8
                        } else if alpha == 255 && method != 5 {
                            source
                        } else {
                            (i32::from(old) + (((i32::from(source) - i32::from(old)) * alpha) >> 8))
                                as u8
                        };
                    }
                }
            }
        }
        destination.pixels.write(0, &pixels)
    }
    fn mask_transition(&mut self, transition: &MaskTransition) -> Result<()> {
        let flags = transition.flags;
        let weight = flags & 0x3fff_ffff;
        ensure!(
            weight <= 512,
            "unsupported mask transition mode or weight {flags:#x}"
        );
        let destination = self
            .surfaces
            .get(&transition.destination)
            .context("mask transition destination is not allocated")?;
        // 437af0 advances packed BGR triples for width * height pixels;
        // it does not skip row padding even when the stride has padding.
        let length = destination.width as usize * destination.height as usize * 3;
        let read = |id: u32| -> Result<Vec<u8>> {
            self.surfaces
                .get(&id)
                .context("mask transition source is not allocated")?
                .pixels
                .read(0, length)
        };
        let first = read(transition.first)?;
        let second = transition.second.map(read).transpose()?;
        let mask = read(transition.mask)?;
        let mut result = vec![0; length];
        let threshold = 256u32.wrapping_sub(weight);
        for (index, pixel) in result.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let offset = index * 3;
            let blue = mask[offset] ^ if flags & 0x8000_0000 != 0 { 255 } else { 0 };
            if flags & 0x4000_0000 == 0 {
                // Scalar 437af0 doubles the progress only for the inverted
                // black-fallback path; two-source transitions use it directly.
                let progress = if second.is_none() && flags & 0x8000_0000 != 0 {
                    weight * 2
                } else {
                    weight
                };
                let alpha = (i32::from(blue) + progress as i32 - 256).clamp(0, 256) as u32;
                for channel in 0..3 {
                    let other = second
                        .as_ref()
                        .map_or(0, |s| u32::from(s[offset + channel]));
                    pixel[channel] = ((u32::from(first[offset + channel]) * alpha
                        + other * (256 - alpha))
                        >> 8) as u8;
                }
            } else if u32::from(blue) >= threshold {
                pixel.copy_from_slice(&first[offset..offset + 3]);
            } else if let Some(second) = &second {
                pixel.copy_from_slice(&second[offset..offset + 3]);
            }
        }
        destination.pixels.write(0, &result)
    }
    fn copy_surface(&mut self, copy: &SurfaceCopy) -> Result<()> {
        let destination = self
            .surfaces
            .get(&copy.destination.id)
            .context("copy destination is not allocated")?;
        let [width, height] = copy.size;
        ensure!(width >= 0 && height >= 0, "negative copy dimensions");
        let right = copy
            .destination
            .x
            .checked_add(width)
            .context("copy rectangle overflow")?;
        let bottom = copy
            .destination
            .y
            .checked_add(height)
            .context("copy rectangle overflow")?;
        let left = copy.destination.x.max(0).min(destination.width as i32);
        let top = copy.destination.y.max(0).min(destination.height as i32);
        let right = right.max(0).min(destination.width as i32);
        let bottom = bottom.max(0).min(destination.height as i32);
        if right <= left || bottom <= top {
            return Ok(());
        }
        let source = self
            .surfaces
            .get(&copy.source.id)
            .context("copy source is not allocated")?;
        let width = (right - left) as usize;
        let height = (bottom - top) as usize;
        let sx = i64::from(copy.source.x) + i64::from(left) - i64::from(copy.destination.x);
        let sy = i64::from(copy.source.y) + i64::from(top) - i64::from(copy.destination.y);
        ensure!(
            sx >= 0
                && sy >= 0
                && sx + width as i64 <= i64::from(source.width)
                && sy + height as i64 <= i64::from(source.height),
            "copy source rectangle outside surface"
        );
        // Native 417900 calls memmove separately for each row, top to bottom.
        // A downward overlapping copy therefore reads rows written earlier.
        for row in 0..height {
            let bytes = source.pixels.read(
                (sy as usize + row) * source.stride + sx as usize * 3,
                width * 3,
            )?;
            destination.pixels.write(
                (top as usize + row) * destination.stride + left as usize * 3,
                &bytes,
            )?;
        }
        Ok(())
    }
    fn blend_surfaces(&mut self, blend: &SurfaceBlend) -> Result<()> {
        let destination = self
            .surfaces
            .get(&blend.destination.id)
            .context("blend destination is not allocated")?;
        let [width, height] = blend.size;
        ensure!(width >= 0 && height >= 0, "negative blend dimensions");
        let [a, b] = blend.weights;
        ensure!(a <= 65535 && b <= 65535, "unsupported blend weights");
        let right = blend
            .destination
            .x
            .checked_add(width)
            .context("blend rectangle overflow")?;
        let bottom = blend
            .destination
            .y
            .checked_add(height)
            .context("blend rectangle overflow")?;
        let left = blend.destination.x.max(0).min(destination.width as i32);
        let top = blend.destination.y.max(0).min(destination.height as i32);
        let right = right.max(0).min(destination.width as i32);
        let bottom = bottom.max(0).min(destination.height as i32);
        if right <= left || bottom <= top {
            return Ok(());
        }
        let width = (right - left) as usize;
        let height = (bottom - top) as usize;
        let mut sources = Vec::with_capacity(2);
        for point in &blend.sources {
            let source = self
                .surfaces
                .get(&point.id)
                .context("blend source is not allocated")?;
            // The native helper uses the destination's row stride for all inputs.
            ensure!(
                source.stride == destination.stride,
                "unsupported differing blend strides"
            );
            let x = i64::from(point.x) + i64::from(left) - i64::from(blend.destination.x);
            let y = i64::from(point.y) + i64::from(top) - i64::from(blend.destination.y);
            ensure!(
                x >= 0
                    && y >= 0
                    && x + width as i64 <= i64::from(source.width)
                    && y + height as i64 <= i64::from(source.height),
                "blend source rectangle outside surface"
            );
            ensure!(
                point.id != blend.destination.id || (x == i64::from(left) && y == i64::from(top)),
                "unsupported overlapping blend rectangles"
            );
            sources.push((source, x as usize, y as usize));
        }
        let total = a + b;
        if total == 0 {
            return Ok(());
        }
        let bias = (total - 1) / 2;
        for row in 0..height {
            let read = |(surface, x, y): &(&DrawingSurface, usize, usize)| {
                surface
                    .pixels
                    .read((y + row) * surface.stride + x * 3, width * 3)
            };
            let first = read(&sources[0])?;
            let second = read(&sources[1])?;
            let pixels: Vec<_> = first
                .iter()
                .zip(second)
                .map(|(&x, y)| {
                    (((u32::from(x) * a + bias) / total) + ((u32::from(y) * b + bias) / total))
                        as u8
                })
                .collect();
            destination.pixels.write(
                (top as usize + row) * destination.stride + left as usize * 3,
                &pixels,
            )?;
        }
        Ok(())
    }
    fn fill_surface(
        &mut self,
        id: u32,
        [x, y, width, height]: [i32; 4],
        color: [u8; 3],
    ) -> Result<()> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .context("drawing surface is not allocated")?;
        // The x86 handler computes signed right/bottom with wrapping addition,
        // clips to the DIB, then GDI fills with exclusive right/bottom edges.
        let left = x.max(0).min(surface.width as i32) as usize;
        let top = y.max(0).min(surface.height as i32) as usize;
        let right = x.wrapping_add(width).max(0).min(surface.width as i32) as usize;
        let bottom = y.wrapping_add(height).max(0).min(surface.height as i32) as usize;
        if right <= left || bottom <= top {
            return Ok(());
        }
        let row_bytes: Vec<u8> = [color[2], color[1], color[0]].repeat(right - left);
        for row in top..bottom {
            surface
                .pixels
                .write(row * surface.stride + left * 3, &row_bytes)?;
        }
        Ok(())
    }
    fn capture_surface(&mut self, capture: &SurfaceCapture) -> Result<()> {
        let [width, height] = capture.size.map(i64::from);
        ensure!(
            [width, height].iter().all(|v| (0..=16384).contains(v)),
            "invalid capture dimensions"
        );
        let src = self
            .surfaces
            .get(&capture.source.id)
            .context("missing capture source")?;
        let Some(Image::Mutable { frames, .. }) = self.images.get_mut(&capture.image) else {
            anyhow::bail!("capture destination must be a mutable image");
        };
        let dst = frames
            .get_mut(capture.frame as usize)
            .context("missing capture frame")?;
        let source = src.pixels.read(0, src.pixels.len())?;
        let [dx, dy] = capture.destination.map(i64::from);
        let sx = i64::from(capture.source.x);
        let sy = i64::from(capture.source.y);
        for y in 0.max(-dy).max(-sy)
            ..height
                .min(i64::from(dst.height) - dy)
                .min(i64::from(src.height) - sy)
        {
            for x in 0.max(-dx).max(-sx)
                ..width
                    .min(i64::from(dst.width) - dx)
                    .min(i64::from(src.width) - sx)
            {
                let read = (sy + y) as usize * src.stride + (sx + x) as usize * 3;
                let write = ((dy + y) as usize * dst.width as usize + (dx + x) as usize) * 4;
                dst.rgba[write..write + 4].copy_from_slice(&[
                    source[read + 2],
                    source[read + 1],
                    source[read],
                    255,
                ]);
            }
        }
        Ok(())
    }

    fn stretch_surface(&mut self, stretch: &SurfaceStretch) -> Result<()> {
        ensure!(
            stretch.mode == 0xcc0020 || stretch.mode & 0x80000000 != 0,
            "unsupported stretch raster operation {:#x}",
            stretch.mode
        );
        let [dw, dh] = stretch.destination_size.map(i64::from);
        let [sw, sh] = stretch.source_size.map(i64::from);
        ensure!(
            [dw, dh, sw, sh].iter().all(|v| (0..=16384).contains(v)),
            "mirrored or oversized stretch rectangle"
        );
        if dw == 0 || dh == 0 || sw == 0 || sh == 0 {
            return Ok(());
        }
        let dst = self
            .surfaces
            .get(&stretch.destination.id)
            .context("missing stretch destination")?;
        let src = self
            .surfaces
            .get(&stretch.source.id)
            .context("missing stretch source")?;
        let source = src.pixels.read(0, src.pixels.len())?;
        let mut pixels = dst.pixels.read(0, dst.pixels.len())?;
        let dx = i64::from(stretch.destination.x);
        let dy = i64::from(stretch.destination.y);
        for y in dy.max(0)..(dy + dh).min(i64::from(dst.height)) {
            let sy = i64::from(stretch.source.y) + (y - dy) * sh / dh;
            if sy < 0 || sy >= i64::from(src.height) {
                continue;
            }
            for x in dx.max(0)..(dx + dw).min(i64::from(dst.width)) {
                let sx = i64::from(stretch.source.x) + (x - dx) * sw / dw;
                if sx < 0 || sx >= i64::from(src.width) {
                    continue;
                }
                let read = sy as usize * src.stride + sx as usize * 3;
                let write = y as usize * dst.stride + x as usize * 3;
                pixels[write..write + 3].copy_from_slice(&source[read..read + 3]);
            }
        }
        dst.pixels.write(0, &pixels)
    }

    pub fn create_surface(&mut self, id: u32, width: u32, height: u32, flags: u32) -> Result<()> {
        ensure!(
            id < 256 && width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "invalid drawing surface dimensions or slot"
        );
        // DirectDraw memory placement flags select the same portable, CPU-accessible
        // surface. Presentation uploads this storage through the native renderer.
        ensure!(
            matches!(flags, 0 | 0x80000000 | 0xc0000000),
            "unresolved surface allocation flags {flags:#x}"
        );
        let stride = (width as usize * 3 + 3) & !3;
        let size = stride * height as usize;
        let old = self
            .surfaces
            .get(&id)
            .map_or(0, |surface| surface.pixels.len());
        let resident = self.resident - old + size;
        ensure!(resident <= LIMIT, "drawing resources exceed 256 MiB");
        let pixels = SharedMemory::zeroed(size)?;
        self.surfaces.insert(
            id,
            DrawingSurface {
                width,
                height,
                stride,
                pixels,
            },
        );
        self.resident = resident;
        Ok(())
    }
    fn release_image(&mut self, id: u32) -> Result<()> {
        ensure!(id <= 256, "image release slot out of bounds");
        if let Some(image) = self.images.remove(&id) {
            self.resident -= image.size();
        }
        Ok(())
    }
    fn release_surface(&mut self, id: u32) -> Result<()> {
        ensure!(id < 256, "surface release slot out of bounds");
        if let Some(surface) = self.surfaces.remove(&id) {
            self.resident -= surface.pixels.len();
        }
        Ok(())
    }
    fn load_image(&mut self, id: u32, data: Vec<u8>) -> Result<()> {
        ensure!(id < 256, "image slot out of bounds");
        image::frames(&data)?;
        let old = self.images.get(&id).map_or(0, Image::size);
        let resident = self.resident - old + data.len();
        ensure!(resident <= LIMIT, "drawing resources exceed 256 MiB");
        self.images.insert(id, Image::Encoded(data));
        self.resident = resident;
        Ok(())
    }
    fn load_sound(&mut self, id: u32, data: &[u8], flags: u32) -> Result<()> {
        ensure!(id < 256, "sound slot out of bounds");
        // Both foreground-only (Wana) and global-focus buffers use the same
        // decoded PCM. Background input/audio policy belongs to the host.
        ensure!(
            matches!(flags, 0 | 0x8000),
            "unresolved sound creation flags {flags:#x}"
        );
        let old = self
            .sounds
            .get(&id)
            .map_or(0, |sound| sound.samples.len() * 2);
        let retained = self.resident - old;
        let mut samples = Vec::new();
        let info = audio::decode(data, |packet| {
            let bytes = samples
                .len()
                .checked_add(packet.len())
                .and_then(|n| n.checked_mul(2))
                .context("audio buffer size overflow")?;
            ensure!(
                bytes <= LIMIT - retained,
                "drawing and audio resources exceed 256 MiB"
            );
            samples.extend_from_slice(packet);
            Ok(())
        })?;
        ensure!(
            matches!(info.channels, 1 | 2),
            "unsupported sound channel count"
        );
        self.resident = retained + samples.len() * 2;
        self.sounds.insert(
            id,
            Arc::new(Sound {
                sample_rate: info.sample_rate,
                channels: info.channels,
                samples,
            }),
        );
        Ok(())
    }
    fn create_image(
        &mut self,
        id: u32,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        count: u32,
    ) -> Result<()> {
        ensure!(
            id < 256 && width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "invalid image dimensions or slot"
        );
        ensure!(
            matches!(bytes_per_pixel, 3 | 4),
            "unsupported mutable image pixel size"
        );
        ensure!(
            count > 0 && count <= 65536,
            "invalid mutable image frame count"
        );
        let size = usize::try_from(u64::from(width) * u64::from(height) * 4 * u64::from(count))?;
        let resident = self
            .resident
            .checked_sub(self.images.get(&id).map_or(0, Image::size))
            .and_then(|value| value.checked_add(size))
            .context("image budget overflow")?;
        ensure!(resident <= LIMIT, "drawing resources exceed 256 MiB");
        let mut frames = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut rgba = vec![0; size / count as usize];
            if bytes_per_pixel == 3 {
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
            frames.push(Surface {
                width,
                height,
                rgba,
            });
        }
        self.images.insert(
            id,
            Image::Mutable {
                bytes_per_pixel,
                frames,
            },
        );
        self.resident = resident;
        Ok(())
    }
    fn fill_image(&mut self, id: u32, frame: u32, rect: [u32; 4], color: u32) -> Result<()> {
        let Image::Mutable {
            bytes_per_pixel,
            frames,
        } = self
            .images
            .get_mut(&id)
            .context("image slot is not loaded")?
        else {
            anyhow::bail!("rectangle fill requires a mutable image");
        };
        let frame = frames
            .get_mut(frame as usize)
            .context("image frame out of bounds")?;
        let [x, y, width, height] = rect;
        let right = x.checked_add(width).context("image rectangle overflow")?;
        let bottom = y.checked_add(height).context("image rectangle overflow")?;
        ensure!(
            right <= frame.width && bottom <= frame.height,
            "image rectangle out of bounds"
        );
        let [a, b, g, r] = color.to_le_bytes();
        // Preserve the original helper's unusual three-byte conversion.
        // See the original-code snapshots in docs/validation/image-probe.json.
        let pixel = if *bytes_per_pixel == 4 {
            [r, g, b, a]
        } else {
            [b, r, g, 255]
        };
        for row in y..bottom {
            let begin = (row as usize * frame.width as usize + x as usize) * 4;
            for dst in frame.rgba[begin..begin + width as usize * 4]
                .as_chunks_mut::<4>()
                .0
            {
                *dst = pixel;
            }
        }
        Ok(())
    }
    /// A reply here means actual asset I/O or buffer allocation completed.
    pub fn respond(&mut self, project: &Project, request: &PlatformRequest) -> Result<Option<u32>> {
        if let PlatformRequest::GetAudioStreamVolume { handle } = request {
            return Ok(Some(if *handle == 0 {
                u32::MAX
            } else {
                self.streams
                    .get(handle)
                    .context("unknown audio stream in volume query")?
                    .volume
                    .percent()
            }));
        }
        match request {
            PlatformRequest::DrawImageGlyph {
                image,
                frame,
                position,
                character,
                style,
            } => {
                let Some(Image::Mutable { frames, .. }) = self.images.get_mut(image) else {
                    anyhow::bail!("image text target must be mutable");
                };
                let target = frames
                    .get_mut(*frame as usize)
                    .context("image text frame is missing")?;
                let pixels = SharedMemory::zeroed(target.rgba.len())?;
                pixels.write(0, &target.rgba)?;
                self.text_renderer.draw(
                    crate::text_render::Canvas {
                        pixels: &pixels,
                        size: [target.width, target.height],
                        stride: target.width as usize * 4,
                        rgba: true,
                    },
                    *position,
                    *character,
                    style,
                )?;
                target.rgba = pixels.read(0, pixels.len())?;
            }
            PlatformRequest::DrawGlyph {
                surface,
                position,
                character,
                style,
            } => {
                let target = self
                    .surfaces
                    .get(surface)
                    .context("text surface is not allocated")?;
                let damage = self.text_renderer.draw(
                    crate::text_render::Canvas {
                        rgba: false,
                        pixels: &target.pixels,
                        size: [target.width, target.height],
                        stride: target.stride,
                    },
                    *position,
                    *character,
                    style,
                )?;
                if *surface == 0
                    && let Some(rect) = damage
                {
                    self.text_damage.push(rect);
                }
            }
            PlatformRequest::HitTestImages { x, y, items } => {
                return Ok(Some(self.hit_test_images(*x, *y, items)?));
            }
            PlatformRequest::DrawImages { id, items } => self.draw_images(*id, items)?,
            PlatformRequest::BlendSurfaces(blend) => self.blend_surfaces(blend)?,
            PlatformRequest::CopySurface(copy) => self.copy_surface(copy)?,
            PlatformRequest::PixelateSurface { copy, block_size } => {
                self.pixelate_surface(copy, *block_size)?
            }
            PlatformRequest::AffineImage(transform) => self.affine_image(transform)?,
            PlatformRequest::StretchSurface(stretch) => self.stretch_surface(stretch)?,
            PlatformRequest::CaptureSurface(capture) => self.capture_surface(capture)?,
            PlatformRequest::MaskTransition(transition) => self.mask_transition(transition)?,
            PlatformRequest::FillSurface { id, rect, color } => {
                self.fill_surface(*id, *rect, *color)?
            }
            PlatformRequest::LoadSound { id, name, flags } => {
                let bytes = project.read_with_archives(name, &self.archives)?;
                self.load_sound(*id, &bytes, *flags)?;
            }
            PlatformRequest::CreateImage {
                id,
                width,
                height,
                bytes_per_pixel,
                frames,
            } => self.create_image(*id, *width, *height, *bytes_per_pixel, *frames)?,
            PlatformRequest::FillImage {
                id,
                frame,
                x,
                y,
                width,
                height,
                color,
            } => self.fill_image(*id, *frame, [*x, *y, *width, *height], *color)?,
            PlatformRequest::CreateSurface {
                id,
                width,
                height,
                flags,
            } => self.create_surface(*id, *width, *height, *flags)?,
            PlatformRequest::ReleaseImage { id } => self.release_image(*id)?,
            PlatformRequest::ReleaseSurface { id } => self.release_surface(*id)?,
            PlatformRequest::LoadImage { id, name } => {
                let bytes = project.read_with_archives(name, &self.archives)?;
                self.load_image(*id, bytes)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_clips_both_rectangles_and_converts_padded_bgr_to_rgba() {
        use shiinario_scenario::SurfacePoint;
        let mut resources = Resources::default();
        resources.create_surface(2, 2, 2, 0).unwrap();
        resources.surfaces[&2]
            .pixels
            .write(0, &[1, 2, 3, 4, 5, 6, 99, 99, 7, 8, 9, 10, 11, 12, 99, 99])
            .unwrap();
        resources.create_image(30, 2, 2, 4, 1).unwrap();
        let operation = SurfaceCapture {
            image: 30,
            frame: 0,
            destination: [-1, 0],
            size: [3, 3],
            source: SurfacePoint { id: 2, x: 0, y: -1 },
        };
        resources.capture_surface(&operation).unwrap();
        let Image::Mutable { frames, .. } = &resources.images[&30] else {
            panic!()
        };
        assert_eq!(&frames[0].rgba[8..12], &[6, 5, 4, 255]);
        assert_eq!(&frames[0].rgba[..8], &[0; 8]);
        assert_eq!(&frames[0].rgba[12..], &[0; 4]);
    }
    #[test]
    fn stretch_clips_destination_and_preserves_padding_and_failed_draws() {
        use shiinario_scenario::SurfacePoint;
        let mut resources = Resources::default();
        resources.create_surface(1, 2, 1, 0).unwrap();
        resources.create_surface(0, 3, 2, 0).unwrap();
        resources.surfaces[&1]
            .pixels
            .write(0, &[1, 2, 3, 4, 5, 6, 99, 99])
            .unwrap();
        let mut operation = SurfaceStretch {
            destination: SurfacePoint { id: 0, x: -1, y: 0 },
            destination_size: [4, 2],
            source: SurfacePoint { id: 1, x: 0, y: 0 },
            source_size: [2, 1],
            mode: 0xcc0020,
        };
        resources.stretch_surface(&operation).unwrap();
        let expected = [1, 2, 3, 4, 5, 6, 4, 5, 6, 0, 0, 0].repeat(2);
        assert_eq!(resources.surfaces[&0].pixels.read(0, 24).unwrap(), expected);
        operation.mode = 0;
        assert!(resources.stretch_surface(&operation).is_err());
        assert_eq!(resources.surfaces[&0].pixels.read(0, 24).unwrap(), expected);
    }
    #[test]
    fn malformed_mutable_image_operations_preserve_existing_pixels_and_budget() {
        let mut resources = Resources::default();
        resources.create_image(1, 3, 2, 4, 2).unwrap();
        resources
            .fill_image(1, 1, [0, 0, 3, 2], 0x80112233)
            .unwrap();
        let previous = resources.frame(1, 1).unwrap().surface;
        for rect in [[0, 0, 4, 2], [0, 1, 3, 2], [u32::MAX, 0, 2, 2]] {
            assert!(resources.fill_image(1, 1, rect, 0).is_err());
            assert_eq!(resources.frame(1, 1).unwrap().surface, previous);
        }
        assert!(resources.fill_image(1, 2, [0, 0, 1, 1], 0).is_err());
        assert!(resources.create_image(1, 16384, 16384, 4, 65536).is_err());
        assert!(resources.create_image(1, 2, 2, 2, 1).is_err());
        assert_eq!(resources.resident_bytes(), 48);
        assert_eq!(resources.frame(1, 1).unwrap().surface, previous);
        resources.create_image(1, 1, 1, 3, 1).unwrap();
        assert_eq!(resources.resident_bytes(), 4);
        assert_eq!(resources.frame(1, 0).unwrap().surface.rgba, [0, 0, 0, 255]);
        assert!(resources.frame(1, 1).is_err());
    }
    #[test]
    fn display_retains_text_outside_invalidated_rect_and_unfinished_drawing() {
        let mut resources = Resources::default();
        resources.create_surface(0, 3, 2, 0).unwrap();
        resources.fill_surface(0, [0, 0, 3, 2], [255; 3]).unwrap();
        resources.commit_display([0, 0, 3, 2]);
        let completed = resources.display().unwrap().clone();

        // A new composition clears the working DIB before redrawing text.
        // Window expose/resize must keep the previously submitted pixels.
        resources.fill_surface(0, [0, 0, 3, 2], [0; 3]).unwrap();
        assert_eq!(resources.display(), Some(&completed));
        // Updating an animated cursor must not erase text outside its RECT.
        resources
            .fill_surface(0, [1, 1, 1, 1], [12, 34, 56])
            .unwrap();
        resources.commit_display([1, 1, 2, 2]);
        let mut expected = completed;
        expected.rgba[16..20].copy_from_slice(&[12, 34, 56, 255]);
        assert_eq!(resources.display(), Some(&expected));
        for rect in [[2, 1, 1, 2], [0, 0, 0, 2], [-3, -2, -1, -1]] {
            resources.commit_display(rect);
            assert_eq!(resources.display(), Some(&expected));
        }
        resources.commit_display([-10, -10, i32::MAX, i32::MAX]);
        assert_eq!(resources.display(), resources.surface(0).as_ref());
    }
    #[test]
    fn surface_replacement_preserves_budget_and_failed_replacement_keeps_pixels() {
        let mut resources = Resources::default();
        resources.create_surface(2, 800, 600, 0).unwrap();
        assert_eq!(resources.resident_bytes(), 800 * 600 * 3);
        assert_eq!(&resources.surface(2).unwrap().rgba[..4], &[0, 0, 0, 255]);
        assert!(resources.create_surface(2, 16384, 16384, 0).is_err());
        assert_eq!(resources.surface(2).unwrap().width, 800);
        resources.create_surface(2, 2, 3, 0).unwrap();
        assert_eq!(resources.resident_bytes(), 24);
        assert!(resources.create_surface(256, 2, 3, 0).is_err());
    }
    #[test]
    fn shared_bgr_surface_observes_script_writes_and_preserves_row_padding() {
        let mut resources = Resources::default();
        resources.create_surface(2, 1, 2, 0).unwrap();
        let pixels = resources.surface_memory(2).unwrap();
        assert_eq!(pixels.len(), 8);
        pixels.write(0, &[3, 2, 1, 99, 6, 5, 4, 88]).unwrap();
        assert_eq!(
            resources.surface(2).unwrap().rgba,
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
        resources
            .fill_surface(2, [0, 1, 1, 1], [10, 20, 30])
            .unwrap();
        assert_eq!(pixels.read(0, 8).unwrap(), [3, 2, 1, 99, 30, 20, 10, 88]);
        assert!(resources.create_surface(2, 1, 2, 1).is_err());
        assert!(pixels.same_region(&resources.surface_memory(2).unwrap()));
        resources.create_surface(2, 1, 2, 0).unwrap();
        assert!(!pixels.same_region(&resources.surface_memory(2).unwrap()));
        assert_eq!(
            resources.surface(2).unwrap().rgba,
            [0, 0, 0, 255, 0, 0, 0, 255]
        );
    }
    #[test]
    fn invalid_image_does_not_replace_an_existing_slot() {
        let mut data = b"S25\0".to_vec();
        data.extend(0u32.to_le_bytes());
        let mut resources = Resources::default();
        resources.load_image(10, data.clone()).unwrap();
        assert!(resources.load_image(10, vec![1, 2, 3]).is_err());
        assert!(matches!(&resources.images[&10], Image::Encoded(bytes) if bytes == &data));
        assert_eq!(resources.resident_bytes(), 8);
        assert!(resources.frame(10, 0).is_err());
    }
}
