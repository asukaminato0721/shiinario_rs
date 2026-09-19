//! Window-independent image slots and drawing buffers used by binary SCN.
use anyhow::{Context, Result, ensure};
use shiinario_assets::{audio, image, project::Project};
use shiinario_scenario::{
    ImageDraw, MaskTransition, PlatformRequest, SharedMemory, SurfaceBlend, SurfaceCapture,
    SurfaceCopy, SurfaceStretch,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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
#[derive(Default)]
pub struct Resources {
    text_renderer: crate::text_render::TextRenderer,
    surfaces: BTreeMap<u32, DrawingSurface>,
    images: BTreeMap<u32, Image>,
    sounds: BTreeMap<u32, Arc<Sound>>,
    streams: BTreeMap<u32, Arc<AudioStream>>,
    resident: usize,
    archives: Vec<String>,
}
impl Resources {
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
        Ok(resources)
    }
    pub fn asset_sizes(&self, project: &Project, name: &str) -> Result<[u32; 2]> {
        project.sizes_with_archives(name, &self.archives)
    }
    pub fn read_asset(&self, project: &Project, name: &str) -> Result<Vec<u8>> {
        project.read_with_archives(name, &self.archives)
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
    /// Snapshot the top-down BGR24 DIB for an RGBA presentation host.
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

    fn create_surface(&mut self, id: u32, width: u32, height: u32, flags: u32) -> Result<()> {
        ensure!(
            id < 256 && width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "invalid drawing surface dimensions or slot"
        );
        // The VM authorizes these DirectDraw modes only in best-effort mode;
        // Session logs their software fallback before allocating BGR24 storage.
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
        // The traced game requests software mixing (DSBCAPS_LOCSOFTWARE).
        // Other device capability flags need separate host semantics.
        ensure!(
            flags == 0x8000,
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
                self.text_renderer.draw(
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
            }
            PlatformRequest::HitTestImages { x, y, items } => {
                return Ok(Some(self.hit_test_images(*x, *y, items)?));
            }
            PlatformRequest::DrawImages { id, items } => self.draw_images(*id, items)?,
            PlatformRequest::BlendSurfaces(blend) => self.blend_surfaces(blend)?,
            PlatformRequest::CopySurface(copy)
            | PlatformRequest::UnfilteredPixelation { copy, .. } => self.copy_surface(copy)?,
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
    fn surface_release_matches_original_and_reclaims_budget() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/surface-release-probe.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let id = case["slot"].as_u64().unwrap() as u32;
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("release.scn", code).unwrap();
            let mut resources = Resources::default();
            resources.create_surface(17, 8, 4, 0).unwrap();
            if case["present"].as_bool().unwrap() {
                resources.create_surface(id, 8, 4, 0).unwrap();
            }
            for state in case["states"].as_array().unwrap() {
                assert!(matches!(vm.step().unwrap(), Event::Platform {
                    request: PlatformRequest::ReleaseSurface { id: actual }, ..
                } if actual == id));
                resources.release_surface(id).unwrap();
                vm.respond(1).unwrap();
                assert_eq!(
                    vm.location().offset as u64,
                    state["offset"].as_u64().unwrap()
                );
                assert!(resources.surface_memory(id).is_none());
                assert!(resources.surface(17).is_some());
                assert_eq!(resources.resident, 96);
            }
            for _ in 0..300 {
                resources.create_surface(id, 8, 4, 0).unwrap();
                resources.release_surface(id).unwrap();
                assert_eq!(resources.resident, 96);
            }
            assert!(resources.release_surface(256).is_err());
            assert_eq!(resources.resident, 96);
        }
    }
    #[test]
    fn image_release_matches_native_and_reclaims_budget() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/image-release-probe.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let id = case["slot"].as_u64().unwrap() as u32;
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("release.scn", code).unwrap();
            let mut resources = Resources::default();
            resources.create_image(17, 2, 2, 4, 1).unwrap();
            if id > 256 {
                assert!(vm.step().is_err());
                assert!(resources.release_image(id).is_err());
                assert_eq!(resources.resident, 16);
                continue;
            }
            if case["present"].as_bool().unwrap() {
                // Insert synthetic storage directly to cover native's inclusive slot 256.
                resources.images.insert(id, Image::Encoded(vec![0; 32]));
                resources.resident += 32;
            }
            let event = vm.step().unwrap();
            assert!(
                matches!(&event, Event::Platform {request:PlatformRequest::ReleaseImage {id:actual},..} if *actual==id)
            );
            assert_eq!(vm.step().unwrap(), event);
            resources.release_image(id).unwrap();
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
            assert!(!resources.images.contains_key(&id));
            assert_eq!(resources.resident, 16);
            assert!(resources.frame(17, 0).is_ok());
            resources.release_image(id).unwrap();
            assert_eq!(resources.resident, 16);
        }
    }
    #[test]
    fn hit_testing_matches_original_rle_methods_layers_and_disabled_items() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/hit-test-probe.json"))
                .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut resources = Resources::default();
            resources.load_image(30, decode(&fixture["s25"])).unwrap();
            let mut vm = BinaryVm::new("hit.scn", decode(&case["code"])).unwrap();
            loop {
                if let Event::Platform {
                    request: PlatformRequest::HitTestImages { x, y, items },
                    ..
                } = vm.step().unwrap()
                {
                    let hit = resources.hit_test_images(x, y, &items).unwrap();
                    assert_eq!(serde_json::json!(hit), case["result"]);
                    vm.respond(hit).unwrap();
                    assert_eq!(
                        vm.location().offset,
                        case["next_offset"].as_u64().unwrap() as usize
                    );
                    break;
                }
            }
        }
    }
    #[test]
    fn image_bounds_match_original_signed_offsets_and_missing_frames() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/image-bounds-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut resources = Resources::default();
            resources.load_image(30, decode(&case["s25"])).unwrap();
            let index = case["index"].as_u64().unwrap() as u32;
            let bounds = resources.image_bounds(30, index);
            if case["status"] == 2 {
                assert!(bounds.is_err());
                continue;
            }
            let bounds = bounds.unwrap();
            assert_eq!(serde_json::json!(bounds), case["bounds"]);
            let mut code = decode(&case["code"]);
            for index in 0..4u16 {
                code.extend([0x9d, 4, 12]);
                code.extend(index.to_le_bytes());
            }
            let mut vm = BinaryVm::new("bounds.scn", code).unwrap();
            assert!(vm.respond_image_bounds(bounds).is_err());
            let event = vm.step().unwrap();
            assert!(matches!(
                event,
                Event::Platform {
                    request: PlatformRequest::ImageBounds { id: 30, frame: 0 },
                    ..
                }
            ));
            assert!(vm.respond(1).is_err());
            assert_eq!(vm.step().unwrap(), event);
            vm.respond_image_bounds(bounds).unwrap();
            assert_eq!(
                vm.location().offset,
                case["next_offset"].as_u64().unwrap() as usize
            );
            assert!(vm.respond_image_bounds(bounds).is_err());
            for edge in bounds {
                assert!(
                    matches!(vm.step().unwrap(), Event::MouseButtonMapping { value, .. } if value == edge as u32)
                );
            }
        }
    }
    #[test]
    fn draw_list_matches_original_s25_alpha_clipping_and_layer_order() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/draw-list-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut vm = BinaryVm::new("draw.scn", decode(&case["code"])).unwrap();
            let mut resources = Resources::default();
            resources.create_surface(3, 8, 4, 0).unwrap();
            resources.load_image(30, decode(&case["s25"])).unwrap();
            let memory = resources.surface_memory(3).unwrap();
            memory.write(0, &decode(&case["initial"])).unwrap();
            for state in case["states"].as_array().unwrap() {
                if let Event::Platform {
                    request: PlatformRequest::DrawImages { id, items },
                    ..
                } = vm.step().unwrap()
                {
                    let native_items: Vec<_> = items
                        .iter()
                        .map(|i| {
                            [
                                i.image, i.frame, i.flags, i.layer, i.x as u32, i.y as u32,
                                i.extra[0], i.extra[1],
                            ]
                        })
                        .collect();
                    assert_eq!(serde_json::json!(native_items), state["items"]);
                    resources.draw_images(id, &items).unwrap();
                    vm.respond(1).unwrap();
                    // A later invalid item must not leave an earlier item drawn.
                    let before = memory.read(0, memory.len()).unwrap();
                    let mut bad = items.clone();
                    let mut last = items[0].clone();
                    last.flags = 0x8000_0001;
                    last.layer = u32::MAX - 1;
                    bad.push(last);
                    assert!(resources.draw_images(id, &bad).is_err());
                    assert_eq!(memory.read(0, memory.len()).unwrap(), before);
                }
                assert_eq!(
                    vm.location().offset,
                    state["offset"].as_u64().unwrap() as usize
                );
                assert_eq!(
                    memory.read(0, memory.len()).unwrap(),
                    decode(&state["pixels"]),
                    "code {} s25 {}",
                    case["code"],
                    case["s25"]
                );
            }
        }
    }
    #[test]
    fn audio_stream_creation_matches_original_and_preserves_budget_on_failure() {
        use shiinario_scenario::{BinaryVm, Event};
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/stream-synthetic-probe.json"
        ))
        .unwrap();
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
        let mut code = vec![0xc9, 0, 0x10];
        code.extend(b"fixture.ogv\0");
        code.extend([12, 0, 0]);
        let stream_offset = code.len();
        // The native probe uses immediate addresses (5 bytes); this program
        // uses a bank operand (3 bytes), so its stream instruction is 13 bytes.
        code.extend([0xd6, 6, 12, 0, 0, 4, 1, 0, 0, 0, 12, 1, 0]);
        code.extend([0x9d, 4, 12, 1, 0]);
        let mut vm = BinaryVm::new("stream.scn", code).unwrap();
        let mut resources = Resources::default();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::LoadAsset { .. },
                ..
            }
        ));
        assert!(vm.respond(1).is_err());
        assert!(vm.respond_asset(Vec::new()).is_err());
        let address = vm.respond_asset(data.clone()).unwrap();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::CreateAudioStream { flags: 0x21, .. },
                ..
            }
        ));
        assert!(vm.respond(0).is_err());
        assert_eq!(vm.allocation_bytes(address).unwrap(), data);
        let handle = resources
            .create_audio_stream(vm.allocation_bytes(address).unwrap(), 0x21)
            .unwrap();
        vm.respond(handle).unwrap();
        assert_eq!(
            vm.location().offset - stream_offset,
            probe["next_offset"].as_u64().unwrap() as usize - 2
        );
        let changed: Vec<_> = data
            .iter()
            .zip(vm.allocation_bytes(address).unwrap())
            .enumerate()
            .filter_map(|(i, (a, b))| (a != b).then_some(i))
            .collect();
        assert_eq!(serde_json::json!(changed), probe["changed_offsets"]);
        assert_eq!(&vm.allocation_bytes(address).unwrap()[8..12], b"WAVE");
        assert!(matches!(vm.step().unwrap(),Event::MouseButtonMapping {value,..} if value==handle));
        let stream = resources.audio_stream(handle).unwrap();
        assert_eq!(
            stream.sound.samples.len() * 2,
            probe["pcm_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            stream.sound.sample_rate,
            probe["buffers"][0]["sample_rate"].as_u64().unwrap() as u32
        );
        assert_eq!(
            stream.sound.channels,
            probe["buffers"][0]["channels"].as_u64().unwrap() as u8
        );
        assert_eq!(stream.loop_start_frame, 0);
        let samples = stream.sound.samples.clone();
        let resident = resources.resident_bytes();
        assert_eq!(resident, samples.len() * 2);
        for invalid in [
            b"OGV\0".to_vec(),
            {
                let mut bad = data.clone();
                bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
                bad
            },
            {
                let mut bad = data.clone();
                *bad.last_mut().unwrap() = 1;
                bad
            },
        ] {
            assert!(resources.create_audio_stream(&invalid, 0x21).is_err());
            assert_eq!(resources.resident_bytes(), resident);
            assert_eq!(
                resources.audio_stream(handle).unwrap().sound.samples,
                samples
            );
        }
        resources.resident = LIMIT;
        assert!(resources.create_audio_stream(&data, 0x21).is_err());
        assert_eq!(resources.streams.len(), 1);
        resources.resident = resident;
        let mut mixer = crate::audio::Mixer::default();
        mixer
            .play(handle, resources.audio_stream(handle).unwrap().clone(), 2)
            .unwrap();
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/stream-lifecycle-probe.json"
        ))
        .unwrap();
        for case in probe["lifecycle"].as_array().unwrap() {
            let id = if case["null"].as_bool().unwrap() {
                0
            } else {
                handle
            };
            match case["opcode"].as_u64().unwrap() {
                0x6da => mixer.stop(id),
                0x6d8 => {
                    mixer.stop(id);
                    resources.release_audio_stream(id).unwrap();
                }
                0x6d9 => mixer
                    .play(id, resources.audio_stream(id).unwrap().clone(), 2)
                    .unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                resources.audio_stream(handle).is_some(),
                case["registered"].as_bool().unwrap()
            );
            assert_eq!(
                mixer.is_playing(handle),
                case["state_flags"].as_u64().unwrap() & 1 != 0
            );
            let mut pcm = [1.0; 8];
            mixer.render(&mut pcm, 8000, 1).unwrap();
            if !mixer.is_playing(handle) {
                assert_eq!(pcm, [0.0; 8]);
            }
        }
        assert_eq!(resources.resident_bytes(), 0);
        assert!(resources.release_audio_stream(handle).is_err());
        for _ in 0..300 {
            let id = resources.create_audio_stream(&data, 0x21).unwrap();
            assert_eq!(id, handle);
            resources.release_audio_stream(id).unwrap();
            assert_eq!(resources.resident_bytes(), 0);
        }
    }
    #[test]
    fn sound_decode_and_failed_replacement_preserve_resource_budget() {
        let mut resources = Resources::default();
        let ogg = include_bytes!("../../shiinario_assets/tests/fixtures/sine.ogg");
        resources.load_sound(20, ogg, 0x8000).unwrap();
        let sound = resources.sound(20).unwrap();
        assert_eq!((sound.channels, sound.sample_rate), (1, 8000));
        let samples = sound.samples.clone();
        let size = samples.len() * 2;
        assert_eq!(resources.resident_bytes(), size);
        assert!(resources.load_sound(20, b"OGV\0", 0x8000).is_err());
        assert!(resources.load_sound(20, ogg, 0).is_err());
        assert!(resources.load_sound(256, ogg, 0x8000).is_err());
        assert_eq!(resources.sound(20).unwrap().samples, samples);
        assert_eq!(resources.resident_bytes(), size);
        resources.load_sound(20, ogg, 0x8000).unwrap();
        assert_eq!(resources.resident_bytes(), size);
        resources.resident = LIMIT;
        assert!(resources.load_sound(21, ogg, 0x8000).is_err());
        assert!(resources.sound(21).is_none());
        assert_eq!(resources.sound(20).unwrap().samples, samples);
    }
    #[test]
    fn surface_fill_matches_native_clipping_and_rgb_packing() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/surface-fill-probe.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let code = case["code"].as_str().unwrap();
            let bytes = (0..code.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&code[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("fill.SCN", bytes).unwrap();
            let mut resources = Resources::default();
            resources.create_surface(3, 5, 4, 0).unwrap();
            let Event::Platform {
                request: PlatformRequest::FillSurface { id, rect, color },
                ..
            } = vm.step().unwrap()
            else {
                panic!("expected fill");
            };
            assert_eq!((id, color), (3, [0x23, 0xfe, 0x56]));
            assert!(resources.fill_surface(17, rect, color).is_err());
            resources.fill_surface(id, rect, color).unwrap();
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset,
                case["next_offset"].as_u64().unwrap() as usize
            );
            let mut expected = vec![0; 5 * 4 * 4];
            for pixel in expected.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            // Rasterize the rectangle recorded at the original GDI call, not
            // the script's input rectangle. This checks engine clipping too.
            for fill in case["fills"].as_array().unwrap() {
                let r: Vec<i64> = fill["rect"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|n| n.as_i64().unwrap())
                    .collect();
                let c = fill["color"].as_u64().unwrap();
                for y in 0..4 {
                    for x in 0..5 {
                        if x >= r[0] && x < r[2] && y >= r[1] && y < r[3] {
                            let offset = ((y * 5 + x) * 4) as usize;
                            expected[offset..offset + 4].copy_from_slice(&[
                                c as u8,
                                (c >> 8) as u8,
                                (c >> 16) as u8,
                                255,
                            ]);
                        }
                    }
                }
            }
            assert_eq!(
                resources.surface(3).unwrap().rgba,
                expected,
                "input {:?}",
                case["args"]
            );
            assert_eq!(resources.resident_bytes(), 64);
        }
    }
    #[test]
    fn mutable_images_match_original_dispatcher_snapshots() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/image-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let code = case["code"].as_str().unwrap();
            let bytes = (0..code.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&code[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("synthetic.scn", bytes).unwrap();
            let mut resources = Resources::default();
            for state in case["states"].as_array().unwrap() {
                let Event::Platform { request, .. } = vm.step().unwrap() else {
                    panic!("expected resource request")
                };
                // A failed host reply must leave the operation pending.
                assert!(vm.respond(0).is_err());
                let Event::Platform {
                    request: repeated, ..
                } = vm.step().unwrap()
                else {
                    panic!("expected pending request")
                };
                assert_eq!(request, repeated);
                match request {
                    PlatformRequest::CreateImage {
                        id,
                        width,
                        height,
                        bytes_per_pixel,
                        frames,
                    } => resources
                        .create_image(id, width, height, bytes_per_pixel, frames)
                        .unwrap(),
                    PlatformRequest::FillImage {
                        id,
                        frame,
                        x,
                        y,
                        width,
                        height,
                        color,
                    } => resources
                        .fill_image(id, frame, [x, y, width, height], color)
                        .unwrap(),
                    _ => panic!("unexpected request"),
                }
                vm.respond(1).unwrap();
                assert_eq!(vm.location().offset, state["pc"].as_u64().unwrap() as usize);
                for (index, expected) in state["frames"].as_array().unwrap().iter().enumerate() {
                    let actual = resources.frame(17, index).unwrap();
                    let rgba: Vec<u8> = expected["rgba"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_u64().unwrap() as u8)
                        .collect();
                    assert_eq!(actual.surface.rgba, rgba);
                    assert_eq!(
                        (
                            actual.x,
                            actual.y,
                            actual.surface.width,
                            actual.surface.height
                        ),
                        (0, 0, 3, 2)
                    );
                }
            }
        }
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
    fn mask_transition_matches_original_thresholds_channels_and_aliases() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/mask-transition-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut resources = Resources::default();
            for (index, id) in [0, 3, 11, 12].into_iter().enumerate() {
                resources.create_surface(id, 8, 4, 0).unwrap();
                resources
                    .surface_memory(id)
                    .unwrap()
                    .write(0, &decode(&fixture["initial"][index]))
                    .unwrap();
            }
            let mut vm = BinaryVm::new("mask.scn", decode(&case["code"])).unwrap();
            let Event::Platform {
                request: PlatformRequest::MaskTransition(transition),
                ..
            } = vm.step().unwrap()
            else {
                panic!("expected mask transition");
            };
            resources.mask_transition(&transition).unwrap();
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
            assert_eq!(
                resources.surface_memory(0).unwrap().read(0, 96).unwrap(),
                decode(&case["pixels"]),
                "{:?}",
                case["args"]
            );
        }
        let mut resources = Resources::default();
        resources.create_surface(0, 8, 4, 0).unwrap();
        let valid = MaskTransition {
            destination: 0,
            first: 0,
            second: None,
            mask: 0,
            flags: 0xc0000080,
        };
        for kind in 0..4 {
            let mut transition = valid.clone();
            match kind {
                0 => transition.flags = 0x80000201,
                1 => transition.flags = 0xc0000201,
                2 => transition.first = 99,
                _ => transition.second = Some(99),
            }
            let before = resources.surface_memory(0).unwrap().read(0, 96).unwrap();
            assert!(resources.mask_transition(&transition).is_err());
            assert_eq!(
                resources.surface_memory(0).unwrap().read(0, 96).unwrap(),
                before
            );
        }
    }
    #[test]
    fn surface_copy_matches_original_clipping_overlap_and_operand_order() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/surface-copy-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut resources = Resources::default();
            for (index, id) in [0, 3, 11].into_iter().enumerate() {
                resources.create_surface(id, 8, 4, 0).unwrap();
                resources
                    .surface_memory(id)
                    .unwrap()
                    .write(0, &decode(&fixture["initial"][index]))
                    .unwrap();
            }
            let mut vm = BinaryVm::new("copy.scn", decode(&case["code"])).unwrap();
            let Event::Platform {
                request: PlatformRequest::CopySurface(copy),
                ..
            } = vm.step().unwrap()
            else {
                panic!("expected copy");
            };
            resources.copy_surface(&copy).unwrap();
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
            assert_eq!(
                resources.surface_memory(0).unwrap().read(0, 96).unwrap(),
                decode(&case["pixels"]),
                "{:?}",
                case["args"]
            );
        }
        let mut resources = Resources::default();
        resources.create_surface(0, 8, 4, 0).unwrap();
        let point = shiinario_scenario::SurfacePoint { id: 0, x: 0, y: 0 };
        let valid = SurfaceCopy {
            destination: point.clone(),
            source: point,
            size: [8, 4],
        };
        for kind in 0..4 {
            let mut copy = valid.clone();
            match kind {
                0 => copy.source.x = 1,
                1 => copy.size[0] = -1,
                2 => copy.destination.x = i32::MAX,
                _ => copy.source.id = 99,
            }
            let before = resources.surface_memory(0).unwrap().read(0, 96).unwrap();
            assert!(resources.copy_surface(&copy).is_err());
            assert_eq!(
                resources.surface_memory(0).unwrap().read(0, 96).unwrap(),
                before
            );
        }
    }
    #[test]
    fn weighted_blend_matches_original_pixels_clipping_and_operand_order() {
        use shiinario_scenario::{BinaryVm, Event};
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/surface-blend-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut resources = Resources::default();
            for (index, id) in [0, 3, 11].into_iter().enumerate() {
                resources.create_surface(id, 8, 4, 0).unwrap();
                resources
                    .surface_memory(id)
                    .unwrap()
                    .write(0, &decode(&fixture["initial"][index]))
                    .unwrap();
            }
            let mut vm = BinaryVm::new("blend.scn", decode(&case["code"])).unwrap();
            let Event::Platform {
                request: PlatformRequest::BlendSurfaces(blend),
                ..
            } = vm.step().unwrap()
            else {
                panic!("expected blend");
            };
            resources.blend_surfaces(&blend).unwrap();
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
            assert_eq!(
                resources.surface_memory(0).unwrap().read(0, 96).unwrap(),
                decode(&case["pixels"]),
                "{:?}",
                case["args"]
            );
        }
        let mut resources = Resources::default();
        resources.create_surface(0, 8, 4, 0).unwrap();
        resources.create_surface(3, 8, 4, 0).unwrap();
        let point = shiinario_scenario::SurfacePoint { id: 0, x: 0, y: 0 };
        let mut blend = SurfaceBlend {
            destination: point.clone(),
            sources: [point.clone(), point],
            size: [8, 4],
            weights: [1, 1],
        };
        resources
            .fill_surface(0, [0, 0, 8, 4], [255, 127, 3])
            .unwrap();
        // Same-coordinate in-place blends are defined; shifted aliasing is not.
        resources.blend_surfaces(&blend).unwrap();
        let previous = resources.surface(0).unwrap();
        blend.size = [7, 4];
        blend.sources[0].x = 1;
        assert!(resources.blend_surfaces(&blend).is_err());
        assert_eq!(resources.surface(0).unwrap(), previous);
        blend.sources[0].id = 3;
        blend.sources[0].x = 8;
        assert!(resources.blend_surfaces(&blend).is_err());
        assert_eq!(resources.surface(0).unwrap(), previous);
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
