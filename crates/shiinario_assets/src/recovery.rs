//! Recover WARC data from PE contents and x86 initialization patterns.
//! No game-name, file-size or executable-hash dispatch is used.
use crate::{compression, fs, profile::Profile};
use anyhow::{Context, Result, ensure};
use object::{
    LittleEndian as LE, pe,
    read::pe::{ImageNtHeaders, ImageOptionalHeader, PeFile},
};
use shiinario_core::EngineVersion;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_EXE: usize = 128 * 1024 * 1024;
const KEY_PREFIX: &[u8] = b"Crypt Type Yoshikun - Copyright(C) 2000 Y.Yamada/STUDIO ";
const ENGINE_PREFIX: &[u8] = b"\x92\xc5\x96\xbc\x97\xa2\x8f\x8f v2.";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(crate) fn from_executable(bytes: &[u8]) -> Result<Profile> {
    ensure!(
        bytes.len() <= MAX_EXE,
        "executable exceeds recovery size limit"
    );
    let image = PeImage::parse(bytes)?;
    match recover(&image) {
        Ok(profile) => Ok(profile),
        Err(plain_error) => {
            // Try the supported byte-transform wrapper after ordinary PE recovery.
            // Its range follows the PE headers/resources, not a game's file size.
            let Some(unpacked) = image.unpack()? else {
                return Err(plain_error);
            };
            recover(&PeImage::parse(&unpacked)?)
                .context("no supported WARC initialization patterns after unpacking")
        }
    }
}

pub(crate) struct RecoveredGame {
    pub profile: Arc<Profile>,
    pub executables: Vec<PathBuf>,
}

pub(crate) fn in_directory(directory: &Path) -> Result<Option<RecoveredGame>> {
    let mut executables = Vec::new();
    let mut profile: Option<Arc<Profile>> = None;
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            || !fs::is_file(&path)
        {
            continue;
        }
        let file = fs::File::open(&path)?;
        if file.metadata()?.len() > MAX_EXE as u64 {
            continue;
        }
        let mut bytes = Vec::new();
        file.take(MAX_EXE as u64 + 1).read_to_end(&mut bytes)?;
        // Launchers, installers and unsupported engines are normal candidates.
        let Ok(candidate) = from_executable(&bytes) else {
            continue;
        };
        if let Some(previous) = &profile {
            ensure!(
                same_profile(previous, &candidate),
                "ambiguous recoverable WARC executables in {}",
                directory.display()
            );
        } else {
            profile = Some(Arc::new(candidate));
        }
        executables.push(path);
    }
    executables.sort();
    Ok(profile.map(|profile| RecoveredGame {
        profile,
        executables,
    }))
}

fn same_profile(a: &Profile, b: &Profile) -> bool {
    a.version == b.version
        && a.entry_name_size == b.entry_name_size
        && a.key == b.key
        && a.helper_key == b.helper_key
        && a.image == b.image
        && a.region == b.region
        && a.decode == b.decode
}

struct PeImage<'a> {
    bytes: &'a [u8],
    pe: PeFile<'a, pe::ImageNtHeaders32>,
    base: u32,
}

impl<'a> PeImage<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        let pe = PeFile::<pe::ImageNtHeaders32>::parse(bytes)?;
        ensure!(
            pe.nt_headers().file_header().machine.get(LE) == pe::IMAGE_FILE_MACHINE_I386,
            "WARC recovery requires an x86 PE32 image"
        );
        let base = pe.nt_headers().optional_header().image_base() as u32;
        Ok(Self { bytes, pe, base })
    }

    fn offset(&self, address: u32) -> Result<usize> {
        let rva = address
            .checked_sub(self.base)
            .context("address precedes PE image")?;
        let (offset, _) = self
            .pe
            .section_table()
            .pe_file_range_at(rva)
            .context("WARC pointer is outside PE sections")?;
        Ok(offset as usize)
    }

    fn address(&self, offset: usize) -> Result<u32> {
        for section in self.pe.section_table().iter() {
            let raw = section.pointer_to_raw_data.get(LE) as usize;
            if offset >= raw && offset - raw < section.size_of_raw_data.get(LE) as usize {
                return self
                    .base
                    .checked_add(section.virtual_address.get(LE))
                    .and_then(|a| a.checked_add((offset - raw) as u32))
                    .context("PE address overflow");
            }
        }
        anyhow::bail!("file offset is outside PE sections")
    }

    fn pointed(&self, operand: usize) -> Result<usize> {
        self.offset(word_at(self.bytes, operand)?)
    }

    fn unpack(&self) -> Result<Option<Vec<u8>>> {
        let sections = self.pe.section_table();
        if sections.len() != 1 {
            return Ok(None);
        }
        let section = sections.iter().next().unwrap();
        if section.virtual_address.get(LE) != section.pointer_to_raw_data.get(LE) {
            return Ok(None);
        }
        let Some(resource) = self.pe.data_directory(pe::IMAGE_DIRECTORY_ENTRY_RESOURCE) else {
            return Ok(None);
        };
        let end = resource.virtual_address.get(LE) as usize;
        let optional = self.pe.nt_headers().optional_header();
        if end == 0 || optional.address_of_entry_point() as usize <= end {
            return Ok(None);
        }
        let start = self.pe.dos_header().nt_headers_offset() as usize
            + 24
            + self
                .pe
                .nt_headers()
                .file_header()
                .size_of_optional_header
                .get(LE) as usize
            + sections.len() * 40;
        if start >= end || end > self.bytes.len() {
            return Ok(None);
        }
        let mut bytes = self.bytes.to_vec();
        for (i, b) in bytes[start..end].iter_mut().enumerate() {
            *b = b
                .rotate_left(6)
                .wrapping_add(0xdd)
                .rotate_left(1)
                .wrapping_add(0xf4)
                .wrapping_add((end - start - i) as u8)
                .wrapping_add(7)
                .wrapping_add(0x9f)
                ^ 0x28;
        }
        Ok(Some(bytes))
    }
}

fn matches<'a>(bytes: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(move |(i, w)| (w == needle).then_some(i))
}

fn unique<T>(mut values: impl Iterator<Item = T>, what: &str) -> Result<T> {
    let value = values.next().with_context(|| format!("missing {what}"))?;
    ensure!(values.next().is_none(), "ambiguous {what}");
    Ok(value)
}

fn recover(image: &PeImage<'_>) -> Result<Profile> {
    let bytes = image.bytes;
    let mut versions = matches(bytes, ENGINE_PREFIX).filter_map(|at| {
        match bytes.get(at + ENGINE_PREFIX.len()..at + ENGINE_PREFIX.len() + 2)? {
            b"36" => Some(EngineVersion::V2_36),
            b"47" => Some(EngineVersion::V2_47),
            _ => None,
        }
    });
    let version = versions
        .next()
        .context("missing supported ShiinaRio version")?;
    ensure!(
        versions.all(|v| v == version),
        "ambiguous ShiinaRio version"
    );
    let key_at = unique(matches(bytes, KEY_PREFIX), "crypt key template")?;
    let mut key = slice(bytes, key_at, 64)?.to_vec();
    ensure!(
        slice(bytes, key_at + 64, 1)? == [0],
        "invalid crypt key terminator"
    );
    let destination = image.address(key_at + 11)?.to_le_bytes();
    let date = unique(
        bytes.windows(12).enumerate().filter_map(|(at, code)| {
            // push date; push key + 11; call edi (lstrcpyA).
            if code[0] != 0x68
                || code[5] != 0x68
                || code[6..10] != destination
                || code[10..12] != [0xff, 0xd7]
            {
                return None;
            }
            let at = image.pointed(at + 1).ok()?;
            let date = slice(bytes, at, 9).ok()?;
            (date[8] == 0 && date[..8].iter().all(u8::is_ascii_digit)).then_some(&date[..8])
        }),
        "crypt date initialization",
    )?;
    key[11..19].copy_from_slice(date);
    key[19] = b' ';

    let (helper_key, crypt_image, region) = unique(
        matches(bytes, b"\x81\xe6\xff\xff\xff\x0f\x81\xfb\x80\0\0\0")
            .filter_map(|at| recover_helper(image, at).ok()),
        "WARC helper initialization",
    )?;
    let decode = match version {
        EngineVersion::V2_36 => Vec::new(),
        EngineVersion::V2_47 => recover_decoder(image)?,
    };
    Ok(Profile {
        version,
        entry_name_size: match version {
            EngineVersion::V2_36 => 16,
            EngineVersion::V2_47 => 32,
        },
        key,
        helper_key,
        region,
        decode,
        image: crypt_image,
    })
}

fn recover_helper(image: &PeImage<'_>, at: usize) -> Result<([u32; 5], Vec<u8>, Vec<u8>)> {
    let bytes = image.bytes;
    let call = slice(bytes, at, 31)?;
    // jbe; lea eax/ecx,[edi+4]; push helper; push eax/ecx; call helper; add esp,8.
    ensure!(
        call[12] == 0x76
            && call[14] == 0x8d
            && matches!(call[15], 0x47 | 0x4f)
            && call[16..18] == [4, 0x68]
            && call[22] == 0x50 + ((call[15] >> 3) & 7)
            && call[23] == 0xe8
            && call[28..31] == [0x83, 0xc4, 8],
        "unsupported helper call"
    );
    let helper_at = image.pointed(at + 18)?;
    let mut helper_key = [0; 5];
    for (i, word) in helper_key.iter_mut().enumerate() {
        *word = word_at(bytes, helper_at + i * 4)?;
    }
    let function = image.offset(
        image
            .address(at + 28)?
            .wrapping_add(word_at(bytes, at + 24)?),
    )?;
    let code = slice(bytes, function, 256)?;
    let png_at = unique(
        matches(code, b"\xc7\x44\x24").filter_map(|p| {
            let offset = image.pointed(function + p + 4).ok()?;
            let header = slice(bytes, offset, 29).ok()?;
            (header.starts_with(PNG)
                && header[16..24] == [0, 0, 0, 48, 0, 0, 0, 48]
                && header[24..26] == [8, 6])
            .then_some(offset)
        }),
        "helper region PNG",
    )?;
    let png = png_bytes(bytes, png_at)?;
    let mut region = image::load_from_memory_with_format(png, image::ImageFormat::Png)?
        .into_rgba8()
        .into_raw();
    ensure!(region.len() == 48 * 48 * 4, "invalid crypt region size");
    for pixel in region.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }

    // fmul [negative image length / 2^32]; call conversion; mov ecx/edx,image.
    let start = at.saturating_sub(96);
    let crypt_image = unique(
        matches(&bytes[start..at], b"\xdc\x0d").filter_map(|p| {
            let p = start + p;
            let ins = slice(bytes, p, 16).ok()?;
            if ins[6] != 0xe8 || !matches!(ins[11], 0xb9 | 0xba) {
                return None;
            }
            let factor = image.pointed(p + 2).ok()?;
            let len =
                -f64::from_le_bytes(slice(bytes, factor, 8).ok()?.try_into().ok()?) * 4294967296.0;
            if !(1.0..=1048576.0).contains(&len) || len.fract() != 0.0 {
                return None;
            }
            let data = slice(bytes, image.pointed(p + 12).ok()?, len as usize).ok()?;
            (data.starts_with(PNG) || data.starts_with(b"\xff\xd8\xff")).then_some(data.to_vec())
        }),
        "crypt image reference",
    )?;
    Ok((helper_key, crypt_image, region))
}

fn png_bytes(bytes: &[u8], start: usize) -> Result<&[u8]> {
    let mut end = start + 8;
    loop {
        let chunk = slice(bytes, end, 8)?;
        let len = u32::from_be_bytes(chunk[..4].try_into()?) as usize;
        ensure!(len <= 65536, "crypt PNG chunk exceeds size limit");
        end = end
            .checked_add(12 + len)
            .context("crypt PNG range overflow")?;
        ensure!(end - start <= 65536, "crypt PNG exceeds size limit");
        let data = slice(bytes, start, end - start)?;
        if &chunk[4..] == b"IEND" {
            return Ok(data);
        }
    }
}

fn recover_decoder(image: &PeImage<'_>) -> Result<Vec<u8>> {
    unique(matches(image.bytes, b"\x6a\0\x56\x6a\0\x68").filter_map(|at| {
        let code = slice(image.bytes, at, 16).ok()?;
        if code[10..12] != [0x50, 0xe8] { return None; }
        let data = image.pointed(at + 6).ok()?;
        let header = data.checked_sub(8)?;
        if slice(image.bytes, header, 8).ok()? != [0,32,0,0,0,32,0,0] { return None; }
        let decoded = compression::yh1(slice(image.bytes, data, 8192).ok()?, 8192).ok()?;
        // The supported runtime decoder stores its tree/state in this allocation.
        if !decoded.starts_with(b"\xe9\x7b\x10\0\0")
            || decoded.get(0x1099..0x10af)? != b"\x8d\xbd\xc0\x13\0\0\xb9\x40\x0c\0\0\x88\x07\x47\x49\x75\xfa\x8d\xb5\xf0\x12\0" {
            return None;
        }
        expand_decode(decoded).ok()
    }), "supported embedded WARC decoder")
}

fn slice(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    bytes
        .get(at..at.checked_add(len).context("executable range overflow")?)
        .context("truncated executable data")
}

fn word_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(slice(bytes, at, 4)?.try_into()?))
}

// The decoder expands another Huffman stream before applying the archive XOR.
// Its tree, remaining-bit count and expanded code are themselves part of the
// 8192-byte XOR table. A plain decompression of the inner code is insufficient.
struct RuntimeHuffman {
    code: Vec<u8>,
    pos: usize,
    word: u32,
    left: u32,
    nodes: Vec<[u32; 2]>,
}

impl RuntimeHuffman {
    fn bit(&mut self) -> Result<u32> {
        self.left -= 1;
        let bit = (self.word >> self.left) & 1;
        if self.left == 0 {
            self.word = word_at(&self.code, self.pos)?;
            self.pos += 4;
            self.left = 32;
        }
        Ok(bit)
    }

    fn node(&mut self, depth: usize) -> Result<u32> {
        ensure!(depth <= 255, "decoder Huffman tree too deep");
        if self.bit()? == 0 {
            let mut value = 0;
            for _ in 0..8 {
                value = value * 2 + self.bit()?;
            }
            return Ok(value);
        }
        ensure!(self.nodes.len() < 255, "decoder Huffman tree too large");
        let index = self.nodes.len();
        self.nodes.push([0; 2]);
        let left = self.node(depth + 1)?;
        let right = self.node(depth + 1)?;
        self.nodes[index] = [left, right];
        Ok(index as u32 + 256)
    }
}

fn expand_decode(code: Vec<u8>) -> Result<Vec<u8>> {
    ensure!(code.len() == 8192, "invalid decoder allocation size");
    let size = word_at(&code, 0x12f0)? as usize;
    ensure!(size > 0 && size <= 0xc40, "invalid inner decoder size");
    let mut h = RuntimeHuffman {
        word: word_at(&code, 0x12f4)?,
        code,
        pos: 0x12f8,
        left: 32,
        nodes: Vec::new(),
    };
    // Original code clears this range before reading the compressed stream.
    h.code[0x13c0..].fill(0);
    let root = h.node(0)?;
    let mut output = Vec::with_capacity(size);
    for _ in 0..size {
        let mut symbol = root;
        while symbol >= 256 {
            let bit = h.bit()? as usize;
            symbol = h.nodes[(symbol - 256) as usize][bit];
        }
        output.push(symbol as u8);
    }
    for (i, [left, right]) in h.nodes.iter().enumerate() {
        let at = (i + 256) * 4;
        h.code[0x20 + at..0x24 + at].copy_from_slice(&left.to_le_bytes());
        h.code[0x81c + at..0x820 + at].copy_from_slice(&right.to_le_bytes());
    }
    h.code[0x1018..0x101c].copy_from_slice(&root.to_le_bytes());
    h.code[0x101c..0x1020].copy_from_slice(&h.left.to_le_bytes());
    h.code[0x13c0..0x13c0 + size].copy_from_slice(&output);
    Ok(h.code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn put(bytes: &mut [u8], at: usize, data: &[u8]) {
        bytes[at..at + data.len()].copy_from_slice(data);
    }

    // Synthetic PE with independent raw offsets, RVA, image base and payload.
    // No original game data or catalog entry is needed to exercise discovery.
    fn fixture(raw: usize, base: u32, date: &[u8; 8]) -> Vec<u8> {
        let mut bytes = vec![0; raw + 0x2000];
        put(&mut bytes, 0, b"MZ");
        put(&mut bytes, 0x3c, &0x80u32.to_le_bytes());
        put(&mut bytes, 0x80, b"PE\0\0\x4c\x01\x01\0");
        put(&mut bytes, 0x94, &0xe0u16.to_le_bytes());
        put(&mut bytes, 0x98, &0x10bu16.to_le_bytes());
        put(&mut bytes, 0xb4, &base.to_le_bytes());
        put(&mut bytes, 0xd4, &(raw as u32).to_le_bytes());
        put(&mut bytes, 0xf4, &16u32.to_le_bytes());
        for (off, value) in [(8, 0x2000), (12, 0x5000), (16, 0x2000), (20, raw as u32)] {
            put(&mut bytes, 0x178 + off, &value.to_le_bytes());
        }
        let address = |offset: u32| (base + 0x5000 + offset).to_le_bytes();
        let data = &mut bytes[raw..];
        put(data, 0, ENGINE_PREFIX);
        put(data, ENGINE_PREFIX.len(), b"36\0");
        put(data, 0x40, KEY_PREFIX);
        put(data, 0x40 + KEY_PREFIX.len(), b"01234567");
        put(data, 0x100, date);
        put(data, 0x200, b"\x68");
        put(data, 0x201, &address(0x100));
        put(data, 0x205, b"\x68");
        put(data, 0x206, &address(0x4b));
        put(data, 0x20a, b"\xff\xd7");
        // Image-length scaling and data reference before the helper call.
        put(data, 0x2d0, b"\xdc\x0d");
        put(data, 0x2d2, &address(0x800));
        put(data, 0x2d6, b"\xe8\0\0\0\0\xba");
        put(data, 0x2dc, &address(0xa00));
        put(
            data,
            0x300,
            b"\x81\xe6\xff\xff\xff\x0f\x81\xfb\x80\0\0\0\x76\x20\x8d\x4f\x04\x68",
        );
        put(data, 0x312, &address(0x900));
        put(data, 0x316, b"\x51\xe8");
        put(data, 0x318, &(0x500u32 - 0x31c).to_le_bytes());
        put(data, 0x31c, b"\x83\xc4\x08");
        put(data, 0x500, b"\xc7\x44\x24\x68");
        put(data, 0x504, &address(0xa00));
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(48, 48, image::Rgba([12, 34, 56, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        put(
            data,
            0x800,
            &(-(png.len() as f64) / 4294967296.0).to_le_bytes(),
        );
        put(data, 0x900, &[1; 20]);
        put(data, 0xa00, &png);
        bytes
    }

    #[test]
    fn discovers_relocated_data_without_size_or_hash_dispatch() -> Result<()> {
        let expected = from_executable(&fixture(0x200, 0x400000, b"20260920"))?;
        assert_eq!(&expected.key[11..19], b"20260920");
        assert_eq!(expected.helper_key, [0x01010101; 5]);
        assert_eq!(&expected.region[..4], &[56, 34, 12, 255]);
        let mut relocated = fixture(0x700, 0x1200000, b"20260920");
        relocated.extend_from_slice(b"unrelated overlay changes file size and hash");
        assert!(same_profile(&expected, &from_executable(&relocated)?));
        // A bad operand must not be rescued with guessed offsets or a key table.
        put(&mut relocated, 0x700 + 0x201, &u32::MAX.to_le_bytes());
        assert!(from_executable(&relocated).is_err());
        Ok(())
    }

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    impl Temp {
        fn new(label: &str) -> Result<Self> {
            let directory = std::env::temp_dir()
                .join(format!("shiinario-recovery-{}-{label}", std::process::id()));
            fs::create_dir(&directory)?;
            Ok(Self(directory))
        }
    }

    #[test]
    fn default_discovery_ignores_names_and_compares_extracted_profiles() -> Result<()> {
        let temp = Temp::new("discovery")?;
        fs::write(temp.0.join("SETUP.EXE"), b"unrelated launcher")?;
        assert!(in_directory(&temp.0)?.is_none());
        let bytes = fixture(0x400, 0x800000, b"20260920");
        fs::write(temp.0.join("anything.ExE"), &bytes)?;
        let expected = from_executable(&bytes)?;
        assert!(same_profile(
            &expected,
            Profile::for_directory(&temp.0)?.as_ref()
        ));
        assert!(same_profile(
            &expected,
            Profile::for_archive(temp.0.join("anything.WAR"))?.as_ref()
        ));
        // Different EXEs with the same extracted profile are not ambiguous.
        fs::write(
            temp.0.join("copy.exe"),
            fixture(0x300, 0x900000, b"20260920"),
        )?;
        assert_eq!(in_directory(&temp.0)?.unwrap().executables.len(), 2);
        fs::write(
            temp.0.join("other.exe"),
            fixture(0x300, 0x900000, b"20260921"),
        )?;
        assert!(in_directory(&temp.0).is_err());
        Ok(())
    }

    #[test]
    fn reconstructs_runtime_tree_and_bit_counter() {
        // A branch with leaves A and B, followed by the path bits A B A.
        let bits = "1001000001001000010010";
        let word = u32::from_str_radix(bits, 2).unwrap() << (32 - bits.len());
        let mut code = vec![0; 8192];
        code[0x12f0..0x12f4].copy_from_slice(&3u32.to_le_bytes());
        code[0x12f4..0x12f8].copy_from_slice(&word.to_le_bytes());
        let table = expand_decode(code).unwrap();
        assert_eq!(&table[0x13c0..0x13c3], b"ABA");
        assert_eq!(word_at(&table, 0x420).unwrap(), u32::from(b'A'));
        assert_eq!(word_at(&table, 0xc1c).unwrap(), u32::from(b'B'));
        assert_eq!(word_at(&table, 0x1018).unwrap(), 256);
        assert_eq!(word_at(&table, 0x101c).unwrap(), 10);
    }

    #[test]
    fn rejects_unknown_executables_and_invalid_decoder_streams() {
        for size in [0, 64, 544_768, 18_333_696] {
            assert!(from_executable(&vec![0; size]).is_err());
        }
        assert!(expand_decode(vec![0; 8191]).is_err());
        assert!(expand_decode(vec![0; 8192]).is_err());
        let mut code = vec![0xff; 8192];
        code[0x12f0..0x12f4].copy_from_slice(&1u32.to_le_bytes());
        assert!(expand_decode(code).is_err());
    }

    /// Local original-game regression; no proprietary binaries are checked in.
    /// Set SHIINARIO_WANA_EXE and SHIINARIO_RAN_EXE to the two original EXEs.
    #[test]
    #[ignore = "requires original game installations"]
    fn original_games_match_catalog_for_every_entry() -> Result<()> {
        use crate::{Archive, profile::Catalog};
        for (variable, title) in [
            ("SHIINARIO_WANA_EXE", "Wana ~Hakudaku Mamire no Houkago~"),
            ("SHIINARIO_RAN_EXE", "Ran→Sem"),
        ] {
            let exe = PathBuf::from(
                std::env::var_os(variable).with_context(|| format!("set {variable}"))?,
            );
            let bytes = fs::read(&exe)?;
            // Recover before touching the catalog: this path has no catalog dependency.
            let recovered = Arc::new(Profile::from_executable(&bytes)?);
            let reference = Catalog::builtin()?.profile(title)?;
            assert_eq!(recovered.version, reference.version);
            assert_eq!(recovered.entry_name_size, reference.entry_name_size);
            assert_eq!(recovered.key, reference.key);
            assert_eq!(recovered.helper_key, reference.helper_key);
            assert_eq!(recovered.image, reference.image);
            assert_eq!(recovered.region, reference.region);
            if recovered.version == EngineVersion::V2_47 {
                assert_eq!(recovered.decode, reference.decode);
            } else {
                assert!(recovered.decode.is_empty());
            }
            let directory = exe.parent().context("EXE has no directory")?;
            let mut archives = fs::read_dir(directory)?
                .map(|e| e.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            archives.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("war")));
            archives.sort();
            ensure!(!archives.is_empty(), "no original archives found");
            for path in archives {
                let actual = Archive::open_with_profile(&path, recovered.clone())?;
                let expected = Archive::open_with_profile(&path, reference.clone())?;
                assert_eq!(
                    serde_json::to_value(&actual.entries)?,
                    serde_json::to_value(&expected.entries)?
                );
                let mut hash = Sha256::new();
                let mut total = 0usize;
                for (entry, reference_entry) in actual.entries.iter().zip(&expected.entries) {
                    let data = actual.read(entry)?;
                    assert_eq!(
                        data,
                        expected.read(reference_entry)?,
                        "{}:{}",
                        path.display(),
                        entry.name
                    );
                    total += data.len();
                    hash.update(entry.name.as_bytes());
                    hash.update(&data);
                }
                println!(
                    "{}: {} entries, {} bytes, {:x}",
                    path.file_name().unwrap().to_string_lossy(),
                    actual.entries.len(),
                    total,
                    hash.finalize()
                );
            }
            // Renaming the EXE must not change selection, including icon lookup.
            let temp = Temp::new(variable)?;
            let mut patched = bytes.clone();
            let pe = word_at(&patched, 0x3c)? as usize;
            // A timestamp patch and overlay defeat both old build guards.
            patched[pe + 8..pe + 12].fill(0x5a);
            patched.extend_from_slice(b"unrelated overlay");
            assert!(same_profile(&recovered, &from_executable(&patched)?));
            let renamed = temp.0.join("renamed.EXE");
            fs::write(&renamed, &patched)?;
            fs::write(temp.0.join("SETUP.EXE"), b"unrelated")?;
            let detected = in_directory(&temp.0)?.context("renamed EXE was not recognized")?;
            assert_eq!(detected.executables, [renamed]);
            assert_eq!(Profile::for_directory(&temp.0)?.image, recovered.image);
            assert_eq!(
                Profile::for_archive(temp.0.join("renamed.WAR"))?.image,
                recovered.image
            );
            assert_eq!(
                crate::icon::from_game(&temp.0)?,
                crate::icon::from_executable(&bytes)?
            );
        }
        Ok(())
    }
}
