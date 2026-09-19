//! Checked S25 frame decoder, including incremental shared rows.
// Based on GARbro ImageS25.cs, Copyright (C) 2015 morkt (MIT).
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::HashMap;
#[derive(Debug, Clone, Serialize)]
pub struct FrameInfo {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub incremental: bool,
    #[serde(skip)]
    offset: usize,
}
#[derive(Debug, Clone)]
pub struct Frame {
    pub info: FrameInfo,
    pub rgba: Vec<u8>,
}
fn bytes(data: &[u8], p: usize, n: usize) -> Result<&[u8]> {
    data.get(p..p.checked_add(n).context("S25 offset overflow")?)
        .context("S25 truncated data")
}
fn word(data: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, p, 4)?.try_into()?))
}
pub fn frames(data: &[u8]) -> Result<Vec<FrameInfo>> {
    ensure!(data.starts_with(b"S25\0"), "not an S25 image");
    let count = word(data, 4)? as usize;
    ensure!(count <= 1_000_000, "S25 frame count exceeds cap");
    bytes(data, 8, count * 4)?;
    let mut frames = Vec::new();
    let mut total_rows = 0u64;
    for index in 0..count {
        let offset = word(data, 8 + index * 4)? as usize;
        if offset == 0 {
            continue;
        }
        let width = word(data, offset)?;
        let height = word(data, offset + 4)?;
        ensure!(
            width > 0
                && height > 0
                && width <= 16384
                && height <= 16384
                && width as u64 * height as u64 <= 16_777_216,
            "S25 invalid dimensions {width}x{height}"
        );
        total_rows += height as u64;
        ensure!(
            total_rows <= 4_000_000,
            "S25 total row references exceed cap"
        );
        let f = FrameInfo {
            index,
            width,
            height,
            x: word(data, offset + 8)? as i32,
            y: word(data, offset + 12)? as i32,
            incremental: word(data, offset + 16)? & 0x80000000 != 0,
            offset,
        };
        bytes(data, offset + 20, height as usize * 4)?;
        frames.push(f);
    }
    Ok(frames)
}
pub fn decode(data: &[u8], index: usize) -> Result<Frame> {
    let all = frames(data)?;
    let info = all
        .iter()
        .find(|f| f.index == index)
        .context("S25 frame not present")?
        .clone();
    let mut counts = HashMap::new();
    if info.incremental {
        for f in &all {
            for y in 0..f.height as usize {
                *counts
                    .entry(word(data, f.offset + 20 + y * 4)? as usize)
                    .or_insert(0usize) += 1;
            }
        }
    }
    let mut rgba = vec![0; info.width as usize * info.height as usize * 4];
    for y in 0..info.height as usize {
        let pos = word(data, info.offset + 20 + y * 4)? as usize;
        let len = u16::from_le_bytes(bytes(data, pos, 2)?.try_into()?) as usize;
        let skip = pos & 1;
        ensure!(len >= skip, "S25 invalid row length");
        let mut row = bytes(data, pos + 2 + skip, len - skip)?.to_vec();
        let repeats = if info.incremental { counts[&pos] } else { 0 };
        decode_row(
            &mut row,
            &mut rgba[y * info.width as usize * 4..(y + 1) * info.width as usize * 4],
            repeats,
        )
        .with_context(|| format!("S25 frame {index} row {y}"))?;
    }
    Ok(Frame { info, rgba })
}
/// Original button hit testing uses the RLE method, not the decoded alpha.
/// In particular, an alpha-zero literal and a method-1 run are both hits.
pub fn hit_test(data: &[u8], index: usize, x: u32, y: u32) -> Result<bool> {
    let all = frames(data)?;
    let frame = all
        .iter()
        .find(|f| f.index == index)
        .context("S25 frame not present")?;
    if x >= frame.width || y >= frame.height {
        return Ok(false);
    }
    let pos = word(data, frame.offset + 20 + y as usize * 4)? as usize;
    if pos == 0 {
        return Ok(false);
    }
    let len = u16::from_le_bytes(bytes(data, pos, 2)?.try_into()?) as usize;
    let skip = pos & 1;
    ensure!(len >= skip, "S25 invalid row length");
    let row = bytes(data, pos + 2 + skip, len - skip)?;
    let (mut p, mut end) = (0usize, 0u32);
    loop {
        p += p & 1;
        let control = u16::from_le_bytes(bytes(row, p, 2)?.try_into()?);
        p += 2 + ((control >> 11) & 3) as usize;
        let mut count = u32::from(control & 0x7ff);
        if count == 0 {
            count = word(row, p)?;
            p += 4;
        }
        ensure!(count > 0, "S25 zero-length run");
        let method = control >> 13;
        let size = match method {
            2 => u64::from(count) * 3,
            3 => 3,
            4 => u64::from(count) * 4,
            5 => 4,
            _ => 0,
        };
        bytes(row, p, usize::try_from(size)?)?;
        end = end
            .checked_add(count)
            .context("S25 hit-test run overflow")?;
        if x < end {
            return Ok(method != 0);
        }
        p += usize::try_from(size)?;
    }
}
fn decode_row(row: &mut [u8], out: &mut [u8], repeat: usize) -> Result<()> {
    let (mut p, mut x) = (0usize, 0usize);
    while x < out.len() / 4 {
        p += p & 1;
        let control = u16::from_le_bytes(bytes(row, p, 2)?.try_into()?);
        p += 2;
        let method = control >> 13;
        p += ((control >> 11) & 3) as usize;
        let mut count = (control & 0x7ff) as usize;
        if count == 0 {
            count = word(row, p)? as usize;
            p += 4;
        }
        ensure!(count > 0, "S25 zero-length run");
        count = count.min(out.len() / 4 - x);
        let stride = match method {
            2 | 3 => 3,
            4 | 5 => 4,
            0 | 1 | 6 | 7 => 0,
            _ => unreachable!(),
        };
        let literal = method == 2 || method == 4;
        let n = if literal { count * stride } else { stride };
        bytes(row, p, n)?;
        if literal && repeat > 0 {
            // A row can be referenced many times; cap work on malformed inputs.
            ensure!(
                repeat <= 65536 && repeat.saturating_mul(n) <= 64 * 1024 * 1024,
                "S25 incremental work limit exceeded"
            );
            for _ in 0..repeat {
                for i in stride..n {
                    row[p + i] = row[p + i].wrapping_add(row[p + i - stride]);
                }
            }
        }
        if stride > 0 {
            for i in 0..count {
                let src = p + if literal { i * stride } else { 0 };
                let dst = (x + i) * 4;
                let bgr = src + if stride == 4 { 1 } else { 0 };
                out[dst..dst + 4].copy_from_slice(&[
                    row[bgr + 2],
                    row[bgr + 1],
                    row[bgr],
                    if stride == 4 { row[src] } else { 255 },
                ]);
            }
        }
        p += n;
        x += count;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hit_test_checks_row_bounds_and_rejects_zero_runs() {
        let mut data = b"S25\0".to_vec();
        for word in [1u32, 12, 1, 1, 0, 0, 0, 36] {
            data.extend(word.to_le_bytes());
        }
        data.extend([2, 0, 0, 0]);
        assert!(hit_test(&data, 0, 0, 0).is_err());
        assert!(!hit_test(&data, 0, 1, 0).unwrap());
        data[38..40].copy_from_slice(&[1, 0x80]);
        assert!(hit_test(&data, 0, 0, 0).is_err());
        data[32..36].fill(0);
        assert!(!hit_test(&data, 0, 0, 0).unwrap());
        data[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(hit_test(&data, 0, 0, 0).is_err());
    }
    #[test]
    fn colors_transparency_and_repeats() {
        let mut row = vec![2, 0x60, 1, 2, 3, 0, 1, 0xa0, 128, 4, 5, 6, 1, 0x20];
        let mut out = [0; 16];
        decode_row(&mut row, &mut out, 0).unwrap();
        assert_eq!(out, [3, 2, 1, 255, 3, 2, 1, 255, 6, 5, 4, 128, 0, 0, 0, 0]);
    }
    #[test]
    fn incremental_literal() {
        let mut row = vec![2, 0x40, 1, 2, 3, 4, 5, 6];
        let mut out = [0; 8];
        decode_row(&mut row, &mut out, 1).unwrap();
        assert_eq!(out, [3, 2, 1, 255, 9, 7, 5, 255]);
    }
    #[test]
    fn malformed_rows() {
        assert!(decode_row(&mut [0, 0], &mut [0; 4], 0).is_err());
        assert!(frames(b"S25\0\xff\xff\xff\xff").is_err());
        assert!(decode_row(&mut [1, 0x80], &mut [0; 4], 0).is_err());
    }
}
