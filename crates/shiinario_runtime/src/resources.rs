//! Window-independent image slots and drawing buffers used by binary SCN.
use anyhow::{Context, Result, ensure};
use shiinario_assets::{audio, image, project::Project};
use shiinario_scenario::{PlatformRequest, SharedMemory};
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
    surfaces: BTreeMap<u32, DrawingSurface>,
    images: BTreeMap<u32, Image>,
    sounds: BTreeMap<u32, Sound>,
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
            for bgr in row[..surface.width as usize * 3].chunks_exact(3) {
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
        self.sounds.get(&id)
    }
    pub fn audio_stream(&self, handle: u32) -> Option<&Arc<AudioStream>> {
        self.streams.get(&handle)
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
                    surface: Surface {
                        width: frame.info.width,
                        height: frame.info.height,
                        rgba: frame.rgba,
                    },
                })
            }
            Image::Mutable { frames, .. } => Ok(Frame {
                x: 0,
                y: 0,
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
    fn create_surface(&mut self, id: u32, width: u32, height: u32, flags: u32) -> Result<()> {
        ensure!(
            id < 256 && width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "invalid drawing surface dimensions or slot"
        );
        // Other allocation modes request legacy DirectDraw surfaces. Their
        // locking and pixel layout must be recovered before accepting them.
        ensure!(flags == 0, "unresolved surface allocation flags {flags:#x}");
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
            Sound {
                sample_rate: info.sample_rate,
                channels: info.channels,
                samples,
            },
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
        match request {
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
