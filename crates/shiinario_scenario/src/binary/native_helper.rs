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
