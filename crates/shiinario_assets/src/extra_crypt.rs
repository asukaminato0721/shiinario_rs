//! From GARbro ArcFormats/ShiinaRio/WarcEncryption.cs. Copyright (C) 2015-2017 morkt (MIT).
use crate::nrbf::{Document, Value};
use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtraCrypt {
    kind: Kind,
    seed: u32,
    table: Vec<u8>,
    min: usize,
    post: usize,
    size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ShojoMama,
    YuruPlus,
    Testament,
    MakiFes,
    KeyAdler,
    Majime,
    Nyaru,
    Alcot,
    Dodakure,
    Jokers,
    PreAdler,
    PostAdler,
    Binbo,
    Count,
    AltCount,
    Ushimitsu,
}

impl ExtraCrypt {
    pub(crate) fn from_record(doc: &Document<'_>, value: &Value<'_>) -> Result<Option<Self>> {
        let value = doc.resolve(value)?;
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        let Value::Object(name, _) = value else {
            bail!("invalid extra crypt record")
        };
        let kind = match name.strip_prefix("GameRes.Formats.ShiinaRio.") {
            Some("ShojoMamaCrypt") => Kind::ShojoMama,
            Some("YuruPlusCrypt") => Kind::YuruPlus,
            Some("TestamentCrypt") => Kind::Testament,
            Some("MakiFesCrypt") => Kind::MakiFes,
            Some("KeyAdlerCrypt") => Kind::KeyAdler,
            Some("MajimeCrypt") => Kind::Majime,
            Some("NyaruCrypt") => Kind::Nyaru,
            Some("AlcotCrypt") => Kind::Alcot,
            Some("DodakureCrypt") => Kind::Dodakure,
            Some("JokersCrypt") => Kind::Jokers,
            Some("PreAdlerCrypt") => Kind::PreAdler,
            Some("PostAdlerCrypt") => Kind::PostAdler,
            Some("BinboCrypt") => Kind::Binbo,
            Some("CountCrypt") => Kind::Count,
            Some("AltCountCrypt") => Kind::AltCount,
            Some("UshimitsuCrypt") => Kind::Ushimitsu,
            _ => bail!("unknown extra crypt class {name}"),
        };
        let mut result = Self {
            kind,
            seed: 0,
            table: Vec::new(),
            min: 0x400,
            post: 0x200,
            size: 0x100,
        };
        let number = |field| -> Result<u32> {
            doc.number(doc.field(value, field)?)?
                .try_into()
                .context("invalid extra crypt parameter")
        };
        if matches!(
            kind,
            Kind::ShojoMama | Kind::YuruPlus | Kind::Testament | Kind::MakiFes | Kind::KeyAdler
        ) {
            result.seed = number("Seed")?;
            result.min = number("MinLength")? as usize;
            result.post = number("PostDataOffset")? as usize;
            if kind != Kind::KeyAdler {
                result.table = doc.bytes(doc.field(value, "DecodeTable")?)?.to_vec();
                ensure!(!result.table.is_empty(), "empty extra crypt table");
            }
            if matches!(kind, Kind::ShojoMama | Kind::YuruPlus | Kind::Testament) {
                result.size = number("EncryptedSize")? as usize;
            }
            ensure!(
                result.min <= 512 * 1024 * 1024
                    && result.post.checked_add(4).is_some_and(|n| n <= result.min),
                "invalid extra crypt post offset"
            );
            ensure!(
                result.size <= result.min && (kind != Kind::KeyAdler || result.min >= 0x208),
                "invalid extra crypt size"
            );
        } else if kind == Kind::Ushimitsu {
            result.seed = number("m_key")?;
        }
        Ok(Some(result))
    }

    pub(crate) fn decrypt(&self, data: &mut [u8], post: bool) -> Result<()> {
        use Kind::*;
        let len = data.len();
        match self.kind {
            ShojoMama | YuruPlus | Testament | MakiFes | KeyAdler => {
                if len < self.min {
                    return Ok(());
                }
                if post {
                    xor_word(data, self.post, self.seed)?;
                } else if self.kind == KeyAdler {
                    xor_word(data, 0x204, adler(&data[..0x100]))?;
                } else if self.kind == MakiFes {
                    let mut key = self.seed;
                    for byte in &mut data[..0x100] {
                        key = key.wrapping_mul(0x343fd).wrapping_add(0x269ec3);
                        *byte ^= self.table[((key >> 16) & 0x7fff) as usize % self.table.len()];
                    }
                } else {
                    let offsets = match self.kind {
                        ShojoMama => [1, 4, 2, 3],
                        YuruPlus => [4, 3, 2, 1],
                        _ => [3, 2, 1, 0],
                    };
                    let mut keys = offsets.map(|n| self.seed.wrapping_add(n));
                    for byte in &mut data[..self.size] {
                        let j = keys[3]
                            ^ (keys[3] << 11)
                            ^ keys[0]
                            ^ ((keys[3] ^ (keys[3] << 11) ^ (keys[0] >> 11)) >> 8);
                        keys = [j, keys[0], keys[1], keys[2]];
                        *byte ^= self.table[j as usize % self.table.len()];
                    }
                }
            }
            Majime | Nyaru if len >= 0x200 && post == (self.kind == Nyaru) => {
                let mut sum = 0u16;
                let mut bit = 0u8;
                for byte in &mut data[..0x100] {
                    let value = *byte;
                    sum += u16::from(value >> 1);
                    *byte = value >> 1 | bit;
                    bit = value << 7;
                }
                data[0] |= bit;
                let pos = if post { 0x100 } else { 0x104 };
                data[pos] ^= sum as u8;
                data[pos + 1] ^= (sum >> 8) as u8;
            }
            Alcot if post && len >= 0x400 => {
                let mut crc = 0xffffu16;
                for &b in &data[..(len & 0x7e | 1)] {
                    crc ^= u16::from(b);
                    for _ in 0..8 {
                        crc = (crc >> 1) ^ if crc & 1 != 0 { 0x8408 } else { 0 };
                    }
                }
                crc ^= 0xffff;
                data[0x104] ^= crc as u8;
                data[0x105] ^= (crc >> 8) as u8;
            }
            PreAdler | PostAdler if len >= 0x400 && post == (self.kind == PostAdler) => {
                xor_word(data, 0x200, adler(&data[..0xff]))?;
            }
            Count | AltCount if post && len >= if self.kind == Count { 0x200 } else { 0x400 } => {
                let mut counts = [0u8; 2];
                for &b in &data[..(len & 0x7e | 1)] {
                    if b == 0 {
                        counts[0] += 1;
                    } else if b == 255 {
                        counts[1] += 1;
                    }
                }
                if self.kind == AltCount {
                    counts.swap(0, 1);
                }
                data[0x100] ^= counts[0];
                data[0x104] ^= counts[1];
            }
            Ushimitsu if post && len >= 0x100 => {
                for pos in (0..0x100).step_by(4) {
                    xor_word(data, pos, self.seed)?;
                }
            }
            Dodakure | Jokers | Binbo
                if post && len >= if self.kind == Jokers { 0x400 } else { 0x200 } =>
            {
                if data[..4] == 0x718e958du32.to_le_bytes() {
                    let input = data[..0x200].to_vec();
                    match self.kind {
                        Dodakure => unpack_rle(&input, data)?,
                        Jokers => unpack_range(&input, data)?,
                        Binbo => unpack_lz(&input, data)?,
                        _ => unreachable!(),
                    }
                }
                for (byte, key) in data[0x200..]
                    .iter_mut()
                    .take(4)
                    .zip((len as u32).to_le_bytes())
                {
                    *byte ^= key;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn adler(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &v in data {
        a = (a + u32::from(v)) % 65521;
        b = (b + a) % 65521;
    }
    a | b << 16
}
fn xor_word(data: &mut [u8], at: usize, key: u32) -> Result<()> {
    let out = data
        .get_mut(at..at + 4)
        .context("extra crypt write exceeds buffer")?;
    for (b, k) in out.iter_mut().zip(key.to_le_bytes()) {
        *b ^= k;
    }
    Ok(())
}
fn byte(input: &[u8], pos: &mut usize) -> Result<u8> {
    let b = *input.get(*pos).context("truncated extra crypt stream")?;
    *pos += 1;
    Ok(b)
}
fn unpack_rle(input: &[u8], output: &mut [u8]) -> Result<()> {
    let length = u32::from_le_bytes(input[8..12].try_into()?) as usize;
    ensure!(
        length <= output.len(),
        "extra crypt RLE output exceeds buffer"
    );
    let (mut src, mut dst, mut repeat) = (12, 0, false);
    while dst < length {
        let count = byte(input, &mut src)? as usize;
        ensure!(count <= length - dst, "extra crypt RLE run exceeds output");
        if repeat {
            ensure!(
                dst > 0 || count == 0,
                "extra crypt RLE has no previous byte"
            );
            if count > 0 {
                let b = output[dst - 1];
                output[dst..dst + count].fill(b);
            }
        } else {
            output[dst..dst + count].copy_from_slice(
                input
                    .get(src..src + count)
                    .context("truncated extra crypt RLE literal")?,
            );
            src += count;
        }
        dst += count;
        if count < 255 {
            repeat = !repeat;
        }
    }
    Ok(())
}
fn unpack_range(input: &[u8], output: &mut [u8]) -> Result<()> {
    let length = u32::from_le_bytes(input[8..12].try_into()?) as usize;
    ensure!(
        length <= output.len(),
        "extra crypt range output exceeds buffer"
    );
    let weights = &input[12..268];
    let mut cumulative = [0u32; 257];
    for i in 0..256 {
        cumulative[i + 1] = cumulative[i] + u32::from(weights[i]);
    }
    let total = cumulative[256];
    ensure!(total > 0, "empty extra crypt range distribution");
    let mut src = 272;
    let mut current = u32::from_be_bytes(input[268..272].try_into()?);
    let (mut low, mut high) = (0u32, u32::MAX);
    for out in &mut output[..length] {
        let range = high / total;
        ensure!(range > 0, "invalid extra crypt range");
        let slot = current.wrapping_sub(low) / range;
        ensure!(
            slot < total,
            "extra crypt range symbol exceeds distribution"
        );
        let symbol = cumulative.partition_point(|&v| v <= slot) - 1;
        *out = symbol as u8;
        low = low.wrapping_add(cumulative[symbol].wrapping_mul(range));
        high = u32::from(weights[symbol]).wrapping_mul(range);
        while (low ^ high.wrapping_add(low)) & 0xff000000 == 0 {
            low <<= 8;
            high <<= 8;
            current = current << 8 | u32::from(byte(input, &mut src)?);
        }
        while high < 0x10000 {
            low <<= 8;
            high = 0x1000000 - (low & 0xffff00);
            current = current << 8 | u32::from(byte(input, &mut src)?);
        }
    }
    Ok(())
}
fn unpack_lz(input: &[u8], output: &mut [u8]) -> Result<()> {
    let mut bits = crate::compression::InterleavedBits::new(input, 8, 32, false)?;
    let mut dst = 0;
    while bits.position() < input.len() {
        if bits.bit()? != 0 {
            *output
                .get_mut(dst)
                .context("extra crypt LZ output exceeds buffer")? = bits.byte()?;
            dst += 1;
        } else {
            let (offset, count) = if bits.bit()? != 0 {
                let n = u16::from_le_bytes([bits.byte()?, bits.byte()?]);
                let count = if n & 7 != 0 {
                    (n & 7) as usize + 2
                } else {
                    let n = bits.byte()? as usize;
                    if n == 0 {
                        return Ok(());
                    }
                    n + 9
                };
                ((n >> 3) as i32 - 0x2000, count)
            } else {
                let count = (bits.bit()? * 2 + bits.bit()? + 2) as usize;
                (i32::from(bits.byte()?) - 0x100, count)
            };
            crate::compression::copy_match(output, &mut dst, offset, count)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_garbro_extra_vectors() {
        use Kind::*;
        use sha2::{Digest, Sha256};
        let kinds = [
            ShojoMama, YuruPlus, Testament, MakiFes, KeyAdler, Majime, Nyaru, Alcot, Dodakure,
            Jokers, PreAdler, PostAdler, Binbo, Count, AltCount, Ushimitsu,
        ];
        for line in include_str!("../tests/fixtures/garbro/extra.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let kind = kinds[fields[0].parse::<usize>().unwrap()];
            let length: usize = fields[1].parse().unwrap();
            let post = fields[2] == "1";
            let extra = ExtraCrypt {
                kind,
                seed: 0x12345678,
                table: (0..257).map(|i| (i * 17 + 3) as u8).collect(),
                min: 0x400,
                post: if kind == YuruPlus { 0x204 } else { 0x200 },
                size: if kind == YuruPlus { 0x100 } else { 0xff },
            };
            let mut data: Vec<_> = (0..length).map(|i| (i * 37 + 11) as u8).collect();
            data[0] = 0;
            data[2] = 255;
            extra.decrypt(&mut data, post).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&data)),
                fields[3],
                "{kind:?} length={length} post={post}"
            );
        }
    }
    #[test]
    fn compressed_prefixes_and_bounds() {
        let make = |kind| ExtraCrypt {
            kind,
            seed: 0,
            table: vec![],
            min: 0x400,
            post: 0x200,
            size: 0x100,
        };
        let mut rle = vec![0; 1024];
        rle[..4].copy_from_slice(&0x718e958du32.to_le_bytes());
        rle[8..12].copy_from_slice(&8u32.to_le_bytes());
        rle[12..18].copy_from_slice(&[2, b'A', b'B', 4, 2, b'C']);
        rle[18] = b'D';
        let mut decoded = rle.clone();
        make(Kind::Dodakure).decrypt(&mut decoded, true).unwrap();
        assert_eq!(&decoded[..8], b"ABBBBBCD");
        rle[8..12].copy_from_slice(&1025u32.to_le_bytes());
        assert!(make(Kind::Dodakure).decrypt(&mut rle, true).is_err());
        let mut range = vec![0; 1024];
        range[..4].copy_from_slice(&0x718e958du32.to_le_bytes());
        range[8..12].copy_from_slice(&100u32.to_le_bytes());
        range[12 + b'Q' as usize] = 1;
        make(Kind::Jokers).decrypt(&mut range, true).unwrap();
        assert_eq!(&range[..100], &[b'Q'; 100]);
        let mut lz = vec![0; 512];
        lz[..4].copy_from_slice(&0x718e958du32.to_le_bytes());
        // Literal A, short overlapping match of length 5, long end marker.
        lz[8..12].copy_from_slice(&(0b1001101u32 << 25).to_le_bytes());
        lz[12..17].copy_from_slice(&[b'A', 255, 0, 0, 0]);
        make(Kind::Binbo).decrypt(&mut lz, true).unwrap();
        assert_eq!(&lz[..6], b"AAAAAA");
    }
}
