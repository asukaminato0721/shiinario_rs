//! Bounded, read-only access to Ran→Sem's original WARC 1.7 assets.
mod compression;
mod crypt;
mod nrbf;
pub mod profile;
use anyhow::{Context, Result, ensure};
use profile::Profile;
use serde::Serialize;
use std::sync::Arc;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub unpacked_size: u32,
    pub filetime: u64,
    pub flags: u32,
}
#[derive(Debug)]
pub struct Archive {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub(crate) profile: Arc<Profile>,
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
impl Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let profile = Profile::builtin()?;
        Self::open_with_profile(path, profile)
    }
    pub fn open_with_profile(path: impl AsRef<Path>, profile: Arc<Profile>) -> Result<Self> {
        let path = path.as_ref();
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        ensure!(
            &header[..8] == b"WARC 1.7",
            "unsupported archive signature in {}",
            path.display()
        );
        let offset = u32le(&header[8..]) ^ 0xf182ad82;
        ensure!(
            offset >= 12 && offset as u64 + 8 < len,
            "invalid index offset"
        );
        let n = (len - offset as u64).min(crypt::MAX_INDEX as u64) as usize;
        let mut index = vec![0; crypt::MAX_INDEX];
        file.seek(SeekFrom::Start(offset as u64))?;
        file.read_exact(&mut index[..n])?;
        crypt::decrypt_index(&profile, offset, &mut index);
        let index = compression::zlib(&index[8..n], crypt::MAX_INDEX)
            .context("decoding WARC index (Ran→Sem profile)")?;
        ensure!(index.len() % 56 == 0, "partial WARC index record");
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for record in index.as_chunks::<56>().0 {
            let end = record[..32].iter().position(|&b| b == 0).unwrap_or(32);
            if end == 0 || record[0] >= 0x80 {
                continue;
            }
            let (name, _, bad) = encoding_rs::SHIFT_JIS.decode(&record[..end]);
            ensure!(!bad, "invalid CP932 entry name");
            let e = Entry {
                name: name.into_owned(),
                offset: u32le(&record[32..]),
                size: u32le(&record[36..]),
                unpacked_size: u32le(&record[40..]),
                filetime: u64::from_le_bytes(record[44..52].try_into().unwrap()),
                flags: u32le(&record[52..]),
            };
            ensure!(
                e.offset >= 12 && e.offset as u64 + e.size as u64 <= offset as u64,
                "invalid placement for {}",
                e.name
            );
            if !names.insert(e.name.to_lowercase()) {
                continue;
            }
            entries.push(e);
        }
        ensure!(!entries.is_empty(), "empty WARC index");
        Ok(Self {
            path: path.to_owned(),
            entries,
            profile,
        })
    }
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        ensure!(
            entry.size as usize <= compression::MAX_OUTPUT,
            "stored entry exceeds cap"
        );
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset as u64))?;
        let mut data = vec![0; entry.size as usize];
        file.read_exact(&mut data)?;
        if data.len() <= 8 {
            return Ok(data);
        }
        let size = u32le(&data[4..]);
        let sig = u32le(&data) ^ ((size ^ 0x82ad82) & 0xffffff);
        if entry.flags & 0x80000000 != 0 {
            crypt::decrypt(&self.profile, &mut data[8..]);
        }
        if entry.flags & 0x20000000 != 0 {
            crypt::decrypt2(&self.profile, &mut data[8..]);
        }
        let compressed = matches!(sig & 0xffffff, 0x314859 | 0x4b5059 | 0x5a4c59);
        let mut output = compression::unpack(sig, &mut data, size as usize)
            .with_context(|| format!("{}:{}", self.path.display(), entry.name))?;
        if compressed && entry.flags & 0x40000000 != 0 {
            crypt::decrypt2(&self.profile, &mut output);
        }
        Ok(output)
    }
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
    }
}
pub mod audio;
pub mod image;
pub mod project;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
