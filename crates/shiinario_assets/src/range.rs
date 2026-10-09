// SPDX-License-Identifier: GPL-2.0-only
// Port of GARbro KogadoCocotte.cs, C# port by morkt (2014).
// Original range coder: juicy.gt. QSModel: Michael Schindler (1997, 1998, 2000).
// See THIRD_PARTY_NOTICES.md and LICENSES/GPL-2.0-only.txt.
use anyhow::{Context, Result, ensure};

struct Model {
    cumulative: [u32; 258],
    frequencies: [u32; 257],
    interval: u32,
    left: u32,
    next: u32,
    increment: u32,
}
impl Model {
    fn new() -> Self {
        let mut frequencies = [15; 257];
        frequencies[..241].fill(16);
        let mut result = Self {
            cumulative: [0; 258],
            frequencies,
            interval: 18,
            left: 0,
            next: 0,
            increment: 0,
        };
        result.cumulative[257] = 4096;
        result.rescale();
        result
    }
    fn rescale(&mut self) {
        if self.next > 0 {
            self.increment += 1;
            self.left = self.next;
            self.next = 0;
            return;
        }
        self.interval = (self.interval * 2).min(2000);
        let mut cumulative = 4096;
        let mut missing = 4096;
        for i in (1..257).rev() {
            cumulative -= self.frequencies[i];
            self.cumulative[i] = cumulative;
            self.frequencies[i] = self.frequencies[i] >> 1 | 1;
            missing -= self.frequencies[i];
        }
        self.frequencies[0] = self.frequencies[0] >> 1 | 1;
        missing -= self.frequencies[0];
        self.increment = missing / self.interval;
        self.next = missing % self.interval;
        self.left = self.interval - self.next;
    }
    fn update(&mut self, symbol: usize) {
        if self.left == 0 {
            self.rescale();
        }
        self.left -= 1;
        self.frequencies[symbol] += self.increment;
    }
}

/// WARC 1.2–1.6 use a 257-symbol quasistatic range stream for the index.
pub(crate) fn decode(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    ensure!(input.len() >= 2, "truncated WARC range header");
    let mut model = Model::new();
    let mut pos = 2;
    let mut buffer = input[1];
    let mut low = u32::from(buffer >> 1);
    let mut range = 128u32;
    let mut output = Vec::new();
    loop {
        while range <= 0x800000 {
            low = low << 8 | u32::from(buffer << 7);
            buffer = *input.get(pos).context("truncated WARC range stream")?;
            pos += 1;
            low |= u32::from(buffer >> 1);
            range <<= 8;
        }
        let scale = range >> 12;
        ensure!(scale != 0, "invalid WARC range state");
        let slot = (low / scale).min(4095);
        let symbol = model.cumulative.partition_point(|&v| v <= slot) - 1;
        if symbol == 256 {
            return Ok(output);
        }
        ensure!(output.len() < limit, "WARC range output exceeds limit");
        output.push(symbol as u8);
        let start = model.cumulative[symbol];
        let end = model.cumulative[symbol + 1];
        let offset = scale * start;
        low = low.wrapping_sub(offset);
        range = if end < 4096 {
            scale * (end - start)
        } else {
            range - offset
        };
        model.update(symbol);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_garbro_verified_range_stream() {
        let input = include_bytes!("../tests/fixtures/garbro/range.rng");
        let expected: Vec<_> = (0..16000)
            .map(|i| ((i * 37 + i / 11) & 255) as u8)
            .collect();
        assert_eq!(decode(input, expected.len()).unwrap(), expected);
        assert!(decode(input, 15999).is_err());
        for len in [0, 1, 2, 3, 8, 100, input.len() / 2] {
            assert!(decode(&input[..len], 16000).is_err());
        }
    }
}
