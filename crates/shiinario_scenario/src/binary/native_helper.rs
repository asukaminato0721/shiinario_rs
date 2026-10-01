//! Bounded Rust replacements for identified native routines embedded in SCN.
//! Unknown machine code is never executed or treated as an ignorable command.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub(super) const THUMBNAIL_CODE_SIZES: [usize; 2] = [115, 117];
pub(super) const SOURCE_SIZE: usize = 800 * 600 * 3;
pub(super) const TARGET_SIZE: usize = 100 * 75 * 3;

pub(super) fn is_thumbnail(code: &[u8]) -> bool {
    matches!(
        (code.len(), format!("{:x}", Sha256::digest(code)).as_str()),
        (117, "ed8a0da1014b470c7985b88fe30ce1b45e669cb363be7180cfb0538d5a143443")
            // TANEGAME start.SCN:0xb1eeb: same 8x8 box average and local-bank
            // source/destination pointers, with a shorter native wrapper.
            | (115, "af9acc9addb1bb970d17c4daf12c127540463195d4b153d90629e6a293027a38")
    )
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
    fn thumbnail_matches_original_v248_pixels() {
        // Original start.SCN:0xb1eeb executed with Unicorn on synthetic BGR24.
        // The probe also checked unchanged source, registers and buffer guards.
        let source: Vec<_> = (0..SOURCE_SIZE)
            .map(|i| (i * 37 + (i / 97) * 13) as u8)
            .collect();
        let output = thumbnail(&source).unwrap();
        assert_eq!(output.len(), TARGET_SIZE);
        assert_eq!(
            format!("{:x}", Sha256::digest(&output)),
            "0bb94390c77516c6feac5317cf1f3eb1c1f9f0a2be1e7bf32210ce918f38ce4a"
        );
        assert!(thumbnail(&source[..SOURCE_SIZE - 1]).is_err());
    }
}
