//! Checked ports of the v2.47 RanSem helpers. Each port requires an exact hash.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub(super) struct Counter {
    pub divisor: u32,
    value: u32,
    epoch: u32,
}
impl Default for Counter {
    fn default() -> Self {
        Self {
            divisor: 1,
            value: 0,
            epoch: 0,
        }
    }
}
impl Counter {
    pub fn reset(&mut self, value: u32, epoch: u32) {
        self.value = value;
        self.epoch = epoch;
    }
}

type NativeWrites = Vec<(MemoryRange, Vec<u8>)>;

impl BinaryVm {
    pub(super) fn ransem_native_helper(&self, address: u32) -> Result<Option<NativeWrites>> {
        let matches = |length, hash: &str| {
            self.memory_read(address, length)
                .is_ok_and(|code| format!("{:x}", Sha256::digest(code)) == hash)
        };
        if matches(
            125,
            "418d5a2e7a4b183bb433375688944151d258ff892e030f6f442b040b5dc3ad60",
        ) {
            let source = self.banks[&12][0];
            let target = self.banks[&12][1];
            let destination =
                self.memory_range(target, native_helper::THUMBNAIL_COPY_TARGET_SIZE)?;
            ensure!(
                u64::from(source) + native_helper::SOURCE_SIZE as u64 <= u64::from(target)
                    || u64::from(target) + native_helper::THUMBNAIL_COPY_TARGET_SIZE as u64
                        <= u64::from(source),
                "native thumbnail regions overlap"
            );
            let thumbnail =
                native_helper::thumbnail(&self.memory_read(source, native_helper::SOURCE_SIZE)?)?;
            let mut output = self.memory_read(target, native_helper::THUMBNAIL_COPY_TARGET_SIZE)?;
            native_helper::copy_thumbnail_rows(&thumbnail, &mut output)?;
            Ok(Some(vec![(destination, output)]))
        } else if matches(
            814,
            "f3ceeb913df8a3490138ae3c991c5fa3ed0f1f44a5d4e5c4c25fcaa315fd019b",
        ) {
            let mut args = [0; 13];
            for (index, value) in args.iter_mut().enumerate() {
                *value = self.banks[&8][self.bank_index(8, index as u16)?];
            }
            self.ransem_scale(args).map(Some)
        } else {
            Ok(None)
        }
    }

    fn ransem_scale(&self, args: [u32; 13]) -> Result<NativeWrites> {
        let [
            dst,
            dst_stride,
            dx,
            dy,
            dr,
            db,
            src,
            src_stride,
            sx,
            sy,
            sr,
            sb,
            scratch,
        ] = args;
        let extent = |end: u32, start: u32| -> Result<i64> {
            ensure!(
                end >= start && end <= 262144,
                "native scaling rectangle exceeds bounds"
            );
            Ok(i64::from(end - start))
        };
        let [dw, dh, sw, sh] = [
            extent(dr, dx)?,
            extent(db, dy)?,
            extent(sr, sx)?,
            extent(sb, sy)?,
        ];
        if [dw, dh, sw, sh].contains(&0) {
            return Ok(Vec::new());
        }
        ensure!(
            dw >= 16 && dh >= 16,
            "native scaling destination is smaller than one pixel"
        );
        let [columns, rows] = [(dw >> 4) as usize, (dh >> 4) as usize];
        ensure!(
            columns * rows <= 4_000_000,
            "native scaling exceeds pixel limit"
        );
        ensure!(
            dst_stride >= columns as u32 * 4 && src_stride > 0,
            "invalid native scaling stride"
        );
        let pointer = |base: u32, offset: u64| {
            base.checked_add(u32::try_from(offset)?)
                .context("native scaling pointer overflow")
        };
        let target = pointer(
            dst,
            u64::from(dy >> 4) * u64::from(dst_stride) + u64::from(dx >> 4) * 4,
        )?;
        let source = pointer(
            src,
            u64::from(sy >> 4) * u64::from(src_stride) + u64::from(sx >> 4) * 4,
        )?;
        let length = (rows - 1)
            .checked_mul(dst_stride as usize)
            .and_then(|n| n.checked_add(columns * 4))
            .context("native scaling size overflow")?;
        let range = self.memory_range(target, length)?;
        let table_length = sw as usize * 8;
        let table_range = self.memory_range(scratch, table_length)?;
        ensure!(
            u64::from(scratch) + table_length as u64 <= u64::from(target)
                || u64::from(target) + length as u64 <= u64::from(scratch),
            "native scratch overlaps destination"
        );
        let mut table = Vec::with_capacity(table_length);
        for index in 0..sw {
            let weight = (index * 257 / sw) as u16;
            for _ in 0..4 {
                table.extend(weight.to_le_bytes());
            }
        }
        let mut output = self.memory_read(target, length)?;
        let mix = |a: [u8; 4], b: [u8; 4], wa: i64, wb: i64| -> [u8; 4] {
            std::array::from_fn(|i| {
                ((u16::from(a[i])
                    .wrapping_mul(wa as u16)
                    .wrapping_add(u16::from(b[i]).wrapping_mul(wb as u16)))
                    >> 8)
                    .min(255) as u8
            })
        };
        let mut source_row = source;
        let mut y_remaining = dh - ((i64::from(sy & 15) * sh) >> 4);
        let x_initial = dw - ((i64::from(sx & 15) * sw) >> 4);
        for row in 0..rows {
            let vertical = y_remaining < sh;
            let weights = if vertical {
                [y_remaining * 256 / sh, (sh - y_remaining) * 256 / sh]
            } else {
                [256, 0]
            };
            ensure!(
                weights.iter().all(|w| (0..=256).contains(w)),
                "native vertical weight outside bounds"
            );
            if !vertical {
                y_remaining -= sh;
            }
            let mut x_remaining = x_initial;
            let mut pixel = source_row;
            for column in 0..columns {
                let read = |at: u32| -> Result<[u8; 4]> {
                    let offset = u64::from(at).checked_sub(u64::from(target));
                    if let Some(offset) = offset.filter(|n| *n + 4 <= length as u64) {
                        Ok(output[offset as usize..offset as usize + 4].try_into()?)
                    } else if let Some(offset) = u64::from(at)
                        .checked_sub(u64::from(scratch))
                        .filter(|n| *n + 4 <= table.len() as u64)
                    {
                        Ok(table[offset as usize..offset as usize + 4].try_into()?)
                    } else {
                        Ok(self.memory_read(at, 4)?.as_slice().try_into()?)
                    }
                };
                let sample = |at| -> Result<[u8; 4]> {
                    let a = read(at)?;
                    Ok(if vertical {
                        mix(
                            a,
                            read(pointer(at, u64::from(src_stride))?)?,
                            weights[0],
                            weights[1],
                        )
                    } else {
                        a
                    })
                };
                let value = if x_remaining >= sw {
                    let value = sample(pixel)?;
                    x_remaining -= sw;
                    if x_remaining == 0 {
                        pixel = pointer(pixel, 4)?;
                        x_remaining = dw;
                    }
                    value
                } else {
                    ensure!(
                        x_remaining > 0 && x_remaining < sw,
                        "native horizontal weight outside table"
                    );
                    let value = mix(
                        sample(pixel)?,
                        sample(pointer(pixel, 4)?)?,
                        x_remaining * 257 / sw,
                        (sw - x_remaining) * 257 / sw,
                    );
                    pixel = pointer(pixel, 4)?;
                    x_remaining += dw - sw;
                    value
                };
                let offset = row * dst_stride as usize + column * 4;
                output[offset..offset + 4].copy_from_slice(&value);
            }
            if vertical {
                source_row = pointer(source_row, u64::from(src_stride))?;
                y_remaining += dh - sh;
            } else if y_remaining == 0 {
                source_row = pointer(source_row, u64::from(src_stride))?;
                y_remaining = dh;
            }
        }
        Ok(vec![(table_range, table), (range, output)])
    }

    /// Map a raw image frame once. All later writes use the same shared region.
    pub fn respond_image_pixels(&mut self, memory: SharedMemory, offset: usize) -> Result<u32> {
        let Some((
            Event::Platform {
                request: PlatformRequest::ImagePixels { image, frame, .. },
                ..
            },
            destination,
        )) = &self.pending
        else {
            bail!("no pending image pixel request");
        };
        let key = (*image, *frame);
        let destination = destination.context("image pointer requires a destination")?;
        ensure!(offset < memory.len(), "image pixel offset exceeds memory");
        let existing = self
            .shared_regions
            .iter()
            .find(|(_, region)| region.same_region(&memory))
            .map(|(&base, _)| base);
        let base = if let Some(base) = existing {
            base
        } else {
            ensure!(
                self.shared_regions
                    .values()
                    .map(SharedMemory::len)
                    .sum::<usize>()
                    + memory.len()
                    <= 256 * 1024 * 1024,
                "mapped resources exceed 256 MiB"
            );
            let base = self.next_shared;
            let next = base
                .checked_add(((memory.len() as u32 + 4095) & !4095) + 4096)
                .context("shared memory address overflow")?;
            ensure!(next < 0xf000_0000, "shared memory address space exhausted");
            self.shared_regions.insert(base, memory);
            self.next_shared = next;
            base
        };
        let address = base
            .checked_add(offset as u32)
            .context("image pixel pointer overflow")?;
        self.write(destination, address)?;
        if let Some(old) = self
            .raw_image_regions
            .insert(key, base)
            .filter(|old| *old != base)
        {
            self.shared_regions.remove(&old);
        }
        self.pending = None;
        Ok(address)
    }
}
