//! Window-independent image slots and drawing buffers used by binary SCN.
use anyhow::{Context, Result, ensure};
use shiinario_assets::{audio, image, project::Project};
use shiinario_scenario::PlatformRequest;
use std::collections::BTreeMap;

const LIMIT: usize = 256 * 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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
    surfaces: BTreeMap<u32, Surface>,
    images: BTreeMap<u32, Image>,
    sounds: BTreeMap<u32, Sound>,
    resident: usize,
    archives: Vec<String>,
}
impl Resources {
    pub fn register_archive(&mut self, name: &str) {
        if !self
            .archives
            .iter()
            .any(|item| item.eq_ignore_ascii_case(name))
        {
            self.archives.push(name.to_owned());
        }
    }
    pub fn surface(&self, id: u32) -> Option<&Surface> {
        self.surfaces.get(&id)
    }
    pub fn sound(&self, id: u32) -> Option<&Sound> {
        self.sounds.get(&id)
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
    fn create_surface(&mut self, id: u32, width: u32, height: u32, flags: u32) -> Result<()> {
        ensure!(
            id < 256 && width > 0 && height > 0 && width <= 16384 && height <= 16384,
            "invalid drawing surface dimensions or slot"
        );
        // Other allocation modes request legacy DirectDraw surfaces. Their
        // locking and pixel layout must be recovered before accepting them.
        ensure!(flags == 0, "unresolved surface allocation flags {flags:#x}");
        let size = usize::try_from(u64::from(width) * u64::from(height) * 4)?;
        let old = self
            .surfaces
            .get(&id)
            .map_or(0, |surface| surface.rgba.len());
        let resident = self.resident - old + size;
        ensure!(resident <= LIMIT, "drawing resources exceed 256 MiB");
        let mut rgba = vec![0; size];
        // GDI's zeroed BGR bitmap represents opaque black.
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        self.surfaces.insert(
            id,
            Surface {
                width,
                height,
                rgba,
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
        assert_eq!(resources.resident_bytes(), 800 * 600 * 4);
        assert_eq!(&resources.surface(2).unwrap().rgba[..4], &[0, 0, 0, 255]);
        assert!(resources.create_surface(2, 16384, 16384, 0).is_err());
        assert_eq!(resources.surface(2).unwrap().width, 800);
        resources.create_surface(2, 2, 3, 0).unwrap();
        assert_eq!(resources.resident_bytes(), 24);
        assert!(resources.create_surface(256, 2, 3, 0).is_err());
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
