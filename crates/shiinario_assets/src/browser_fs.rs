use std::{
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_name = shiinarioStat)]
    fn stat(path: &str) -> Result<String, JsValue>;
    #[wasm_bindgen(catch, js_name = shiinarioList)]
    fn list(path: &str) -> Result<String, JsValue>;
    #[wasm_bindgen(catch, js_name = shiinarioRead)]
    fn read_range(path: &str, offset: f64, length: u32) -> Result<js_sys::Uint8Array, JsValue>;
    #[wasm_bindgen(catch, js_name = shiinarioWrite)]
    fn write_bytes(path: &str, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(catch, js_name = shiinarioMkdir)]
    fn mkdir(path: &str) -> Result<(), JsValue>;
}
fn error(value: JsValue) -> io::Error {
    let text = value.as_string().unwrap_or_else(|| format!("{value:?}"));
    let kind = if text.contains("NotFound") {
        io::ErrorKind::NotFound
    } else if text.contains("AlreadyExists") {
        io::ErrorKind::AlreadyExists
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, text)
}
fn name(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))
}
pub struct Metadata {
    directory: bool,
    size: u64,
}
impl Metadata {
    pub fn len(&self) -> u64 {
        self.size
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
    pub fn is_dir(&self) -> bool {
        self.directory
    }
    pub fn is_file(&self) -> bool {
        !self.directory
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
}
pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let value = stat(name(path.as_ref())?).map_err(error)?;
    let (directory, size): (bool, u64) = serde_json::from_str(&value)?;
    Ok(Metadata { directory, size })
}
pub fn is_dir(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir())
}
pub fn is_file(path: &Path) -> bool {
    metadata(path).is_ok_and(|m| m.is_file())
}
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    metadata(&path)?;
    Ok(path.as_ref().to_owned())
}
pub struct DirEntry {
    path: PathBuf,
}
impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> OsString {
        self.path.file_name().unwrap().into()
    }
    pub fn file_type(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }
}
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<std::vec::IntoIter<io::Result<DirEntry>>> {
    let names: Vec<String> = serde_json::from_str(&list(name(path.as_ref())?).map_err(error)?)?;
    Ok(names
        .into_iter()
        .map(|n| {
            Ok(DirEntry {
                path: path.as_ref().join(n),
            })
        })
        .collect::<Vec<_>>()
        .into_iter())
}
pub struct File {
    path: PathBuf,
    offset: u64,
}
impl File {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        if !metadata(&path)?.is_file() {
            return Err(io::Error::other("path is a directory"));
        }
        Ok(Self {
            path: path.as_ref().to_owned(),
            offset: 0,
        })
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }
}
impl Read for File {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = buffer.len().min(4 * 1024 * 1024) as u32;
        let bytes = read_range(name(&self.path)?, self.offset as f64, count).map_err(error)?;
        let count = bytes.length() as usize;
        if count > buffer.len() {
            return Err(io::Error::other("invalid browser read length"));
        }
        bytes.copy_to(&mut buffer[..count]);
        self.offset += count as u64;
        Ok(count)
    }
}
impl Seek for File {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let offset = match position {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.offset) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.metadata()?.len()) + i128::from(n),
        };
        self.offset = u64::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?;
        Ok(self.offset)
    }
}
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        return Err(io::Error::other("file exceeds 256 MiB"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    mkdir(name(path.as_ref())?).map_err(error)
}
pub fn write(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    write_bytes(name(path.as_ref())?, bytes).map_err(error)
}
