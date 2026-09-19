//! Verified non-drawing subset of the engine's inline text controls.
use anyhow::{Result, ensure};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextStyle {
    pub color: [u8; 3],
    pub opacity: u32,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255; 3],
            opacity: 256,
        }
    }
}
impl TextStyle {
    /// Validate the complete control stream before changing the retained style.
    pub fn configure(&self, bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= 65536, "text controls exceed 64 KiB");
        let mut result = self.clone();
        let mut at = 0;
        while at < bytes.len() {
            ensure!(
                bytes.get(at..at + 2) == Some(b"_c"),
                "unsupported text control or glyph at byte {at}"
            );
            at += 2;
            if bytes.get(at) == Some(&b'a') {
                at += 1;
                result.opacity = number(bytes, &mut at)?;
            } else {
                for component in &mut result.color {
                    *component = number(bytes, &mut at)? as u8;
                }
                if at > 0 && bytes[at - 1] == b',' {
                    result.opacity = number(bytes, &mut at)?;
                }
            }
        }
        Ok(result)
    }
}

fn number(bytes: &[u8], at: &mut usize) -> Result<u32> {
    while bytes.get(*at) == Some(&b' ') {
        *at += 1;
    }
    let negative = bytes.get(*at) == Some(&b'-');
    if matches!(bytes.get(*at), Some(b'+' | b'-')) {
        *at += 1;
    }
    let mut radix = 10;
    let mut value = 0u32;
    let mut digits = 0;
    while let Some(&byte) = bytes.get(*at) {
        if byte == b'x' {
            radix = 16;
            *at += 1;
            continue;
        }
        let Some(digit) = char::from(byte).to_digit(radix) else {
            break;
        };
        value = value.wrapping_mul(radix).wrapping_add(digit);
        digits += 1;
        *at += 1;
    }
    ensure!(digits != 0, "missing text-control number at byte {}", *at);
    if matches!(bytes.get(*at), Some(b',' | b'.' | b' ')) {
        *at += 1;
    }
    if let Some(byte) = bytes.get(*at) {
        ensure!(*byte != 0, "embedded NUL in text controls");
    }
    Ok(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_controls_glyphs_and_incomplete_parameters() {
        let style = TextStyle::default();
        for bytes in [
            b"_c1,2".as_slice(),
            b"_c1,2,3_text",
            b"hello",
            b"_c{v},2,3",
            b"_ca",
            b"_C1,2,3",
        ] {
            assert!(style.configure(bytes).is_err());
            assert_eq!(style, TextStyle::default());
        }
    }
}
