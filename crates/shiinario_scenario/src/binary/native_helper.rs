//! Bounded Rust replacements for identified native routines embedded in SCN.
//! Unknown machine code is never executed or treated as an ignorable command.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};

pub(super) const THUMBNAIL_CODE_SIZES: [usize; 3] = [114, 115, 117];
pub(super) const SOURCE_SIZE: usize = 800 * 600 * 3;
pub(super) const TARGET_SIZE: usize = 100 * 75 * 3;
pub(super) const SAVE_XOR_CODE_SIZE: usize = 57;
pub(super) const SAVE_XOR_SIZE: usize = 0x367d10;
pub(super) const INVERT_COPY_CODE_SIZE: usize = 92;
pub(super) const IMAGE_COPY_CODE_SIZE: usize = 158;
// The helper writes 600 rows of 2400 bytes, with four untouched bytes
// between rows. It does not access the padding after the final row.
pub(super) const IMAGE_COPY_TARGET_SIZE: usize = 599 * 2404 + 2400;
pub(super) const THUMBNAIL_COPY_CODE_SIZE: usize = 75;
pub(super) const THUMBNAIL_COPY_TARGET_SIZE: usize = 74 * 2400 + 300;

pub(super) fn is_thumbnail_copy(code: &[u8]) -> bool {
    code.len() == THUMBNAIL_COPY_CODE_SIZE
        && format!("{:x}", Sha256::digest(code))
            == "da17d745174ffd1ae2662996337d38f744644015d10f9f0dc234468a69bf13b6"
}

pub(super) fn copy_thumbnail_rows(source: &[u8], target: &mut [u8]) -> Result<()> {
    ensure!(
        source.len() == TARGET_SIZE,
        "invalid native thumbnail copy source size"
    );
    ensure!(
        target.len() == THUMBNAIL_COPY_TARGET_SIZE,
        "invalid native thumbnail copy target size"
    );
    for row in 0..75 {
        target[row * 2400..row * 2400 + 300].copy_from_slice(&source[row * 300..(row + 1) * 300]);
    }
    Ok(())
}

pub(super) fn is_image_copy(code: &[u8]) -> bool {
    code.len() == IMAGE_COPY_CODE_SIZE
        && format!("{:x}", Sha256::digest(code))
            == "d10b3166f97bba5ae8db2ccd314895f581dc458c7e150fc8b1478e8365e4a98b"
}

pub(super) fn copy_image_rows(source: &[u8], target: &mut [u8]) -> Result<()> {
    ensure!(
        source.len() == SOURCE_SIZE,
        "invalid native image copy source size"
    );
    ensure!(
        target.len() == IMAGE_COPY_TARGET_SIZE,
        "invalid native image copy target size"
    );
    for row in 0..600 {
        target[row * 2404..row * 2404 + 2400]
            .copy_from_slice(&source[row * 2400..(row + 1) * 2400]);
    }
    Ok(())
}

pub(super) fn is_invert_copy(code: &[u8]) -> bool {
    code.len() == INVERT_COPY_CODE_SIZE
        && format!("{:x}", Sha256::digest(code))
            == "28e0556b4dd601958221c44661eac54acbb7897374dbabacf80cbc14315621ec"
}

pub(super) fn is_save_xor(code: &[u8]) -> bool {
    code.len() == SAVE_XOR_CODE_SIZE
        && format!("{:x}", Sha256::digest(code))
            == "e6d8dab28fab59f3b93740e16b1965efd8d70f6174c578637611361c934073e2"
}

pub(super) fn save_xor(bytes: &mut [u8], mut key: u32) -> Result<()> {
    ensure!(bytes.len() == SAVE_XOR_SIZE, "invalid native save XOR size");
    for word in bytes.as_chunks_mut::<4>().0 {
        let value = u32::from_le_bytes(*word) ^ key;
        word.copy_from_slice(&value.to_le_bytes());
        key = key.wrapping_add(1).rotate_left(3).swap_bytes();
    }
    Ok(())
}

/// Return the bank and the source/destination operand indices.
pub(super) fn thumbnail_arguments(code: &[u8]) -> Option<(u8, [u16; 2])> {
    match (code.len(), format!("{:x}", Sha256::digest(code)).as_str()) {
        (117, "ed8a0da1014b470c7985b88fe30ce1b45e669cb363be7180cfb0538d5a143443")
            // TANEGAME start.SCN:0xb1eeb: same 8x8 box average and local-bank
            // source/destination pointers, with a shorter native wrapper.
        | (115, "af9acc9addb1bb970d17c4daf12c127540463195d4b153d90629e6a293027a38") => Some((12, [0, 1])),
        // HANAOTO uses the current stack pointer from argument-block +16.
        // Destination is stack[0]; source is stack[4].
        (114, "fbd189cc011d4369a56d8fca63b6e77696b6121db35cde2fceea93555df2a8f2") => Some((8, [4, 0])),
        _ => None,
    }
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
    fn thumbnail_copy_matches_original_hanaoto_machine_code() {
        // The original i386 helper produced this digest for all four target
        // alignments. It left the source, other image columns and guards intact.
        let source: Vec<_> = (0..TARGET_SIZE)
            .map(|i| (i * 37 + (i / 97) * 13) as u8)
            .collect();
        let mut target = vec![0xa5; THUMBNAIL_COPY_TARGET_SIZE];
        copy_thumbnail_rows(&source, &mut target).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&target)),
            "f5f0c3d474566787ca1543fe168da47a655edc3f78fdceda87dc62e90fbe71bb"
        );
        let original = target.clone();
        assert!(copy_thumbnail_rows(&source[..TARGET_SIZE - 1], &mut target).is_err());
        assert!(
            copy_thumbnail_rows(&source, &mut target[..THUMBNAIL_COPY_TARGET_SIZE - 1]).is_err()
        );
        assert_eq!(target, original);
    }

    #[test]
    fn image_copy_matches_original_hanaoto_machine_code() {
        // Linux i386 ran the original 158-byte helper at each destination
        // alignment modulo four. All four runs produced this digest.
        // The probe also checked unchanged source, padding and guards.
        let source: Vec<_> = (0..SOURCE_SIZE)
            .map(|i| (i * 37 + (i / 97) * 13) as u8)
            .collect();
        let mut target = vec![0xa5; IMAGE_COPY_TARGET_SIZE + 4];
        copy_image_rows(&source, &mut target[..IMAGE_COPY_TARGET_SIZE]).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&target)),
            "d6bfb24b587525c1d890152ab8fbe5dce1e62d997b150934a2dc512a96dd86a3"
        );
        let original = target.clone();
        assert!(
            copy_image_rows(
                &source[..SOURCE_SIZE - 1],
                &mut target[..IMAGE_COPY_TARGET_SIZE]
            )
            .is_err()
        );
        assert!(copy_image_rows(&source, &mut target[..IMAGE_COPY_TARGET_SIZE - 1]).is_err());
        assert_eq!(target, original);
    }

    #[test]
    fn save_xor_matches_original_hanaoto_machine_code_and_reverses_itself() {
        // The original 57-byte helper produced this digest on Linux i386.
        let original: Vec<_> = (0..SAVE_XOR_SIZE)
            .map(|i| (i * 37 + (i / 97) * 13) as u8)
            .collect();
        let mut bytes = original.clone();
        save_xor(&mut bytes, 0x8765_4321).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "a09129ecc17781f702c24e0ec7cd78e44e1f5f242a93f12ff81c2430d4d8f416"
        );
        save_xor(&mut bytes, 0x8765_4321).unwrap();
        assert_eq!(bytes, original);
        bytes.pop();
        let unchanged = bytes.clone();
        assert!(save_xor(&mut bytes, 0).is_err());
        assert_eq!(bytes, unchanged);
    }

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
