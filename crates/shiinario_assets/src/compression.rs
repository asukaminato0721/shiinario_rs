// Ported from GARbro ArcWARC.cs, Copyright (C) 2015-2017 morkt (MIT).
use anyhow::{Result, bail, ensure};

pub const MAX_OUTPUT: usize = 256 * 1024 * 1024;
pub fn zlib(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    ensure!(
        limit <= MAX_OUTPUT,
        "decompression limit exceeds safety cap"
    );
    let mut output = Vec::new();
    let mut decoder = flate2::Decompress::new(true);
    loop {
        let mut chunk = [0u8; 65536];
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder.decompress(
            &input[before_in as usize..],
            &mut chunk,
            flate2::FlushDecompress::None,
        )?;
        let produced = (decoder.total_out() - before_out) as usize;
        ensure!(
            output.len() + produced <= limit,
            "zlib output exceeds limit {limit}"
        );
        output.extend_from_slice(&chunk[..produced]);
        if status == flate2::Status::StreamEnd {
            return Ok(output);
        }
        ensure!(
            decoder.total_in() != before_in || produced != 0,
            "truncated zlib stream"
        );
    }
}
struct Bits<'a> {
    input: &'a [u8],
    pos: usize,
    cache: u32,
    left: u32,
    eager: bool,
}
impl<'a> Bits<'a> {
    fn new(input: &'a [u8], eager: bool) -> Self {
        Self {
            input,
            pos: 0,
            cache: 0,
            left: 0,
            eager,
        }
    }
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .input
            .get(self.pos)
            .ok_or_else(|| anyhow::anyhow!("truncated compressed stream at {:#x}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }
    fn fill(&mut self) -> Result<()> {
        let n = (self.input.len() - self.pos).min(4);
        ensure!(n > 0, "truncated bit stream");
        ensure!(!self.eager || n == 4, "truncated YLZ control word");
        self.cache = 0;
        for i in 0..n {
            self.cache |= (self.byte()? as u32) << (8 * i);
        }
        self.left = 32;
        Ok(())
    }
    fn bits(&mut self, n: u32) -> Result<u32> {
        let mut result = 0;
        for _ in 0..n {
            if self.left == 0 {
                self.fill()?;
            }
            self.left -= 1;
            result = (result << 1) | ((self.cache >> self.left) & 1);
            if self.eager && self.left == 0 {
                self.fill()?;
            }
        }
        Ok(result)
    }
    fn bit(&mut self) -> Result<bool> {
        Ok(self.bits(1)? != 0)
    }
}
pub fn yh1(input: &[u8], size: usize) -> Result<Vec<u8>> {
    ensure!(size <= MAX_OUTPUT, "YH1 output exceeds cap");
    let mut bits = Bits::new(input, false);
    let mut tree = Vec::<[usize; 2]>::new();
    fn node(b: &mut Bits<'_>, t: &mut Vec<[usize; 2]>, depth: usize) -> Result<usize> {
        ensure!(depth <= 255, "Huffman tree too deep");
        if !b.bit()? {
            return Ok(b.bits(8)? as usize);
        }
        ensure!(t.len() < 255, "Huffman tree too large");
        let i = t.len();
        t.push([0, 0]);
        let l = node(b, t, depth + 1)?;
        let r = node(b, t, depth + 1)?;
        t[i] = [l, r];
        Ok(i + 256)
    }
    let root = node(&mut bits, &mut tree, 0)?;
    let mut out = Vec::with_capacity(size);
    for _ in 0..size {
        let mut symbol = root;
        while symbol >= 256 {
            symbol = tree[symbol - 256][bits.bit()? as usize];
        }
        out.push(symbol as u8);
    }
    Ok(out)
}
pub fn ylz(input: &[u8], size: usize) -> Result<Vec<u8>> {
    ensure!(size <= MAX_OUTPUT, "YLZ output exceeds cap");
    let mut b = Bits::new(input, true);
    b.fill()?;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        if b.bit()? {
            out.push(b.byte()?);
            continue;
        }
        let next = b.bit()?;
        let mut offset = b.byte()? as i32 | !0xffff;
        let mut ah = 255i32;
        let count = if next {
            if b.bit()? {
                ah = (ah << 1) | b.bits(1)? as i32;
            } else if b.bit()? {
                ah = (ah << 1) | b.bits(1)? as i32;
                offset -= 0x200;
            } else if b.bit()? {
                ah = (ah << 2) | b.bits(2)? as i32;
                offset -= 0x400;
            } else if b.bit()? {
                ah = (ah << 3) | b.bits(3)? as i32;
                offset -= 0x800;
            } else {
                ah = (ah << 4) | b.bits(4)? as i32;
                offset -= 0x1000;
            }
            if b.bit()? {
                3
            } else if b.bit()? {
                4
            } else if b.bit()? {
                5 + b.bits(1)? as usize
            } else if b.bit()? {
                7 + b.bits(2)? as usize
            } else if b.bit()? {
                11 + b.bits(3)? as usize
            } else {
                19 + b.byte()? as usize
            }
        } else if b.bit()? {
            ah = ((ah << 3) | b.bits(3)? as i32) - 1;
            ah &= 255;
            2
        } else if offset & 255 == 255 {
            bail!("premature YLZ terminator: {} of {size} bytes", out.len());
        } else {
            2
        };
        offset += (ah & 255) << 8;
        ensure!(
            offset < 0 && (-offset) as usize <= out.len(),
            "invalid YLZ backreference {offset} at {}",
            out.len()
        );
        ensure!(count <= size - out.len(), "YLZ match exceeds output");
        for _ in 0..count {
            out.push(out[(out.len() as i64 + offset as i64) as usize]);
        }
    }
    Ok(out)
}
pub fn unpack(sig: u32, input: &mut [u8], size: usize) -> Result<Vec<u8>> {
    ensure!(input.len() > 8, "entry header truncated");
    let method = sig & 0xffffff;
    if !matches!(method, 0x314859 | 0x4b5059 | 0x5a4c59) {
        return Ok(input.to_vec());
    }
    ensure!(size <= MAX_OUTPUT, "entry output exceeds cap");
    if input[3] != 0 {
        let key: u32 = match method {
            0x314859 => 0x6393528e ^ 0x4b4d,
            0x4b5059 => !0x4b4d4b4d,
            0x5a4c59 => 0x4b4d4b4d,
            _ => return Ok(input.to_vec()),
        };
        let end = input.len() / 4 * 4;
        for (i, byte) in input[8..end].iter_mut().enumerate() {
            *byte ^= key.to_le_bytes()[i % 4];
        }
        if method == 0x4b5059 {
            for d in &mut input[end..] {
                *d ^= key as u8;
            }
        }
        if method == 0x5a4c59 {
            for (i, d) in input[end..].iter_mut().enumerate() {
                *d ^= key.to_le_bytes()[i];
            }
        }
    }
    match method {
        0x314859 => yh1(&input[8..], size),
        0x4b5059 => {
            let out = zlib(&input[8..], size)?;
            ensure!(
                out.len() == size,
                "YPK length mismatch: {} != {size}",
                out.len()
            );
            Ok(out)
        }
        0x5a4c59 => ylz(&input[8..], size),
        _ => Ok(input.to_vec()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ylz_literals_and_overlapping_match() {
        let mut encoded = 0xc0000000u32.to_le_bytes().to_vec();
        encoded.extend_from_slice(&[b'A', b'B', 0xfe]);
        assert_eq!(ylz(&encoded, 4).unwrap(), b"ABAB");
        assert!(ylz(&encoded, 3).is_err());
    }
    #[test]
    fn strict_zlib_bounds_and_checksum() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"synthetic").unwrap();
        let data = encoder.finish().unwrap();
        assert_eq!(zlib(&data, 9).unwrap(), b"synthetic");
        assert!(zlib(&data, 8).is_err());
        assert!(zlib(&data[..data.len() - 1], 9).is_err());
        let mut bad = data;
        let end = bad.len() - 1;
        bad[end] ^= 1;
        assert!(zlib(&bad, 9).is_err());
    }
    #[test]
    fn huffman_single_symbol() {
        let word = (65u32) << 23;
        assert_eq!(yh1(&word.to_le_bytes(), 4).unwrap(), b"AAAA");
    }
    #[test]
    fn rejects_truncated_and_invalid_matches() {
        assert!(yh1(&[], 2).is_err());
        assert!(ylz(&[0, 0, 0, 0, 0], 2).is_err());
        assert!(zlib(&[0x78], 2).is_err());
    }
    #[test]
    fn rejects_output_bombs() {
        assert!(yh1(&[], MAX_OUTPUT + 1).is_err());
        assert!(ylz(&[], MAX_OUTPUT + 1).is_err());
    }
}
