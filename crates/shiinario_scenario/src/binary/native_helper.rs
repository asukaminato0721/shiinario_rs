//! Bounded Rust replacements for identified native routines embedded in SCN.
//! Unknown machine code is never executed or treated as an ignorable command.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub(super) const THUMBNAIL_CODE_SIZE: usize = 117;
pub(super) const SOURCE_SIZE: usize = 800 * 600 * 3;
pub(super) const TARGET_SIZE: usize = 100 * 75 * 3;

pub(super) fn is_thumbnail(code: &[u8]) -> bool {
    format!("{:x}", Sha256::digest(code))
        == "ed8a0da1014b470c7985b88fe30ce1b45e669cb363be7180cfb0538d5a143443"
}

pub(super) fn thumbnail(source: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        source.len() == SOURCE_SIZE,
        "thumbnail source must be packed 800x600 BGR24"
    );
    let mut result = vec![0; TARGET_SIZE];
    for y in 0..75 {
        for x in 0..100 {
            for channel in 0..3 {
                let mut sum = 0u32;
                for dy in 0..8 {
                    for dx in 0..8 {
                        sum += u32::from(source[((y * 8 + dy) * 800 + x * 8 + dx) * 3 + channel]);
                    }
                }
                result[(y * 100 + x) * 3 + channel] = (sum / 64) as u8;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnail_averages_blocks_without_mixing_rows_or_channels() {
        let mut source = vec![0; SOURCE_SIZE];
        for y in 0..600 {
            for x in 0..800 {
                source[(y * 800 + x) * 3..(y * 800 + x) * 3 + 3].copy_from_slice(&[
                    (x / 8) as u8,
                    (y / 8) as u8,
                    ((x % 8) + (y % 8) * 8) as u8,
                ]);
            }
        }
        let result = thumbnail(&source).unwrap();
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/validation/native-thumbnail-probe.json"
        ))
        .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&result)),
            reference["expected_sha256"].as_str().unwrap()
        );
        for y in 0..75 {
            for x in 0..100 {
                assert_eq!(
                    &result[(y * 100 + x) * 3..(y * 100 + x) * 3 + 3],
                    &[x as u8, y as u8, 31]
                );
            }
        }
        assert!(thumbnail(&source[..SOURCE_SIZE - 1]).is_err());
        assert!(!is_thumbnail(&[0; THUMBNAIL_CODE_SIZE]));
    }
}
