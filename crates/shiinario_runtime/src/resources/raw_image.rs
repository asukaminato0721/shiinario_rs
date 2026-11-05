//! Native mutable-image rows: eight header bytes followed by BGR or ABGR.
use super::*;
use std::cell::Cell;

pub(super) struct RawFrame {
    pub memory: SharedMemory,
    size: [u32; 2],
    bpp: usize,
    pub stride: usize,
    synced: Cell<u64>,
}
impl RawFrame {
    pub fn surface(&self) -> Result<Surface> {
        let bytes = self.memory.read(0, self.memory.len())?;
        let mut rgba = Vec::with_capacity(self.size[0] as usize * self.size[1] as usize * 4);
        for row in bytes.chunks_exact(self.stride) {
            for pixel in row[8..].chunks_exact(self.bpp) {
                rgba.extend(if self.bpp == 4 {
                    [pixel[3], pixel[2], pixel[1], pixel[0]]
                } else {
                    [pixel[2], pixel[1], pixel[0], 255]
                });
            }
        }
        Ok(Surface {
            width: self.size[0],
            height: self.size[1],
            rgba,
        })
    }
    fn store(&self, surface: &Surface) -> Result<()> {
        let mut bytes = self.memory.read(0, self.memory.len())?;
        for (row, pixels) in bytes
            .chunks_exact_mut(self.stride)
            .zip(surface.rgba.chunks_exact(self.size[0] as usize * 4))
        {
            for (target, source) in row[8..]
                .chunks_exact_mut(self.bpp)
                .zip(pixels.as_chunks::<4>().0.iter())
            {
                if self.bpp == 4 {
                    target.copy_from_slice(&[source[3], source[2], source[1], source[0]]);
                } else {
                    target.copy_from_slice(&[source[2], source[1], source[0]]);
                }
            }
        }
        self.memory.write(0, &bytes)?;
        self.synced.set(self.memory.revision());
        Ok(())
    }
}
impl Resources {
    pub(crate) fn fade_frame(&mut self, level: u32) -> Result<()> {
        ensure!(level <= 255, "fade level exceeds 255");
        let dst = self.surfaces.get(&0).context("fade destination missing")?;
        let src = self.surfaces.get(&1).context("fade source missing")?;
        ensure!(
            dst.width == src.width && dst.height == src.height && dst.stride == src.stride,
            "fade surfaces differ in size"
        );
        let mut bytes = dst.pixels.read(0, dst.pixels.len())?;
        let input = src.pixels.read(0, src.pixels.len())?;
        for row in 0..dst.height as usize {
            for col in 0..dst.width as usize * 3 {
                let at = row * dst.stride + col;
                bytes[at] = (u32::from(input[at]) * level / 255) as u8;
            }
        }
        dst.pixels.write(0, &bytes)?;
        self.commit_display([0, 0, dst.width as i32, dst.height as i32]);
        Ok(())
    }

    pub fn image_stride(&self, image: u32, frame: u32) -> Result<u32> {
        let Image::Mutable {
            bytes_per_pixel,
            frames,
        } = self
            .images
            .get(&image)
            .context("image slot is not loaded")?
        else {
            bail!("raw image access requires a mutable image");
        };
        let surface = frames
            .get(frame as usize)
            .context("raw image frame is missing")?;
        Ok(surface.width * bytes_per_pixel + 8)
    }
    pub fn image_pixels(
        &mut self,
        image: u32,
        frame: u32,
        position: [u32; 2],
    ) -> Result<(SharedMemory, usize)> {
        let stride = self.image_stride(image, frame)? as usize;
        let Image::Mutable {
            bytes_per_pixel,
            frames,
        } = &self.images[&image]
        else {
            unreachable!()
        };
        let surface = &frames[frame as usize];
        ensure!(
            position[0] < surface.width && position[1] < surface.height,
            "raw image pixel outside frame"
        );
        let offset =
            position[1] as usize * stride + 8 + position[0] as usize * *bytes_per_pixel as usize;
        if !self.raw_frames.contains_key(&(image, frame)) {
            let length = stride * surface.height as usize;
            ensure!(
                self.resident + length <= LIMIT,
                "raw image exceeds resource budget"
            );
            let raw = RawFrame {
                memory: SharedMemory::zeroed(length)?,
                size: [surface.width, surface.height],
                bpp: *bytes_per_pixel as usize,
                stride,
                synced: Cell::new(0),
            };
            // Native 410f70 stores the row flags/byte count and pixel count.
            for row in 0..surface.height as usize {
                raw.memory.write(
                    row * stride,
                    &((if *bytes_per_pixel == 4 {
                        0x8000_0000
                    } else {
                        0x4000_0000
                    }) | (surface.width * *bytes_per_pixel + 6))
                        .to_le_bytes(),
                )?;
                raw.memory
                    .write(row * stride + 4, &(surface.width).to_le_bytes())?;
            }
            raw.store(surface)?;
            self.raw_frames.insert((image, frame), raw);
            self.resident += length;
        }
        Ok((self.raw_frames[&(image, frame)].memory.clone(), offset))
    }
    pub(super) fn sync_raw_images(&mut self) -> Result<()> {
        for (&(id, index), raw) in &self.raw_frames {
            if raw.synced.get() != raw.memory.revision() {
                let Image::Mutable { frames, .. } = self
                    .images
                    .get_mut(&id)
                    .context("raw image slot is missing")?
                else {
                    bail!("raw image is no longer mutable");
                };
                frames[index as usize] = raw.surface()?;
                raw.synced.set(raw.memory.revision());
            }
        }
        Ok(())
    }
    pub(super) fn update_raw_images(&mut self, request: &PlatformRequest) -> Result<()> {
        let replaced = match request {
            PlatformRequest::CreateImage { id, .. }
            | PlatformRequest::CreateImageFromSurface { id, .. }
            | PlatformRequest::LoadImage { id, .. }
            | PlatformRequest::BindImageMemory { id, .. }
            | PlatformRequest::ReleaseImage { id } => Some(*id),
            _ => None,
        };
        if let Some(id) = replaced {
            self.raw_frames.retain(|&(slot, _), raw| {
                let keep = slot != id && id != 256;
                if !keep {
                    self.resident -= raw.memory.len();
                }
                keep
            });
        }
        let changed = match request {
            PlatformRequest::FillImage { id, frame, .. } => Some((*id, *frame)),
            PlatformRequest::DrawImageGlyph { image, frame, .. } => Some((*image, *frame)),
            PlatformRequest::DrawImageIntoImage { destination, .. } => {
                Some((destination[0], destination[1]))
            }
            PlatformRequest::CaptureSurface(copy) => Some((copy.image, copy.frame)),
            PlatformRequest::CopyImage(copy) => Some((copy.destination.id, copy.destination.frame)),
            PlatformRequest::BlendImages(blend) => {
                Some((blend.destination.id, blend.destination.frame))
            }
            PlatformRequest::AffineImage(transform) => {
                Some((transform.destination[0], transform.destination[1]))
            }
            _ => None,
        };
        if let Some(key) = changed
            && let Some(raw) = self.raw_frames.get(&key)
        {
            let Image::Mutable { frames, .. } = &self.images[&key.0] else {
                bail!("raw image is no longer mutable");
            };
            raw.store(&frames[key.1 as usize])?;
        }
        Ok(())
    }
}
