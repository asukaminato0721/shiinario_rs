//! From GARbro ArcFormats/ShiinaRio/ImageMI4.cs and ImageCHD.cs. Copyright (C) 2015-2017 morkt (MIT).
use super::{Frame, FrameInfo, bytes, word};
use anyhow::{Context, Result, ensure};

fn dimensions(width: u32, height: u32) -> Result<()> {
    ensure!(
        width > 0
            && height > 0
            && width <= 16384
            && height <= 16384
            && u64::from(width) * u64::from(height) <= 16_777_216,
        "invalid image dimensions {width}x{height}"
    );
    Ok(())
}
pub(super) fn info(data: &[u8]) -> Result<FrameInfo> {
    let chd = data.starts_with(b"CHD\0");
    let offset = if chd {
        let count = word(data, 4)? as usize;
        ensure!(count <= 0xfffff, "CHD frame count exceeds cap");
        bytes(data, 12, count * 4)?;
        (0..count)
            .map(|i| word(data, 12 + i * 4).map(|v| v as usize))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find(|&v| v != 0)
            .context("CHD has no image")?
    } else {
        ensure!(data.starts_with(b"MAI4"), "unsupported image signature");
        8
    };
    let width = word(data, offset)?;
    let height = word(data, offset + 4)?;
    dimensions(width, height)?;
    Ok(FrameInfo {
        index: 0,
        width,
        height,
        x: if chd {
            word(data, offset + 8)? as i32
        } else {
            0
        },
        y: if chd {
            word(data, offset + 12)? as i32
        } else {
            0
        },
        incremental: false,
        offset,
    })
}
pub(super) fn decode(data: &[u8], index: usize) -> Result<Frame> {
    ensure!(index == 0, "image has only frame 0");
    let info = info(data)?;
    let pixels = info.width as usize * info.height as usize;
    let rgba = if data.starts_with(b"CHD\0") {
        chd(data, &info)?
    } else {
        mi4(data, &info, true).or_else(|_| mi4(data, &info, false))?
    };
    Ok(Frame {
        info,
        rgba,
        methods: vec![2; pixels],
    })
}
fn chd(data: &[u8], info: &FrameInfo) -> Result<Vec<u8>> {
    let width = info.width as usize;
    let mut rgba = vec![0; width * info.height as usize * 4];
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    for y in 0..info.height as usize {
        let mut pos = word(data, info.offset + 16 + y * 4)? as usize;
        let mut left = width;
        let mut dst = y * width * 4;
        while left != 0 {
            let skip = run(data, &mut pos, 255)?;
            ensure!(skip <= left, "CHD skip exceeds row");
            left -= skip;
            if left == 0 {
                break;
            }
            let count = run(data, &mut pos, 0)?;
            ensure!(count > 0 && count <= left, "CHD literal run exceeds row");
            // GARbro decreases the remaining width for skips but does not move dst.
            for &value in bytes(data, pos, count)? {
                let gray = value.wrapping_add(0x57);
                rgba[dst..dst + 4].copy_from_slice(&[gray, gray, gray, 255]);
                dst += 4;
            }
            pos += count;
            left -= count;
        }
    }
    Ok(rgba)
}
fn run(data: &[u8], pos: &mut usize, extended: u8) -> Result<usize> {
    let value = bytes(data, *pos, 1)?[0];
    *pos += 1;
    if value != extended {
        return Ok(value as usize);
    }
    let result = u16::from_le_bytes(bytes(data, *pos, 2)?.try_into()?) as usize;
    *pos += 2;
    Ok(result)
}
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u32,
    left: u8,
}
impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut b = Self {
            data,
            pos: 16,
            cache: 0,
            left: 0,
        };
        b.fill();
        b
    }
    fn fill(&mut self) {
        for _ in 0..4 {
            if let Some(&v) = self.data.get(self.pos) {
                self.cache = (self.cache >> 8) | (u32::from(v) << 24);
                self.left += 8;
                self.pos += 1;
            } else {
                break;
            }
        }
    }
    fn get(&mut self, n: u8) -> Result<u8> {
        let mut value = 0;
        for _ in 0..n {
            ensure!(self.left != 0, "MI4 truncated control bits");
            value = (value << 1) | (self.cache >> 31) as u8;
            self.cache <<= 1;
            self.left -= 1;
            if self.left == 0 {
                self.fill();
            }
        }
        Ok(value)
    }
}
fn mi4(data: &[u8], info: &FrameInfo, second: bool) -> Result<Vec<u8>> {
    let mut bits = Bits::new(data);
    let width = info.width as usize;
    let mut pixels: Vec<[u8; 3]> = Vec::with_capacity(width * info.height as usize);
    let mut color = [0u8; 3];
    for dst in 0..width * info.height as usize {
        if bits.get(1)? == 0 {
            if bits.get(1)? != 0 {
                if !second || bits.pos < data.len() {
                    color.copy_from_slice(bytes(data, bits.pos, 3)?);
                    bits.pos += 3;
                }
            } else {
                let n = if bits.get(1)? != 0 {
                    2
                } else if bits.get(1)? != 0 {
                    3
                } else if bits.get(1)? != 0 {
                    4
                } else {
                    5
                };
                let value = bits.get(n)?;
                let maximum = (1 << n) - 1;
                let bias = (1 << (n - 1)) - 1;
                let mut reference = None;
                let mut delta = false;
                if value == maximum && n == 2 {
                    reference = Some(0isize);
                } else if value == maximum && n == 3 {
                    reference = Some(if second { 0 } else { 1 });
                    delta = second;
                } else if value == maximum && n == 4 && !second {
                    reference = Some(-1);
                } else {
                    color[0] = color[0].wrapping_add(value.wrapping_sub(bias));
                    let green = bits.get(n)?;
                    if second && n == 2 && green == 3 {
                        reference = Some(if bits.get(1)? != 0 { -1 } else { 1 });
                    } else {
                        color[1] = color[1].wrapping_add(green.wrapping_sub(bias));
                        color[2] = color[2].wrapping_add(bits.get(n)?.wrapping_sub(bias));
                    }
                }
                if let Some(dx) = reference {
                    let src = (dst as isize - width as isize + dx)
                        .try_into()
                        .ok()
                        .and_then(|i: usize| pixels.get(i))
                        .context("MI4 invalid previous-row reference")?;
                    color = *src;
                    if delta {
                        for c in &mut color {
                            *c = c.wrapping_add(bits.get(n)?.wrapping_sub(bias));
                        }
                    }
                }
            }
        }
        pixels.push(color);
    }
    Ok(pixels
        .into_iter()
        .flat_map(|[b, g, r]| [r, g, b, 255])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mai(bits: u32, tail: &[u8], width: u32, height: u32) -> Vec<u8> {
        let mut data = b"MAI4\0\0\0\0".to_vec();
        data.extend(width.to_le_bytes());
        data.extend(height.to_le_bytes());
        data.extend(bits.to_le_bytes());
        data.extend(tail);
        data
    }
    #[test]
    fn original_garbro_mi4_predictors() {
        for (data, expected, second) in [
            (
                &include_bytes!("../tests/fixtures/garbro/mi4-1.mi4")[..],
                &include_bytes!("../tests/fixtures/garbro/mi4-1.bgr")[..],
                false,
            ),
            (
                &include_bytes!("../tests/fixtures/garbro/mi4-2.mi4")[..],
                &include_bytes!("../tests/fixtures/garbro/mi4-2.bgr")[..],
                true,
            ),
        ] {
            let rgba: Vec<_> = expected
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], 255])
                .collect();
            assert_eq!(mi4(data, &info(data).unwrap(), second).unwrap(), rgba);
        }
    }
    #[test]
    fn mi4_literal_repeat_and_row() {
        let data = mai(0b011001111 << 23, &[3, 5, 7], 2, 2);
        for second in [false, true] {
            assert_eq!(
                mi4(&data, &info(&data).unwrap(), second).unwrap(),
                [7, 5, 3, 255].repeat(4)
            );
        }
    }
    #[test]
    fn mi4_first_variant_fallback() {
        // The second variant interprets green=3 as an invalid previous-row copy.
        let data = mai(0b001011101 << 23, &[], 1, 1);
        assert!(mi4(&data, &info(&data).unwrap(), true).is_err());
        assert_eq!(decode(&data, 0).unwrap().rgba, [0, 2, 0, 255]);
    }
    #[test]
    fn chd_gray_and_signed_offsets() {
        let mut data = b"CHD\0".to_vec();
        for n in [1u32, 0, 16, 3, 1, u32::MAX, 2, 36] {
            data.extend(n.to_le_bytes());
        }
        data.extend([0, 2, 0, 0xff, 1]);
        let frame = decode(&data, 0).unwrap();
        assert_eq!(frame.info.x, -1);
        assert_eq!(
            frame.rgba,
            [0x57, 0x57, 0x57, 255, 0x56, 0x56, 0x56, 255, 0, 0, 0, 255]
        );
        data[36] = 4;
        assert!(decode(&data, 0).is_err());
    }
}
