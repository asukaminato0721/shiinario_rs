use crate::{
    Archive, Entry,
    compression::MAX_OUTPUT,
    profile::{Catalog, Profile},
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use shiinario_core::EngineVersion;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub source: PathBuf,
    pub version: EngineVersion,
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
        let version = EngineVersion::from_config(&version)
            .with_context(|| format!("unsupported configuration section {version:?}"))?;
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
        let root = root.as_ref();
        let profile = Catalog::builtin()?.for_directory(root)?;
        Self::open_with_profile(root, profile)
    }
    pub fn open_with_profile(root: impl AsRef<Path>, profile: Arc<Profile>) -> Result<Self> {
        let root = crate::fs::canonicalize(root.as_ref())?;
        let mut paths = crate::fs::read_dir(&root)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut configs = Vec::new();
        for p in &paths {
            if p.extension().is_some_and(|s| s.eq_ignore_ascii_case("ini")) {
                let data = crate::fs::read(p)?;
                if let Ok(c) = Config::parse(p.clone(), &data) {
                    configs.push(c);
                }
            }
        }
        let config = configs
            .first()
            .context("no supported Shiina Rio v2.36/v2.47 configuration found")?
            .clone();
        ensure!(
            configs.iter().all(|c| c.version == config.version
                && c.width == config.width
                && c.height == config.height
                && c.startup.eq_ignore_ascii_case(&config.startup)
                && c.archive.eq_ignore_ascii_case(&config.archive)),
            "ambiguous startup configurations"
        );
        let mut archives = Vec::new();
        let mut files = BTreeMap::new();
        for p in &paths {
            if p.extension().is_some_and(|s| s.eq_ignore_ascii_case("war")) {
                let a = Archive::open_with_profile(p, profile.clone())?;
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
            let mut paths = crate::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
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
        if let Some(data) = self.read_loose(name)? {
            return Ok(data);
        }
        match self.resolve(name)? {
            Source::Loose(p) => {
                ensure!(
                    crate::fs::metadata(p)?.len() <= MAX_OUTPUT as u64,
                    "asset exceeds cap"
                );
                Ok(crate::fs::read(p)?)
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
        let (archive, entry) = self.registered_entry(name, paths)?;
        archive.read(entry)
    }
    /// Original WARC index fields, in decoded-size / stored-size order.
    pub fn sizes_with_archives(&self, name: &str, paths: &[String]) -> Result<[u32; 2]> {
        if let Some(path) = self.loose_path(name, false)? {
            let size = crate::fs::metadata(path)?.len();
            ensure!(size <= MAX_OUTPUT as u64, "file exceeds cap");
            return Ok([0, size as u32]);
        }
        let (_, entry) = self.registered_entry(name, paths)?;
        Ok([entry.unpacked_size, entry.size])
    }
    fn registered_entry(&self, name: &str, paths: &[String]) -> Result<(&Archive, &Entry)> {
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
                return Ok((archive, entry));
            }
        }
        bail!("asset not found in registered archives: {name}")
    }
    /// Filesystem-only lookup used by SCN GetFileAttributes checks. Archive
    /// entries and the archive basename fallback do not participate.
    pub fn loose_path_exists(&self, name: &str) -> Result<bool> {
        Ok(self.loose_path(name, false)?.is_some())
    }
    pub fn directory_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .loose_path(name, false)?
            .is_some_and(|path| crate::fs::is_dir(&path)))
    }
    /// Create one game-relative directory with Windows-style case lookup.
    pub fn create_directory(&self, name: &str) -> Result<bool> {
        let Some(path) = self.loose_path(name, true)? else {
            return Ok(false);
        };
        match crate::fs::create_dir(path) {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists
                        | std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }
    /// Resolve on each access: scripts can create files after Project::open.
    /// Reject links and ambiguous case aliases, including at the write target.
    fn loose_path(&self, name: &str, create: bool) -> Result<Option<PathBuf>> {
        let normalized = normalize(name)?;
        let mut current = self.root.clone();
        let mut components = normalized.split('/').peekable();
        while let Some(component) = components.next() {
            if !crate::fs::is_dir(&current) {
                return Ok(None);
            }
            let mut matched = None;
            for entry in crate::fs::read_dir(&current)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().to_lowercase() == component {
                    ensure!(
                        !entry.file_type()?.is_symlink(),
                        "symbolic link in file path {name}"
                    );
                    ensure!(matched.is_none(), "ambiguous Windows filename {name}");
                    matched = Some(entry.path());
                }
            }
            match matched {
                Some(path) => current = path,
                None if create && components.peek().is_none() => current.push(component),
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }
    pub fn read_loose(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(path) = self.loose_path(name, false)? else {
            return Ok(None);
        };
        let mut file = crate::fs::File::open(path)?;
        ensure!(
            file.metadata()?.len() <= MAX_OUTPUT as u64,
            "file exceeds cap"
        );
        let mut bytes = Vec::new();
        use std::io::Read;
        (&mut file)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= MAX_OUTPUT, "file exceeds cap");
        Ok(Some(bytes))
    }
    /// Preserve the previous slot if a write fails. Persist only original bytes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn write_file(&self, name: &str, bytes: &[u8]) -> Result<()> {
        use std::io::Write;
        ensure!(bytes.len() <= 16 * 1024 * 1024, "file write exceeds 16 MiB");
        let target = self
            .loose_path(name, true)?
            .context("file parent directory does not exist")?;
        ensure!(
            !crate::fs::is_dir(&target),
            "file destination is a directory"
        );
        let parent = target.parent().context("missing file parent")?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let (temp, mut file) = loop {
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = parent.join(format!(".shiinario-{}-{id}.tmp", std::process::id()));
            match crate::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let result = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            crate::fs::rename(&temp, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = crate::fs::remove_file(temp);
        }
        result.with_context(|| format!("write file {name}"))
    }
    #[cfg(target_arch = "wasm32")]
    pub fn write_file(&self, name: &str, bytes: &[u8]) -> Result<()> {
        ensure!(bytes.len() <= 16 * 1024 * 1024, "file write exceeds 16 MiB");
        let target = self
            .loose_path(name, true)?
            .context("file parent directory does not exist")?;
        ensure!(
            !crate::fs::is_dir(&target),
            "file destination is a directory"
        );
        crate::fs::write(target, bytes).with_context(|| format!("write file {name}"))
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
    fn archive_size_queries_preserve_registered_order_and_case_folding() {
        let root = PathBuf::from("/synthetic");
        let entry = |unpacked_size, size| Entry {
            name: "A001.TXT".into(),
            offset: 4096,
            size,
            unpacked_size,
            filetime: 0,
            flags: 0,
        };
        let project = Project {
            root: root.clone(),
            config: Config {
                source: root.join("game.ini"),
                version: EngineVersion::V2_47,
                width: 800,
                height: 600,
                startup: String::new(),
                archive: String::new(),
                values: Default::default(),
            },
            archives: vec![
                Archive {
                    path: root.join("first.war"),
                    entries: vec![entry(17, 25)],
                    profile: Catalog::from_bytes(&crate::test_support::database())
                        .unwrap()
                        .profile("Ran→Sem")
                        .unwrap(),
                },
                Archive {
                    path: root.join("second.war"),
                    entries: vec![entry(100, 64)],
                    profile: Catalog::from_bytes(&crate::test_support::database())
                        .unwrap()
                        .profile("Ran→Sem")
                        .unwrap(),
                },
            ],
            files: Default::default(),
        };
        assert_eq!(
            project
                .sizes_with_archives(
                    "story\\a001.txt",
                    &[
                        "MISSING.WAR".into(),
                        "FIRST.WAR".into(),
                        "second.war".into()
                    ]
                )
                .unwrap(),
            [17, 25]
        );
        assert_eq!(
            project
                .sizes_with_archives("a001.txt", &["second.war".into(), "first.war".into()])
                .unwrap(),
            [100, 64]
        );
        assert!(project.sizes_with_archives("a001.txt", &[]).is_err());
        assert!(
            project
                .sizes_with_archives("a002.txt", &["first.war".into()])
                .is_err()
        );
        assert!(
            project
                .sizes_with_archives("../a001.txt", &["first.war".into()])
                .is_err()
        );
    }

    #[test]
    fn windows_paths() {
        assert_eq!(normalize("BG\\Bg01a.S25").unwrap(), "bg/bg01a.s25");
        for p in ["../foo", "x/../foo", "C:\\foo", "/foo", ""] {
            assert!(normalize(p).is_err());
        }
    }
}
