//! GARbro catalog loading, following FormatCatalog.DeserializeScheme and ImageArray.
//! GARbro Copyright (C) 2015-2017 morkt (MIT); see THIRD_PARTY_NOTICES.md.
use crate::fs::{self, File};
use crate::{
    compression,
    nrbf::{self, Document, Value},
};
use anyhow::{Context, Result, ensure};
use shiinario_core::EngineVersion;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
    sync::{Arc, OnceLock},
};

const MAX_DATABASE: usize = 16 * 1024 * 1024;
const BUNDLED_DATABASE: &[u8] = include_bytes!("../data/garbro/Formats.dat");

/// Validated WARC decryption data recovered from an executable or catalog.
/// Loading does not execute .NET code. Unverified versions can try the existing decoder.
#[derive(Debug)]
pub struct Profile {
    pub(crate) version: EngineVersion,
    pub(crate) entry_name_size: usize,
    pub(crate) key: Vec<u8>,
    pub(crate) image: Vec<u8>,
    pub(crate) region: Vec<u8>,
    pub(crate) decode: Vec<u8>,
    pub(crate) helper_key: [u32; 5],
    pub(crate) prefix_xor_key: Option<[u8; 64]>,
}

/// Embedded GARbro schemes and filename-to-game mappings.
/// Unsupported schemes retain their validation error and are never used to decrypt.
#[derive(Debug)]
pub struct Catalog {
    profiles: BTreeMap<String, Result<Arc<Profile>, String>>,
    games: BTreeMap<String, String>,
}

impl Catalog {
    pub fn builtin() -> Result<&'static Self> {
        static CATALOG: OnceLock<Result<Catalog, String>> = OnceLock::new();
        CATALOG
            .get_or_init(|| Self::from_bytes(BUNDLED_DATABASE).map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("loading embedded GARbro catalog: {e}"))
    }

    pub fn from_formats_dat(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let load = || -> Result<Self> {
            let mut data = Vec::new();
            File::open(path)?
                .take(MAX_DATABASE as u64 + 1)
                .read_to_end(&mut data)?;
            Self::from_bytes(&data)
        };
        load().with_context(|| format!("loading GARbro catalog {}", path.display()))
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        ensure!(
            data.len() <= MAX_DATABASE,
            "GARbro catalog exceeds size limit"
        );
        ensure!(
            data.len() >= 12 && &data[..8] == b"GARbroDB",
            "invalid GARbro catalog header"
        );
        let version = i32::from_le_bytes(data[8..12].try_into().unwrap());
        ensure!(version > 0, "invalid GARbro catalog version");
        let raw = compression::zlib(&data[12..], 32 * 1024 * 1024)
            .context("decompressing GARbro catalog")?;
        let doc = nrbf::parse(&raw).context("reading GARbro NRBF records")?;
        let root = doc.root()?;
        doc.class(root, "GameRes.SchemeDataBase")?;
        ensure!(
            doc.number(doc.field(root, "Version")?)? == version as i128,
            "GARbro catalog version mismatch"
        );
        let map = doc.field(root, "SchemeMap")?;
        let pairs = doc.array(doc.field(map, "KeyValuePairs")?)?;
        let mut war = None;
        for pair in pairs {
            if doc.string(doc.field(pair, "key")?)? == "WAR" {
                ensure!(war.is_none(), "duplicate WAR catalog entry");
                war = Some(doc.field(pair, "value")?);
            }
        }
        let war = war.context("GARbro catalog has no WAR schemes")?;
        doc.class(war, "GameRes.Formats.ShiinaRio.WarcScheme")?;
        let mut profiles = BTreeMap::new();
        for candidate in doc.array(doc.field(war, "KnownSchemes")?)? {
            let name = doc.string(doc.field(candidate, "<Name>k__BackingField")?)?;
            ensure!(!name.is_empty(), "empty GARbro scheme name");
            let profile = Profile::from_record(&doc, candidate)
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            ensure!(
                profiles.insert(name.to_owned(), profile).is_none(),
                "duplicate GARbro scheme {name:?}"
            );
        }
        let mut games = BTreeMap::new();
        let map = doc.field(root, "GameMap")?;
        if !matches!(map, Value::Null) {
            for pair in doc.array(doc.field(map, "KeyValuePairs")?)? {
                let filename = doc.string(doc.field(pair, "key")?)?.to_lowercase();
                let title = doc.string(doc.field(pair, "value")?)?;
                if let Some(previous) = games.insert(filename.clone(), title.to_owned()) {
                    ensure!(
                        previous == title,
                        "conflicting GARbro filename mapping {filename:?}"
                    );
                }
            }
        }
        // The pinned catalog includes Wana's scheme but omits its executable
        // from GameMap. Keep this correction separate from the upstream data.
        let wana = "Wana ~Hakudaku Mamire no Houkago~";
        if profiles.contains_key(wana) {
            games
                .entry("wana.exe".into())
                .or_insert_with(|| wana.into());
        }
        Ok(Self { profiles, games })
    }

    pub fn profile(&self, name: &str) -> Result<Arc<Profile>> {
        self.profiles
            .get(name)
            .with_context(|| format!("GARbro catalog has no scheme {name:?}"))?
            .as_ref()
            .map(Arc::clone)
            .map_err(|e| anyhow::anyhow!("unsupported GARbro scheme {name:?}: {e}"))
    }

    fn mapped_name(&self, filename: &str) -> Option<&str> {
        let title = self.games.get(&filename.to_lowercase())?;
        self.profiles.contains_key(title).then_some(title.as_str())
    }

    /// Uses a mapped archive filename first, then EXEs in the directory tree.
    pub fn for_archive(&self, path: impl AsRef<Path>) -> Result<Arc<Profile>> {
        let path = path.as_ref();
        if let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| self.mapped_name(n))
        {
            return self.profile(name);
        }
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        self.detect(directory, false)
    }

    /// Matches root archive names and recursive EXE names without executing EXEs.
    pub fn for_directory(&self, directory: impl AsRef<Path>) -> Result<Arc<Profile>> {
        self.detect(directory.as_ref(), true)
    }

    /// Original game executables matching the selected catalog scheme. This
    /// excludes setup/uninstall programs when looking up the game's icon.
    pub fn game_executables(&self, directory: &Path) -> Result<Vec<std::path::PathBuf>> {
        let selected = self.for_directory(directory)?;
        let mut paths = Vec::new();
        for path in fs::executable_paths(directory)? {
            if let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| self.mapped_name(n))
                && Arc::ptr_eq(&selected, &self.profile(name)?)
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn detect(&self, directory: &Path, archives: bool) -> Result<Arc<Profile>> {
        let mut matches = BTreeSet::new();
        let mut paths = fs::executable_paths(directory)?;
        if archives {
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if fs::is_file(&path)
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("war"))
                {
                    paths.push(path);
                }
            }
        }
        for path in paths {
            if let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| self.mapped_name(n))
            {
                matches.insert(name);
            }
        }
        ensure!(
            !matches.is_empty(),
            "cannot identify WARC decryption scheme in {} or its subdirectories. Keep the original game EXE with its original filename in this directory tree",
            directory.display()
        );
        ensure!(
            matches.len() == 1,
            "ambiguous WARC decryption schemes in {}: {}",
            directory.display(),
            matches.iter().copied().collect::<Vec<_>>().join(", ")
        );
        self.profile(matches.first().unwrap())
    }
}

impl Profile {
    /// Recover decryption data from supported PE contents and initialization patterns.
    /// This does not load Formats.dat or execute any Windows code.
    pub fn from_executable(bytes: &[u8]) -> Result<Self> {
        crate::recovery::from_executable(bytes)
    }

    /// Prefer static executable recovery; use the catalog for other builds.
    pub fn for_directory(directory: impl AsRef<Path>) -> Result<Arc<Self>> {
        let directory = directory.as_ref();
        if let Some(game) = crate::recovery::in_directory(directory)? {
            return Ok(game.profile);
        }
        Catalog::builtin()?.for_directory(directory)
    }

    /// Prefer EXE contents over filename mappings for direct archive access too.
    pub fn for_archive(path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let path = path.as_ref();
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if let Some(game) = crate::recovery::in_directory(directory)? {
            return Ok(game.profile);
        }
        Catalog::builtin()?.for_archive(path)
    }

    fn from_record(doc: &Document<'_>, scheme: &Value<'_>) -> Result<Self> {
        doc.class(scheme, "GameRes.Formats.ShiinaRio.EncryptionScheme")?;
        let version = doc.number(doc.field(scheme, "<Version>k__BackingField")?)?;
        let version = EngineVersion::from_scheme(version)
            .with_context(|| format!("invalid ShiinaRio scheme version {version}"))?;
        Self::parse_record(doc, scheme, version).map_err(|error| match version.failure_context() {
            Some(context) => error.context(context),
            None => error,
        })
    }

    fn parse_record(
        doc: &Document<'_>,
        scheme: &Value<'_>,
        version: EngineVersion,
    ) -> Result<Self> {
        let entry_name_size = doc.number(doc.field(scheme, "EntryNameSize")?)?;
        ensure!(
            matches!(entry_name_size, 16 | 32),
            "unsupported selected scheme entry name size"
        );
        ensure!(
            matches!(doc.field(scheme, "ExtraCrypt")?, Value::Null),
            "unsupported selected scheme extra crypt stage"
        );
        let key = doc.bytes(doc.field(scheme, "CryptKey")?)?;
        let region = doc.bytes(doc.field(scheme, "Region")?)?;
        let decode = doc.bytes(doc.field(scheme, "DecodeBin")?)?;
        ensure!(key.len() == 64, "invalid selected scheme CryptKey length");
        ensure!(
            region.len() == 48 * 48 * 4,
            "invalid selected scheme Region length"
        );
        ensure!(
            decode.len() == 8192,
            "invalid selected scheme DecodeBin length"
        );
        let helpers = doc.array(doc.field(scheme, "HelperKey")?)?;
        ensure!(
            helpers.len() == 5,
            "invalid selected scheme HelperKey length"
        );
        let mut helper_key = [0u32; 5];
        for (out, value) in helper_key.iter_mut().zip(helpers) {
            *out = doc
                .number(value)?
                .try_into()
                .context("invalid HelperKey value")?;
        }
        let image = doc.field(scheme, "ShiinaImage")?;
        doc.class(image, "GameRes.Formats.ShiinaRio.ImageArray")?;
        let common = doc.bytes(doc.field(image, "m_common")?)?;
        let length: usize = doc
            .number(doc.field(image, "m_common_length")?)?
            .try_into()
            .context("invalid ShiinaImage common length")?;
        ensure!(
            length <= common.len(),
            "ShiinaImage common length exceeds array"
        );
        let extra = doc.bytes(doc.field(image, "m_extra")?)?;
        ensure!(length + extra.len() > 0, "empty ShiinaImage");
        let mut image = common[..length].to_vec();
        image.extend_from_slice(extra);
        Ok(Self {
            version,
            entry_name_size: entry_name_size as usize,
            key: key.to_vec(),
            image,
            region: region.to_vec(),
            decode: decode.to_vec(),
            helper_key,
            prefix_xor_key: None,
        })
    }

    pub(crate) fn max_index(&self) -> usize {
        (self.entry_name_size + 24) * 16384
    }

    pub(crate) fn parse_result<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|error| match self.version.failure_context() {
            Some(context) => error.context(context),
            None => error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn detects_filenames_and_rejects_missing_or_ambiguous_games() {
        let temp = Temp(
            std::env::temp_dir().join(format!("shiinario-catalog-test-{}", std::process::id())),
        );
        fs::create_dir(&temp.0).unwrap();
        let catalog = Catalog::from_bytes(&test_support::database()).unwrap();
        assert!(catalog.for_directory(&temp.0).is_err());
        fs::create_dir(temp.0.join("GAME.EXE")).unwrap();
        fs::write(temp.0.join("NONWARC.EXE"), []).unwrap();
        assert!(catalog.for_directory(&temp.0).is_err()); // Neither is a WARC game executable.
        fs::write(temp.0.join("gAmE.eXe"), []).unwrap();
        let expected = catalog.profile("Ran→Sem").unwrap();
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_directory(&temp.0).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_archive(temp.0.join("unknown.war")).unwrap()
        ));
        fs::write(temp.0.join("OTHER.EXE"), []).unwrap();
        assert!(
            catalog
                .for_directory(&temp.0)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert!(catalog.for_archive(temp.0.join("unknown.war")).is_err());
        // A mapped archive name takes precedence over unrelated EXEs for direct archive access.
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_archive(temp.0.join("MaPpEd.WaR")).unwrap()
        ));
        fs::remove_file(temp.0.join("gAmE.eXe")).unwrap();
        fs::remove_file(temp.0.join("OTHER.EXE")).unwrap();
        fs::write(temp.0.join("MaPpEd.WaR"), []).unwrap();
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_directory(&temp.0).unwrap()
        ));
        fs::remove_file(temp.0.join("MaPpEd.WaR")).unwrap();
        let nested = temp.0.join("解析素材&起動用ファイル").join("originals");
        fs::create_dir_all(&nested).unwrap();
        // Nested WAR names do not select a scheme for the root archives.
        fs::write(nested.join("MaPpEd.WaR"), []).unwrap();
        assert!(catalog.for_directory(&temp.0).is_err());
        let nested_exe = nested.join("gAmE.eXe");
        fs::write(&nested_exe, []).unwrap();
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_directory(&temp.0).unwrap()
        ));
        assert!(Arc::ptr_eq(
            &expected,
            &catalog.for_archive(temp.0.join("unknown.war")).unwrap()
        ));
        assert_eq!(catalog.game_executables(&temp.0).unwrap(), vec![nested_exe]);
        fs::write(temp.0.join("OTHER.EXE"), []).unwrap();
        assert!(
            catalog
                .for_directory(&temp.0)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        let mut unsupported = test_support::stream();
        let at = unsupported
            .windows(4)
            .position(|v| v == 2470i32.to_le_bytes())
            .unwrap();
        unsupported[at..at + 4].copy_from_slice(&(-1i32).to_le_bytes());
        let catalog = Catalog::from_bytes(&test_support::wrap(&unsupported)).unwrap();
        assert!(catalog.profile("Ran→Sem").is_ok());
        let error = catalog
            .for_archive(temp.0.join("OTHER.EXE"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported") && error.contains("Unrelated"));
    }

    #[test]
    fn unverified_versions_parse_archives_before_reporting_compatibility() -> Result<()> {
        let temp = Temp(
            std::env::temp_dir().join(format!("shiinario-unverified-test-{}", std::process::id())),
        );
        fs::create_dir(&temp.0)?;
        let path = temp.0.join("fixture.war");
        fs::write(&path, include_bytes!("../tests/fixtures/minimal.war"))?;
        for number in [2460i32, 2500] {
            let mut raw = test_support::stream();
            for at in 0..raw.len() - 3 {
                if raw[at..at + 4] == 2470i32.to_le_bytes() {
                    raw[at..at + 4].copy_from_slice(&number.to_le_bytes());
                }
            }
            let catalog = Catalog::from_bytes(&test_support::wrap(&raw))?;
            let profile = catalog.profile("Ran→Sem")?;
            assert_eq!(profile.version.number(), (number / 10) as u16);
            let archive = crate::Archive::open_with_profile(&path, profile.clone())?;
            assert_eq!(
                archive.read(&archive.entries[0])?,
                b"synthetic asset fixture\n"
            );
            let mut bad_entry = archive.entries[0].clone();
            bad_entry.offset = u32::MAX;
            let error = archive.read(&bad_entry).unwrap_err();
            assert!(format!("{error:#}").contains("unverified ShiinaRio"));
            assert!(error.chain().count() > 1);
            let broken = temp.0.join("broken.war");
            fs::write(&broken, b"invalid archive")?;
            let error = crate::Archive::open_with_profile(&broken, profile).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("unverified ShiinaRio"), "{message}");
            assert!(
                message.contains("unsupported archive signature"),
                "{message}"
            );

            // Invalid tables still fail, and the structural error remains visible.
            for at in 0..raw.len() - 7 {
                if raw[at..at + 4] == number.to_le_bytes()
                    && raw[at + 4..at + 8] == 32i32.to_le_bytes()
                {
                    raw[at + 4..at + 8].copy_from_slice(&7i32.to_le_bytes());
                }
            }
            let error = Catalog::from_bytes(&test_support::wrap(&raw))?
                .profile("Ran→Sem")
                .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("entry name size"), "{message}");
            assert!(message.contains("unverified ShiinaRio"), "{message}");
        }
        assert_eq!(
            Catalog::builtin()?
                .profile("Chikan Circle")?
                .version
                .number(),
            246
        );
        Ok(())
    }

    #[test]
    fn catalog_references_metadata_reuse_and_image_slicing() {
        let profile = Catalog::from_bytes(&test_support::database())
            .unwrap()
            .profile("Ran→Sem")
            .unwrap();
        assert_eq!(profile.key.len(), 64);
        assert_eq!(profile.region, vec![0; 9216]);
        assert_eq!(profile.decode, vec![0; 8192]);
        assert_eq!(
            profile.helper_key,
            [1667458680, 1719289936, 1349942115, 20061, 0]
        );
        assert_eq!(profile.image, [0x11, 0x22, 0x33]);
    }

    #[test]
    fn rejects_invalid_catalogs() {
        let database = test_support::database();
        for len in [0, 1, 8, 11, 12, database.len() / 2, database.len() - 1] {
            assert!(
                Catalog::from_bytes(&database[..len]).is_err(),
                "accepted length {len}"
            );
        }
        let mut bad = database.clone();
        bad[0] ^= 1;
        assert!(Catalog::from_bytes(&bad).is_err());
        let mut bad = database.clone();
        bad[8] ^= 1;
        assert!(Catalog::from_bytes(&bad).is_err());
        let mut bad = database.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(Catalog::from_bytes(&bad).is_err());

        let raw = test_support::stream();
        let mut bad = raw.clone();
        let title = "Ran→Sem".as_bytes();
        let at = bad.windows(title.len()).position(|w| w == title).unwrap();
        bad[at] = b'X';
        assert!(
            Catalog::from_bytes(&test_support::wrap(&bad))
                .and_then(|catalog| catalog.profile("Ran→Sem"))
                .is_err()
        );

        let mut bad = raw.clone();
        let image_fields = [9, 17, 0, 0, 0, 1, 0, 0, 0, 9, 18, 0, 0, 0];
        let at = bad
            .windows(image_fields.len())
            .position(|w| w == image_fields)
            .unwrap();
        for len in [-1i32, 3] {
            bad[at + 5..at + 9].copy_from_slice(&len.to_le_bytes());
            assert!(
                Catalog::from_bytes(&test_support::wrap(&bad))
                    .and_then(|catalog| catalog.profile("Ran→Sem"))
                    .is_err()
            );
        }
        let mut bad = raw.clone();
        let version = 2470i32.to_le_bytes();
        // Both schemes must be changed: the second one is the selected profile.
        for at in 0..bad.len() - 4 {
            if bad[at..at + 4] == version {
                bad[at..at + 4].copy_from_slice(&(-1i32).to_le_bytes());
            }
        }
        assert!(
            Catalog::from_bytes(&test_support::wrap(&bad))
                .and_then(|catalog| catalog.profile("Ran→Sem"))
                .is_err()
        );
    }

    #[test]
    fn reference_catalog_matches_recorded_profile() {
        use sha2::{Digest, Sha256};
        let catalog = Catalog::builtin().unwrap();
        assert_eq!(catalog.mapped_name("rAnDl_.eXe"), Some("Ran→Sem"));
        assert_eq!(catalog.mapped_name("RANDL.exe"), Some("Ran→Sem"));
        let profile = catalog.profile("Ran→Sem").unwrap();
        assert!(Arc::ptr_eq(&profile, &catalog.profile("Ran→Sem").unwrap()));
        // Independent GARbro reference values for the pinned catalog. Keep these
        // expectations separate from the reader and synthetic catalog builder.
        assert_eq!(
            format!("{:x}", Sha256::digest(BUNDLED_DATABASE)),
            "54039fde222592c911536b0bd3f4d931bd580c66afd96eaefece34ea872f2982"
        );
        assert_eq!(
            i32::from_le_bytes(BUNDLED_DATABASE[8..12].try_into().unwrap()),
            148
        );
        assert_eq!(
            profile.helper_key,
            [1667458680, 1719289936, 1349942115, 20061, 0]
        );
        for (bytes, size, hash) in [
            (
                &profile.key,
                64,
                "588aedd4f3bc200ac0545909fa51e16489ab803c043fafefd57c5761648fbffc",
            ),
            (
                &profile.region,
                9216,
                "701d8a205b1a17e23d7eadeb679b71e04c9321d35c483d8d598c5734e4eb4b7e",
            ),
            (
                &profile.decode,
                8192,
                "79ac02929638cc345967a9b688c2aef3e96c6123fbb96c8771f0917a997f95c2",
            ),
            (
                &profile.image,
                33438,
                "57a763e048e8e62d4eeb3b858b2e7355d68bf6e35545f703788fbec91a1bb324",
            ),
        ] {
            assert_eq!(bytes.len(), size);
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), hash);
        }
    }
}
