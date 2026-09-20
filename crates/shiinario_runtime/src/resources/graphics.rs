//! Portable implementations of the 0564 and 0574 graphics kernels.
use super::*;

impl Resources {
    pub(super) fn pixelate_surface(&mut self, copy: &SurfaceCopy, block: i32) -> Result<()> {
        if block <= 0 {
            return self.copy_surface(copy);
        }
        let dst = self
            .surfaces
            .get(&copy.destination.id)
            .context("pixelation destination missing")?;
        let src = self
            .surfaces
            .get(&copy.source.id)
            .context("pixelation source missing")?;
        let [w, h] = copy.size.map(i64::from);
        ensure!(w >= 0 && h >= 0, "negative pixelation dimensions");
        let dx = i64::from(copy.destination.x);
        let dy = i64::from(copy.destination.y);
        let left = dx.max(0);
        let top = dy.max(0);
        let right = (dx + w).min(i64::from(dst.width));
        let bottom = (dy + h).min(i64::from(dst.height));
        if right <= left || bottom <= top {
            return Ok(());
        }
        let sx = i64::from(copy.source.x) + left - dx;
        let sy = i64::from(copy.source.y) + top - dy;
        let width = right - left;
        let height = bottom - top;
        let block = i64::from(block);
        // The original starts its sample coordinates at block/2, independently
        // of the source rectangle origin; subsequent samples clamp to its end.
        let sample_x = |x: i64| {
            if x == 0 {
                block / 2
            } else {
                (block / 2 + x).min(sx + width - 1)
            }
        };
        let sample_y = |y: i64| (block / 2 + y).min(sy + height - 1);
        for y in (0..height).step_by(block as usize) {
            for x in (0..width).step_by(block as usize) {
                ensure!(
                    (0..i64::from(src.width)).contains(&sample_x(x))
                        && (0..i64::from(src.height)).contains(&sample_y(y)),
                    "pixelation sample outside source surface"
                );
            }
        }
        let mut output = dst.pixels.read(0, dst.pixels.len())?;
        let source = src.pixels.read(0, src.pixels.len())?;
        for y in (0..height).step_by(block as usize) {
            for x in (0..width).step_by(block as usize) {
                let offset = sample_y(y) as usize * src.stride + sample_x(x) as usize * 3;
                let pixels = if copy.source.id == copy.destination.id {
                    &output
                } else {
                    &source
                };
                let color: [u8; 3] = pixels[offset..offset + 3].try_into()?;
                for by in y..(y + block).min(height) {
                    for bx in x..(x + block).min(width) {
                        let offset = (top + by) as usize * dst.stride + (left + bx) as usize * 3;
                        output[offset..offset + 3].copy_from_slice(&color);
                    }
                }
            }
        }
        dst.pixels.write(0, &output)
    }

    pub(super) fn affine_image(&mut self, op: &ImageAffine) -> Result<()> {
        let frame = |key: [u32; 2]| -> Result<&Surface> {
            let Some(Image::Mutable {
                bytes_per_pixel: 4,
                frames,
            }) = self.images.get(&key[0])
            else {
                bail!("affine transform requires a mutable four-byte image");
            };
            frames
                .get(key[1] as usize)
                .context("affine image frame missing")
        };
        let dst = frame(op.destination)?;
        let src = frame(op.source)?;
        let [dx, dy, w, h] = op.destination_rect.map(|v| i64::from(v as i32));
        let [sx, sy, sw, sh] = op.source_rect.map(|v| i64::from(v as i32));
        ensure!(
            w >= 0 && h >= 0 && sw >= 0 && sh >= 0,
            "negative affine rectangle size"
        );
        ensure!(
            sx >= 0
                && sy >= 0
                && sx + sw <= i64::from(src.width)
                && sy + sh <= i64::from(src.height),
            "affine source rectangle outside image"
        );
        let [degrees, scale_x, scale_y] = op.transform.map(|bits| f64::from(f32::from_bits(bits)));
        ensure!(
            degrees.is_finite()
                && scale_x.is_finite()
                && scale_y.is_finite()
                && scale_x != 0.0
                && scale_y != 0.0,
            "invalid affine angle or scale"
        );
        let left = dx.max(0);
        let top = dy.max(0);
        let right = (dx + w).min(i64::from(dst.width));
        let bottom = (dy + h).min(i64::from(dst.height));
        if right <= left || bottom <= top {
            return Ok(());
        }
        const UNIT: f64 = 4294967296.0;
        let fixed = |v: f64| -> Result<i64> {
            ensure!(
                v.is_finite() && v >= i64::MIN as f64 && v < -(i64::MIN as f64),
                "affine fixed-point overflow"
            );
            Ok(v.trunc() as i64)
        };
        let angle = degrees * (std::f64::consts::PI / 180.0);
        let sin = fixed(angle.sin() * UNIT)?;
        let cos = fixed(angle.cos() * UNIT)?;
        let cx = -((dx * 2 + w) / 2);
        let cy = -((dy * 2 + h) / 2);
        let origin_x = fixed(
            (cx as f64 * cos as f64 - cy as f64 * sin as f64 + f64::from(op.center[0]) * UNIT)
                / scale_x,
        )?;
        let origin_y = fixed(
            (cy as f64 * cos as f64 + cx as f64 * sin as f64 + f64::from(op.center[1]) * UNIT)
                / scale_y,
        )?;
        let step_x = fixed(cos as f64 / scale_x)?;
        let step_y = fixed(sin as f64 / scale_y)?;
        let row_x = fixed((angle + std::f64::consts::FRAC_PI_2).cos() * UNIT / scale_x)?;
        let row_y = fixed((angle + std::f64::consts::FRAC_PI_2).sin() * UNIT / scale_y)?;
        let [a, b, g, r] = op.background.to_le_bytes();
        let mut output = dst.rgba.clone();
        for y in top..bottom {
            // Native 4193e0 starts every clipped row at origin + y*row_step;
            // the destination left coordinate does not advance the source.
            let mut u = i128::from(origin_x) + i128::from(y) * i128::from(row_x);
            let mut v = i128::from(origin_y) + i128::from(y) * i128::from(row_y);
            for x in left..right {
                let px = u >> 32;
                let py = v >> 32;
                let offset = (y as usize * dst.width as usize + x as usize) * 4;
                if px >= i128::from(sx)
                    && px < i128::from(sx + sw)
                    && py >= i128::from(sy)
                    && py < i128::from(sy + sh)
                {
                    let source_offset = (py as usize * src.width as usize + px as usize) * 4;
                    let pixels = if op.source == op.destination {
                        &output
                    } else {
                        &src.rgba
                    };
                    let color: [u8; 4] = pixels[source_offset..source_offset + 4].try_into()?;
                    output[offset..offset + 4].copy_from_slice(&color);
                } else if op.flags & 1 == 0 {
                    output[offset..offset + 4].copy_from_slice(&[r, g, b, a]);
                }
                u += i128::from(step_x);
                v += i128::from(step_y);
            }
        }
        let Image::Mutable { frames, .. } = self.images.get_mut(&op.destination[0]).unwrap() else {
            unreachable!()
        };
        frames[op.destination[1] as usize].rgba = output;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shiinario_scenario::SurfacePoint;

    #[test]
    fn graphics_validate_before_writing_and_clip_to_allocations() {
        let mut resources = Resources::default();
        resources.create_image(1, 2, 2, 4, 1).unwrap();
        resources
            .fill_image(1, 0, [0, 0, 2, 2], 0x12345678)
            .unwrap();
        let original = resources.frame(1, 0).unwrap().surface;
        let valid = ImageAffine {
            destination: [1, 0],
            destination_rect: [0, 0, 2, 2],
            source: [1, 0],
            source_rect: [0, 0, 2, 2],
            center: [1, 1],
            transform: [0, 1f32.to_bits(), 1f32.to_bits()],
            flags: 0,
            background: 0,
        };
        for transform in [
            [0, 0, 1f32.to_bits()],
            [f32::NAN.to_bits(), 1f32.to_bits(), 1f32.to_bits()],
            [0, f32::INFINITY.to_bits(), 1f32.to_bits()],
            [0, 1, 1],
        ] {
            assert!(
                resources
                    .affine_image(&ImageAffine {
                        transform,
                        ..valid.clone()
                    })
                    .is_err()
            );
            assert_eq!(resources.frame(1, 0).unwrap().surface, original);
        }
        assert!(
            resources
                .affine_image(&ImageAffine {
                    source_rect: [1, 1, 2, 2],
                    ..valid.clone()
                })
                .is_err()
        );
        assert!(
            resources
                .affine_image(&ImageAffine {
                    destination: [1, 1],
                    ..valid
                })
                .is_err()
        );
        assert_eq!(resources.frame(1, 0).unwrap().surface, original);
        for flags in [0, 0x80000000, 0xc0000000] {
            resources.create_surface(1, 2, 2, flags).unwrap();
            resources.create_surface(2, 2, 2, flags).unwrap();
            resources
                .fill_surface(2, [0, 0, 2, 2], [11, 22, 33])
                .unwrap();
            let copy = SurfaceCopy {
                destination: SurfacePoint {
                    id: 1,
                    x: -1,
                    y: -1,
                },
                source: SurfacePoint { id: 2, x: 0, y: 0 },
                size: [3, 3],
            };
            resources.pixelate_surface(&copy, 2).unwrap();
            assert_eq!(
                resources.surface(1).unwrap().rgba,
                [11, 22, 33, 255].repeat(4)
            );
            let before = resources.surface(1).unwrap();
            assert!(resources.pixelate_surface(&copy, i32::MAX).is_err());
            assert_eq!(resources.surface(1).unwrap(), before);
        }
    }
}
