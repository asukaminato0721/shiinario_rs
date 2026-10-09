//! Bounded, read-only access to ShiinaRio WARC 1.0–1.7 resources.
mod compression;
mod crypt;
mod extra_crypt;
pub mod fs;
pub mod icon;
mod nrbf;
pub mod profile;
mod range;
mod recovery;
use anyhow::{Context, Result, ensure};
use fs::File;
use profile::Profile;
use serde::Serialize;
use std::sync::Arc;
use std::{
    collections::BTreeSet,
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
    pub version: u16,
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
impl Archive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let profile = if Self::probe_version(path)? <= 110 {
            Profile::unencrypted()
        } else {
            Profile::for_archive(path)?
        };
        Self::open_with_profile(path, profile)
    }
    pub(crate) fn probe_version(path: &Path) -> Result<u16> {
        let mut header = [0; 8];
        File::open(path)?.read_exact(&mut header)?;
        ensure!(
            &header[..7] == b"WARC 1." && (b'0'..=b'7').contains(&header[7]),
            "unsupported archive signature in {}",
            path.display()
        );
        Ok(100 + u16::from(header[7] - b'0') * 10)
    }
    pub fn open_with_profile(path: impl AsRef<Path>, profile: Arc<Profile>) -> Result<Self> {
        profile.parse_result(Self::parse_index(path.as_ref(), profile.clone()))
    }
    fn parse_index(path: &Path, profile: Arc<Profile>) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let len = file.metadata()?.len();
        let mut header = [0; 12];
        file.read_exact(&mut header)?;
        ensure!(
            &header[..7] == b"WARC 1." && (b'0'..=b'7').contains(&header[7]),
            "unsupported archive signature in {}",
            path.display()
        );
        let version = 100 + u16::from(header[7] - b'0') * 10;
        let profile = if version <= 110 {
            Profile::unencrypted()
        } else {
            profile
        };
        let offset = u32le(&header[8..]) ^ if version == 100 { 0 } else { 0xf182ad82 };
        ensure!(
            offset >= 12 && (offset as u64) < len,
            "invalid index offset"
        );
        let max_index = if version == 100 {
            0xc000
        } else {
            profile.max_index(version)
        };
        let n = (len - offset as u64).min(max_index as u64) as usize;
        let mut index = vec![0; max_index];
        file.seek(SeekFrom::Start(offset as u64))?;
        file.read_exact(&mut index[..n])?;
        let index = if version == 100 {
            index.truncate(n);
            for (i, b) in index.iter_mut().enumerate() {
                *b ^= [0xfe, 0xe5][i % 2];
            }
            index
        } else {
            crypt::decrypt_index(&profile, version, offset, &mut index)?;
            if version >= 170 {
                ensure!(n >= 8, "truncated WARC index header");
                compression::zlib(&index[8..n], max_index).context("decoding WARC index")?
            } else if version >= 120 {
                crate::range::decode(&index[..n], max_index).context("decoding WARC range index")?
            } else {
                index.truncate(n);
                index
            }
        };
        let name_size = profile.entry_name_size;
        let record_size = name_size + if version == 100 { 8 } else { 24 };
        ensure!(index.len() % record_size == 0, "partial WARC index record");
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for record in index.chunks_exact(record_size) {
            let end = record[..name_size]
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_size);
            if end == 0 || (version != 100 && record[0] >= 0x80) {
                continue;
            }
            let (name, _, bad) = encoding_rs::SHIFT_JIS.decode(&record[..end]);
            ensure!(!bad, "invalid CP932 entry name");
            let e = Entry {
                name: name.into_owned(),
                offset: u32le(&record[name_size..]),
                size: u32le(&record[name_size + 4..]),
                unpacked_size: if version == 100 {
                    u32le(&record[name_size + 4..])
                } else {
                    u32le(&record[name_size + 8..])
                },
                filetime: if version == 100 {
                    0
                } else {
                    u64::from_le_bytes(record[name_size + 12..name_size + 20].try_into().unwrap())
                },
                flags: if version == 100 {
                    0
                } else {
                    u32le(&record[name_size + 20..])
                },
            };
            ensure!(
                e.offset >= 12 && e.offset as u64 + e.size as u64 <= len,
                "invalid placement for {}",
                e.name
            );
            if version != 100 && !names.insert(e.name.clone()) {
                continue;
            }
            entries.push(e);
        }
        ensure!(!entries.is_empty(), "empty WARC index");
        Ok(Self {
            path: path.to_owned(),
            entries,
            profile,
            version,
        })
    }
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        self.profile.parse_result(self.read_entry(entry))
    }
    fn read_entry(&self, entry: &Entry) -> Result<Vec<u8>> {
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
        if self.version == 100 {
            return if data.starts_with(b"Ylz") {
                compression::ylz16(&data[8..], u32le(&data[4..]) as usize)
            } else {
                Ok(data)
            };
        }
        let size = u32le(&data[4..]);
        let sig = u32le(&data)
            ^ if self.version > 110 {
                (size ^ 0x82ad82) & 0xffffff
            } else {
                0
            };
        if self.version > 110 {
            if entry.flags & 0x80000000 != 0 {
                crypt::decrypt(&self.profile, self.version, &mut data[8..])?;
            }
            crypt::decrypt_extra(&self.profile, &mut data[8..]);
            if let Some(extra) = &self.profile.extra_crypt {
                extra.decrypt(&mut data[8..], false)?;
            }
            if entry.flags & 0x20000000 != 0 {
                crypt::decrypt2(&self.profile, &mut data[8..])?;
            }
        }
        let compressed = matches!(sig & 0xffffff, 0x314859 | 0x4b5059 | 0x5a4c59);
        let mut output = compression::unpack(sig, &mut data, size as usize)
            .with_context(|| format!("{}:{}", self.path.display(), entry.name))?;
        if compressed && self.version > 110 {
            if entry.flags & 0x40000000 != 0 {
                crypt::decrypt2(&self.profile, &mut output)?;
            }
            if let Some(extra) = &self.profile.extra_crypt {
                extra.decrypt(&mut output, true)?;
            }
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

#[cfg(test)]
mod archive_versions {
    use super::*;
    #[test]
    fn original_garbro_encrypted_range_indexes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/garbro");
        for version in [120, 130, 140, 150, 160] {
            let path = root.join(format!("warc{version}.war"));
            let archive =
                Archive::open_with_profile(path, Arc::new(crate::profile::oracle_profile(2360)))
                    .unwrap_or_else(|e| panic!("version {version}: {e:#}"));
            assert_eq!(archive.version, version);
            assert_eq!(archive.entries.len(), 1);
            assert_eq!(archive.entries[0].name, "fixture.txt");
            assert_eq!(
                archive.read(&archive.entries[0]).unwrap(),
                b"synthetic archive payload!!\n"
            );
        }
    }
    #[test]
    fn legacy_archives_need_no_executable_or_scheme() {
        for (version, payload) in [
            (100, b"raw legacy asset".to_vec()),
            (110, b"raw legacy asset".to_vec()),
        ] {
            let path = std::env::temp_dir().join(format!(
                "shiinario-legacy-{}-{version}.war",
                std::process::id()
            ));
            let offset = 12 + payload.len() as u32;
            let mut file = format!("WARC 1.{}", (version - 100) / 10).into_bytes();
            file.extend((offset ^ if version == 100 { 0 } else { 0xf182ad82 }).to_le_bytes());
            file.extend(&payload);
            let mut index = vec![0; if version == 100 { 24 } else { 40 }];
            index[..9].copy_from_slice(b"asset.bin");
            index[16..20].copy_from_slice(&12u32.to_le_bytes());
            index[20..24].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            if version == 110 {
                index[24..28].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            }
            for (i, byte) in index.iter_mut().enumerate() {
                *byte ^= if version == 100 {
                    [0xfe, 0xe5][i % 2]
                } else {
                    offset.to_le_bytes()[i % 4]
                };
            }
            file.extend(index);
            std::fs::write(&path, file).unwrap();
            let archive = Archive::open(&path).unwrap();
            assert_eq!(archive.read(&archive.entries[0]).unwrap(), payload);
            std::fs::remove_file(path).unwrap();
        }
    }
}
