use crate::{Archive, compression::MAX_OUTPUT};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub source: PathBuf,
    pub version: String,
    pub width: u32,
    pub height: u32,
    pub startup: String,
    pub archive: String,
    pub values: BTreeMap<String, String>,
}
impl Config {
    pub fn parse(source: PathBuf, data: &[u8]) -> Result<Self> {
        let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(data);
        ensure!(!bad, "invalid CP932 configuration");
        let mut version = String::new();
        let mut values = BTreeMap::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with(';'))
        {
            if line.starts_with('[') && line.ends_with(']') {
                version = line[1..line.len() - 1].into();
            } else if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_lowercase(), value.trim().to_owned());
            }
        }
        ensure!(
            version == "椎名里緒 v2.47",
            "unsupported configuration section {version:?}"
        );
        let get = |key: &str| {
            values
                .get(key)
                .with_context(|| format!("missing configuration key {key}"))
        };
        let width = get("windowwidth")?.parse()?;
        let height = get("windowheight")?.parse()?;
        ensure!(
            width == 800 && height == 600,
            "unsupported viewport {width}x{height}"
        );
        let startup = get("scn")?.clone();
        let archive = get("arc")?.clone();
        Ok(Self {
            source,
            version,
            width,
            height,
            startup,
            archive,
            values,
        })
    }
}
#[derive(Debug)]
enum Source {
    Loose(PathBuf),
    Archive(usize, usize),
}
/// Case-insensitive virtual filesystem. Archive basename aliases model the engine's flat indexes.
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    pub archives: Vec<Archive>,
    files: BTreeMap<String, Source>,
}
pub fn normalize(name: &str) -> Result<String> {
    let name = name.replace('\\', "/");
    ensure!(!name.starts_with('/'), "absolute asset path is forbidden");
    let mut parts = Vec::new();
    for part in name.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        ensure!(
            part != ".." && !part.contains(':') && !part.contains('\0'),
            "invalid asset path"
        );
        parts.push(part.to_lowercase());
    }
    ensure!(!parts.is_empty(), "empty asset path");
    Ok(parts.join("/"))
}
impl Project {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let mut paths = std::fs::read_dir(&root)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut configs = Vec::new();
        for p in &paths {
            if p.extension().is_some_and(|s| s.eq_ignore_ascii_case("ini")) {
                let data = std::fs::read(p)?;
                if let Ok(c) = Config::parse(p.clone(), &data) {
                    configs.push(c);
                }
            }
        }
        let config = configs
            .first()
            .context("no Shiina Rio v2.47 configuration found")?
            .clone();
        ensure!(
            configs.iter().all(|c| c.width == config.width
                && c.height == config.height
                && c.startup.eq_ignore_ascii_case(&config.startup)
                && c.archive.eq_ignore_ascii_case(&config.archive)),
            "ambiguous startup configurations"
        );
        let mut archives = Vec::new();
        let mut files = BTreeMap::new();
        for p in &paths {
            if p.extension().is_some_and(|s| s.eq_ignore_ascii_case("war")) {
                let a = Archive::open(p)?;
                for (ei, e) in a.entries.iter().enumerate() {
                    files
                        .entry(normalize(&e.name)?)
                        .or_insert(Source::Archive(archives.len(), ei));
                }
                archives.push(a);
            }
        }
        ensure!(
            archives.iter().any(|a| a
                .path
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case(&config.archive))),
            "configured startup archive is missing"
        );
        fn loose(
            dir: &Path,
            root: &Path,
            files: &mut BTreeMap<String, Source>,
            depth: usize,
        ) -> Result<()> {
            ensure!(depth < 32, "asset directory nesting exceeds limit");
            let mut paths = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
            paths.sort_by_key(|e| e.file_name());
            for e in paths {
                let ty = e.file_type()?;
                if ty.is_symlink() {
                    continue;
                }
                let p = e.path();
                if ty.is_dir() {
                    loose(&p, root, files, depth + 1)?;
                } else if ty.is_file() {
                    let key = normalize(&p.strip_prefix(root)?.to_string_lossy())?;
                    if matches!(files.get(&key), Some(Source::Loose(_))) {
                        bail!("ambiguous Windows filename {key}");
                    }
                    files.insert(key, Source::Loose(p));
                }
            }
            Ok(())
        }
        loose(&root, &root, &mut files, 0)?;
        let project = Self {
            root,
            config,
            archives,
            files,
        };
        project.resolve(&project.config.startup)?;
        Ok(project)
    }
    fn resolve(&self, name: &str) -> Result<&Source> {
        let key = normalize(name)?;
        self.files
            .get(&key)
            .or_else(|| self.files.get(key.rsplit('/').next().unwrap()))
            .with_context(|| format!("asset not found: {name}"))
    }
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        match self.resolve(name)? {
            Source::Loose(p) => {
                ensure!(
                    std::fs::metadata(p)?.len() <= MAX_OUTPUT as u64,
                    "asset exceeds cap"
                );
                Ok(std::fs::read(p)?)
            }
            Source::Archive(a, e) => self.archives[*a].read(&self.archives[*a].entries[*e]),
        }
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
    /// Search WARC basenames in the order registered by the binary scenario.
    /// Missing archives are skipped, as registration itself does not open them.
    pub fn read_with_archives(&self, name: &str, paths: &[String]) -> Result<Vec<u8>> {
        let key = normalize(name)?;
        let basename = key.rsplit('/').next().unwrap();
        for path in paths {
            let archive_key = normalize(path)?;
            if let Some(archive) = self.archives.iter().find(|archive| {
                archive
                    .path
                    .strip_prefix(&self.root)
                    .ok()
                    .and_then(|relative| normalize(&relative.to_string_lossy()).ok())
                    .is_some_and(|key| key == archive_key)
            }) && let Some(entry) = archive.find(basename)
            {
                return archive.read(entry);
            }
        }
        bail!("asset not found in registered archives: {name}")
    }
    /// Filesystem-only lookup used by SCN GetFileAttributes checks. Archive
    /// entries and the archive basename fallback do not participate.
    pub fn loose_path_exists(&self, name: &str) -> Result<bool> {
        let normalized = normalize(name)?;
        let mut current = self.root.clone();
        for component in normalized.split('/') {
            if !current.is_dir() {
                return Ok(false);
            }
            let mut matched = None;
            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().to_lowercase() == component
                    && !entry.file_type()?.is_symlink()
                {
                    ensure!(matched.is_none(), "ambiguous Windows filename {name}");
                    matched = Some(entry.path());
                }
            }
            let Some(path) = matched else {
                return Ok(false);
            };
            current = path;
        }
        Ok(current.exists())
    }
}
/// LRU cache counts decoded bytes and never retains a single oversized asset.
pub struct Cache {
    limit: usize,
    size: usize,
    order: VecDeque<String>,
    data: HashMap<String, Arc<[u8]>>,
}
impl Cache {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            size: 0,
            order: VecDeque::new(),
            data: HashMap::new(),
        }
    }
    pub fn read(&mut self, project: &Project, name: &str) -> Result<Arc<[u8]>> {
        let key = normalize(name)?;
        if let Some(data) = self.data.get(&key) {
            self.order.retain(|k| k != &key);
            self.order.push_back(key);
            return Ok(data.clone());
        }
        let data: Arc<[u8]> = project.read(name)?.into();
        if data.len() <= self.limit {
            while self.size + data.len() > self.limit {
                if let Some(k) = self.order.pop_front() {
                    self.size -= self.data.remove(&k).unwrap().len();
                }
            }
            self.size += data.len();
            self.order.push_back(key.clone());
            self.data.insert(key, data.clone());
        }
        Ok(data)
    }
    pub fn resident_bytes(&self) -> usize {
        self.size
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_paths() {
        assert_eq!(normalize("BG\\Bg01a.S25").unwrap(), "bg/bg01a.s25");
        for p in ["../foo", "x/../foo", "C:\\foo", "/foo", ""] {
            assert!(normalize(p).is_err());
        }
    }
}
