//! GARbro catalog loading, following FormatCatalog.DeserializeScheme and ImageArray.
//! GARbro Copyright (C) 2015-2017 morkt (MIT); see THIRD_PARTY_NOTICES.md.
use crate::{
    compression,
    nrbf::{self, Value},
};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, OnceLock},
};

const MAX_DATABASE: usize = 16 * 1024 * 1024;
const BUNDLED_DATABASE: &[u8] = include_bytes!("../data/garbro/Formats.dat");

/// Validated  v2.47 decryption data read from a GARbro catalog.
/// Loading does not execute .NET code. Other games/scheme versions are unsupported.
#[derive(Debug)]
pub struct Profile {
    pub(crate) key: Vec<u8>,
    pub(crate) image: Vec<u8>,
    pub(crate) region: Vec<u8>,
    pub(crate) decode: Vec<u8>,
    pub(crate) helper_key: [u32; 5],
}

impl Profile {
    /// Loads the pinned, embedded GARbro catalog once and shares its selected profile.
    pub fn builtin() -> Result<Arc<Self>> {
        static PROFILE: OnceLock<Result<Arc<Profile>, String>> = OnceLock::new();
        PROFILE
            .get_or_init(|| {
                Self::from_bytes(BUNDLED_DATABASE)
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map(Arc::clone)
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
        let mut scheme = None;
        for candidate in doc.array(doc.field(war, "KnownSchemes")?)? {
            if doc.string(doc.field(candidate, "<Name>k__BackingField")?)? == "" {
                ensure!(scheme.is_none(), "duplicate  scheme");
                scheme = Some(candidate);
            }
        }
        let scheme = scheme.context("GARbro catalog has no  scheme")?;
        doc.class(scheme, "GameRes.Formats.ShiinaRio.EncryptionScheme")?;
        ensure!(
            doc.number(doc.field(scheme, "<Version>k__BackingField")?)? == 2470,
            "unsupported  scheme version"
        );
        ensure!(
            doc.number(doc.field(scheme, "EntryNameSize")?)? == 32,
            "unsupported  entry name size"
        );
        ensure!(
            matches!(doc.field(scheme, "ExtraCrypt")?, Value::Null),
            "unsupported  extra crypt stage"
        );
        let key = doc.bytes(doc.field(scheme, "CryptKey")?)?;
        let region = doc.bytes(doc.field(scheme, "Region")?)?;
        let decode = doc.bytes(doc.field(scheme, "DecodeBin")?)?;
        ensure!(key.len() == 64, "invalid  CryptKey length");
        ensure!(region.len() == 48 * 48 * 4, "invalid  Region length");
        ensure!(decode.len() == 8192, "invalid  DecodeBin length");
        let helpers = doc.array(doc.field(scheme, "HelperKey")?)?;
        ensure!(helpers.len() == 5, "invalid  HelperKey length");
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
            key: key.to_vec(),
            image,
            region: region.to_vec(),
            decode: decode.to_vec(),
            helper_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn catalog_references_metadata_reuse_and_image_slicing() {
        let profile = Profile::from_bytes(&test_support::database()).unwrap();
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
                Profile::from_bytes(&database[..len]).is_err(),
                "accepted length {len}"
            );
        }
        let mut bad = database.clone();
        bad[0] ^= 1;
        assert!(Profile::from_bytes(&bad).is_err());
        let mut bad = database.clone();
        bad[8] ^= 1;
        assert!(Profile::from_bytes(&bad).is_err());
        let mut bad = database.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(Profile::from_bytes(&bad).is_err());

        let raw = test_support::stream();
        let mut bad = raw.clone();
        let title = "".as_bytes();
        let at = bad.windows(title.len()).position(|w| w == title).unwrap();
        bad[at] = b'X';
        assert!(Profile::from_bytes(&test_support::wrap(&bad)).is_err());

        let mut bad = raw.clone();
        let image_fields = [9, 17, 0, 0, 0, 1, 0, 0, 0, 9, 18, 0, 0, 0];
        let at = bad
            .windows(image_fields.len())
            .position(|w| w == image_fields)
            .unwrap();
        for len in [-1i32, 3] {
            bad[at + 5..at + 9].copy_from_slice(&len.to_le_bytes());
            assert!(Profile::from_bytes(&test_support::wrap(&bad)).is_err());
        }
        let mut bad = raw.clone();
        let version = 2470i32.to_le_bytes();
        // Both schemes must be changed: the second one is the selected profile.
        for at in 0..bad.len() - 4 {
            if bad[at..at + 4] == version {
                bad[at..at + 4].copy_from_slice(&2500i32.to_le_bytes());
            }
        }
        assert!(Profile::from_bytes(&test_support::wrap(&bad)).is_err());
    }

    #[test]
    fn reference_catalog_matches_recorded_profile() {
        use sha2::{Digest, Sha256};
        let profile = Profile::builtin().unwrap();
        assert!(Arc::ptr_eq(&profile, &Profile::builtin().unwrap()));
        let meta: serde_json::Value =
            serde_json::from_str(include_str!("../profiles/ransem-v1/profile.json")).unwrap();
        assert_eq!(
            meta["source_database_sha256"],
            format!("{:x}", Sha256::digest(BUNDLED_DATABASE))
        );
        assert_eq!(
            meta["database_version"],
            i32::from_le_bytes(BUNDLED_DATABASE[8..12].try_into().unwrap())
        );
        assert_eq!(serde_json::json!(profile.helper_key), meta["helper_key"]);
        for (name, bytes) in [
            ("CryptKey", &profile.key),
            ("Region", &profile.region),
            ("DecodeBin", &profile.decode),
            ("ShiinaImage", &profile.image),
        ] {
            assert_eq!(meta["tables"][name]["size"], bytes.len(), "{name}");
            assert_eq!(
                meta["tables"][name]["sha256"],
                format!("{:x}", Sha256::digest(bytes)),
                "{name}"
            );
        }
    }
}
