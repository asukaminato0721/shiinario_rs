//! Scalar v2.48 kernels. Offsets are validated before any pixel is written.
use super::*;
use shiinario_scenario::{ImageCopy, SurfaceArithmetic, SurfacePoint, SurfaceRipple, SurfaceWave};

struct CopyRegion {
    dx: usize,
    dy: usize,
    sx: usize,
    sy: usize,
    width: usize,
    height: usize,
}

impl CopyRegion {
    fn new(
        copy: &SurfaceCopy,
        dst: [u32; 2],
        src: [u32; 2],
        clip_source: bool,
    ) -> Result<Option<Self>> {
        let [w, h] = copy.size.map(i64::from);
        ensure!(
            (0..=16384).contains(&w) && (0..=16384).contains(&h),
            "invalid effect dimensions"
        );
        let [dx, dy] = [copy.destination.x, copy.destination.y].map(i64::from);
        let [sx, sy] = [copy.source.x, copy.source.y].map(i64::from);
        let mut left = 0.max(-dx);
        let mut top = 0.max(-dy);
        let mut right = w.min(i64::from(dst[0]) - dx);
        let mut bottom = h.min(i64::from(dst[1]) - dy);
        if clip_source {
            left = left.max(-sx);
            top = top.max(-sy);
            right = right.min(i64::from(src[0]) - sx);
            bottom = bottom.min(i64::from(src[1]) - sy);
        }
        if left >= right || top >= bottom {
            return Ok(None);
        }
        ensure!(
            sx + left >= 0
                && sy + top >= 0
                && sx + right <= i64::from(src[0])
                && sy + bottom <= i64::from(src[1]),
            "effect source rectangle outside buffer"
        );
        Ok(Some(Self {
            dx: (dx + left) as usize,
            dy: (dy + top) as usize,
            sx: (sx + left) as usize,
            sy: (sy + top) as usize,
            width: (right - left) as usize,
            height: (bottom - top) as usize,
        }))
    }
}

impl Resources {
    pub(super) fn copy_image(&mut self, op: &ImageCopy) -> Result<()> {
        let mutable_frame = |p: &shiinario_scenario::ImagePoint| -> Result<(&Surface, u32)> {
            let Some(Image::Mutable {
                frames,
                bytes_per_pixel,
            }) = self.images.get(&p.id)
            else {
                bail!("raw image copy requires mutable images");
            };
            Ok((
                frames
                    .get(p.frame as usize)
                    .context("image copy frame missing")?,
                *bytes_per_pixel,
            ))
        };
        let (dst, dst_bpp) = mutable_frame(&op.destination)?;
        let (src, src_bpp) = mutable_frame(&op.source)?;
        let point = |p: &shiinario_scenario::ImagePoint| SurfacePoint {
            id: p.id,
            x: p.x,
            y: p.y,
        };
        let copy = SurfaceCopy {
            destination: point(&op.destination),
            source: point(&op.source),
            size: op.size,
        };
        let Some(r) = CopyRegion::new(
            &copy,
            [dst.width, dst.height],
            [src.width, src.height],
            false,
        )?
        else {
            return Ok(());
        };
        let pack = |frame: &Surface, bpp: u32| -> Vec<u8> {
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let bytes = if bpp == 4 {
                        [p[3], p[2], p[1], p[0]]
                    } else {
                        [p[2], p[1], p[0], 0]
                    };
                    bytes.into_iter().take(bpp as usize)
                })
                .collect()
        };
        let mut out = pack(dst, dst_bpp);
        let input = pack(src, src_bpp);
        let dst_stride = dst.width as usize * dst_bpp as usize;
        let src_stride = src.width as usize * src_bpp as usize;
        let alias = op.destination.id == op.source.id && op.destination.frame == op.source.frame;
        for y in 0..r.height {
            let a = (r.dy + y) * dst_stride + r.dx * dst_bpp as usize;
            let b = (r.sy + y) * src_stride + r.sx * src_bpp as usize;
            if dst_bpp == src_bpp {
                let length = r.width * dst_bpp as usize;
                // REP MOVSD reads four bytes before it writes them. This
                // matters for an overlapping three-byte image rectangle.
                for byte in (0..length).step_by(4) {
                    let count = (length - byte).min(4);
                    if count == 4 {
                        let pixels = if alias { &out } else { &input };
                        let value: [u8; 4] = pixels[b + byte..b + byte + 4].try_into()?;
                        out[a + byte..a + byte + 4].copy_from_slice(&value);
                    } else {
                        for tail in byte..length {
                            out[a + tail] = if alias {
                                out[b + tail]
                            } else {
                                input[b + tail]
                            };
                        }
                    }
                }
            } else {
                // The original 3-to-4 path compares destination bytes with
                // the three-byte source span, so it stops before full width.
                let pixels = if dst_bpp == 4 {
                    (r.width * 3).div_ceil(4)
                } else {
                    r.width
                };
                for x in 0..pixels {
                    let at = a + x * dst_bpp as usize;
                    let from = b + x * src_bpp as usize;
                    if dst_bpp == 4 {
                        out[at] = 255;
                        out[at + 1..at + 4].copy_from_slice(&input[from..from + 3]);
                    } else {
                        out[at..at + 3].copy_from_slice(&input[from + 1..from + 4]);
                    }
                }
            }
        }
        let rgba = out
            .chunks_exact(dst_bpp as usize)
            .flat_map(|p| {
                if dst_bpp == 4 {
                    [p[3], p[2], p[1], p[0]]
                } else {
                    [p[2], p[1], p[0], 255]
                }
            })
            .collect();
        let Some(Image::Mutable { frames, .. }) = self.images.get_mut(&op.destination.id) else {
            unreachable!()
        };
        frames[op.destination.frame as usize].rgba = rgba;
        Ok(())
    }

    pub(super) fn arithmetic_surface(&mut self, op: &SurfaceArithmetic) -> Result<()> {
        let dst = self
            .surfaces
            .get(&op.copy.destination.id)
            .context("effect destination missing")?;
        let src = self
            .surfaces
            .get(&op.copy.source.id)
            .context("effect source missing")?;
        ensure!(
            dst.stride == src.stride,
            "surface arithmetic requires equal strides"
        );
        let Some(r) = CopyRegion::new(
            &op.copy,
            [dst.width, dst.height],
            [src.width, src.height],
            false,
        )?
        else {
            return Ok(());
        };
        let mut out = dst.pixels.read(0, dst.pixels.len())?;
        let input = src.pixels.read(0, src.pixels.len())?;
        for y in 0..r.height {
            for byte in 0..r.width * 3 {
                let a = (r.dy + y) * dst.stride + r.dx * 3 + byte;
                let b = (r.sy + y) * src.stride + r.sx * 3 + byte;
                let source = if op.copy.source.id == op.copy.destination.id {
                    out[b]
                } else {
                    input[b]
                };
                // The native scalar path takes AH from a wrapping multiply.
                let value = (u32::from(source).wrapping_mul(op.amount) >> 8) as u8;
                out[a] = if op.subtract {
                    out[a].saturating_sub(value)
                } else {
                    out[a].saturating_add(value)
                };
            }
        }
        dst.pixels.write(0, &out)
    }

    fn copy_gdi(&mut self, copy: &SurfaceCopy) -> Result<()> {
        if copy.size[0] <= 0 || copy.size[1] <= 0 {
            return Ok(());
        }
        let dst = self
            .surfaces
            .get(&copy.destination.id)
            .context("blit destination missing")?;
        let src = self
            .surfaces
            .get(&copy.source.id)
            .context("blit source missing")?;
        let Some(r) =
            CopyRegion::new(copy, [dst.width, dst.height], [src.width, src.height], true)?
        else {
            return Ok(());
        };
        // Each GDI call reads its complete input before it changes an overlap.
        let input = src.pixels.read(0, src.pixels.len())?;
        for y in 0..r.height {
            let b = (r.sy + y) * src.stride + r.sx * 3;
            dst.pixels.write(
                (r.dy + y) * dst.stride + r.dx * 3,
                &input[b..b + r.width * 3],
            )?;
        }
        Ok(())
    }

    pub(super) fn wave_surface(&mut self, op: &SurfaceWave) -> Result<()> {
        let dst = self
            .surfaces
            .get(&op.copy.destination.id)
            .context("wave destination missing")?;
        ensure!(
            self.surfaces.contains_key(&op.copy.source.id),
            "wave source missing"
        );
        // Clip the destination before calculating strip widths and wave phase.
        let size = [dst.width, dst.height];
        let mut clipped = op.copy.clone();
        let [w, h] = clipped.size.map(i64::from);
        ensure!(
            (0..=16384).contains(&w) && (0..=16384).contains(&h),
            "invalid wave dimensions"
        );
        let left = i64::from(clipped.destination.x).max(0);
        let top = i64::from(clipped.destination.y).max(0);
        let right = (i64::from(clipped.destination.x) + w).min(i64::from(size[0]));
        let bottom = (i64::from(clipped.destination.y) + h).min(i64::from(size[1]));
        if right <= left || bottom <= top {
            return Ok(());
        }
        clipped.source.x = clipped
            .source
            .x
            .wrapping_add((left - i64::from(clipped.destination.x)) as i32);
        clipped.source.y = clipped
            .source
            .y
            .wrapping_add((top - i64::from(clipped.destination.y)) as i32);
        clipped.destination.x = left as i32;
        clipped.destination.y = top as i32;
        clipped.size = [(right - left) as i32, (bottom - top) as i32];
        let span = clipped.size[0]; // The original also uses width for vertical wrap.
        let count = clipped.size[usize::from(!op.vertical)];
        let mut phase = op.phase.wrapping_shl(8).wrapping_add(op.step);
        for strip in 0..count {
            let displacement =
                trig::SIN[((phase >> 8) & 255) as usize].wrapping_mul(op.amplitude) >> 16;
            phase = phase.wrapping_add(op.step);
            let (first, source, second_source) = if displacement < 0 {
                (
                    span.wrapping_add(displacement),
                    displacement.wrapping_neg(),
                    0,
                )
            } else {
                (displacement, span.wrapping_sub(displacement), 0)
            };
            let second = span.wrapping_sub(first);
            for (offset, source_offset, length) in
                [(0i32, source, first), (first, second_source, second)]
            {
                let mut blit = clipped.clone();
                if op.vertical {
                    blit.destination.x = blit.destination.x.wrapping_add(strip);
                    blit.source.x = blit.source.x.wrapping_add(strip);
                    blit.destination.y = blit.destination.y.wrapping_add(offset);
                    blit.source.y = blit.source.y.wrapping_add(source_offset);
                    blit.size = [1, length];
                } else {
                    blit.destination.y = blit.destination.y.wrapping_add(strip);
                    blit.source.y = blit.source.y.wrapping_add(strip);
                    blit.destination.x = blit.destination.x.wrapping_add(offset);
                    blit.source.x = blit.source.x.wrapping_add(source_offset);
                    blit.size = [length, 1];
                }
                self.copy_gdi(&blit)?;
            }
        }
        Ok(())
    }

    pub(super) fn ripple_surface(&mut self, op: &SurfaceRipple) -> Result<()> {
        let dst = self
            .surfaces
            .get(&op.destination)
            .context("ripple destination missing")?;
        let src = self
            .surfaces
            .get(&op.source)
            .context("ripple source missing")?;
        ensure!(
            dst.width == src.width && dst.height == src.height,
            "ripple surface dimensions differ"
        );
        let [w, h] = [dst.width as usize, dst.height as usize];
        let mut out = dst.pixels.read(0, dst.pixels.len())?;
        let input = src.pixels.read(0, src.pixels.len())?;
        let phase = (op.phase >> 8).wrapping_neg() as u8;
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                let ax = (w / 2 - x) as f64;
                let ay = (h / 2 - y) as f64;
                let radius = (ax * ax + ay * ay).sqrt() as u32;
                let angle = ((-ay).atan2(ax) * 40.74366543152521 + 256.0) as usize & 255;
                let index = (radius as u8)
                    .wrapping_add(phase)
                    .wrapping_mul(op.frequency as u8) as usize;
                let magnitude = trig::SIN[index].wrapping_mul(op.amplitude) / 256;
                let dx = trig::COS[angle].wrapping_mul(magnitude) / 256;
                let dy = trig::SIN[angle].wrapping_mul(magnitude) / 256;
                let left = x as i64 + i64::from(dx);
                let right = w as i64 - i64::from(dx) - x as i64 - 1;
                let top = y as i64 - i64::from(dy);
                let bottom = h as i64 + i64::from(dy) - y as i64 - 1;
                let valid = [left, right].iter().all(|v| (0..w as i64).contains(v))
                    && [top, bottom].iter().all(|v| (0..h as i64).contains(v));
                for (tx, ty, sx, sy) in [
                    (x, y, left, top),
                    (w - x - 1, y, right, top),
                    (x, h - y - 1, left, bottom),
                    (w - x - 1, h - y - 1, right, bottom),
                ] {
                    // Native output is tight RGB, even if the DIB has padding.
                    let at = (ty * w + tx) * 3;
                    let color: [u8; 3] = if valid {
                        let offset = sy as usize * dst.stride + sx as usize * 3;
                        let pixels = if op.source == op.destination {
                            &out
                        } else {
                            &input
                        };
                        pixels[offset..offset + 3].try_into()?
                    } else {
                        [0; 3]
                    };
                    out[at..at + 3].copy_from_slice(&color);
                }
            }
        }
        dst.pixels.write(0, &out)
    }

    pub(super) fn present_surface(&mut self, source: u32, level: u32) -> Result<()> {
        ensure!(level <= 256, "invalid presentation blend level");
        if source == 0 || level == 256 {
            return Ok(());
        }
        let dst = self.surfaces.get(&0).context("display surface missing")?;
        if level == 0 {
            return self.clear_surface(0);
        }
        let src = self
            .surfaces
            .get(&source)
            .context("presentation source missing")?;
        ensure!(
            src.pixels.len() >= dst.pixels.len(),
            "presentation source is too small"
        );
        if level == 255 {
            return dst.pixels.write(0, &src.pixels.read(0, dst.pixels.len())?);
        }
        self.blend_surfaces(&SurfaceBlend {
            destination: SurfacePoint { id: 0, x: 0, y: 0 },
            sources: [
                SurfacePoint { id: 0, x: 0, y: 0 },
                SurfacePoint {
                    id: source,
                    x: 0,
                    y: 0,
                },
            ],
            size: [dst.width as i32, dst.height as i32],
            weights: [255 - level, level],
        })
    }
}
