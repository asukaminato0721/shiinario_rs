//! Verified non-drawing subset of the engine's inline text controls.
use anyhow::{Result, ensure};

#[derive(Debug, Clone, Copy)]
pub enum TextClock {
    Character,
    Wait,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TextStyle {
    pub color: [u8; 3],
    pub opacity: u32,
    pub effects: u32,
    pub edge_color: [u8; 3],
    pub edge_offset: [u32; 2],
    pub font_height: u32,
    pub font_weight: u32,
    pub half_advance: u32,
    pub full_advance: u32,
    pub line_advance: u32,
    pub line_limit: u32,
    pub fullwidth_ascii: bool,
    pub fullwidth_spaces: bool,
    pub character_delay: u32,
    pub character_epoch: u32,
    pub wait_epoch: u32,
    pub font_face: Vec<u8>,
    pub skip_mask: u32,
    pub background_mode: u32,
    pub outline_rasterizer: bool,
    pub punctuation: [Vec<u8>; 3],
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255; 3],
            opacity: 256,
            effects: 0,
            edge_color: [255; 3],
            edge_offset: [1; 2],
            font_height: 16,
            font_weight: 400,
            half_advance: 8,
            full_advance: 16,
            line_advance: 16,
            line_limit: 640,
            fullwidth_ascii: true,
            fullwidth_spaces: false,
            character_delay: 0,
            character_epoch: 0,
            wait_epoch: 0,
            font_face: b"\x82l\x82r \x83S\x83V\x83b\x83N".to_vec(),
            skip_mask: 0,
            background_mode: 1,
            outline_rasterizer: false,
            punctuation: Default::default(),
        }
    }
}
impl TextStyle {
    /// Validate the complete control stream before changing the retained style.
    pub fn configure(&self, bytes: &[u8]) -> Result<(Self, Option<TextClock>)> {
        let (style, clock, _) = self.parse(bytes, false)?;
        Ok((style, clock))
    }

    pub(crate) fn prefix(&self, bytes: &[u8]) -> Result<(Self, Option<TextClock>, usize)> {
        self.parse(bytes, true)
    }

    fn parse(&self, bytes: &[u8], one: bool) -> Result<(Self, Option<TextClock>, usize)> {
        ensure!(bytes.len() <= 65536, "text controls exceed 64 KiB");
        let mut result = self.clone();
        let mut at = 0;
        let mut clock_read = None;
        while at < bytes.len() {
            if one && at != 0 {
                break;
            }
            ensure!(
                bytes.get(at) == Some(&b'_') && bytes.get(at + 1).is_some(),
                "unsupported text control or glyph at byte {at}"
            );
            let command = bytes[at + 1];
            at += 2;
            match command {
                b'e' => result.effects = number(bytes, &mut at)?,
                b'w' | b'W' => {
                    ensure!(
                        clock_read.is_none(),
                        "multiple timed text controls in one stream are unresolved"
                    );
                    let delay = number(bytes, &mut at)?;
                    if command == b'w' {
                        result.character_delay = delay;
                        clock_read = Some(TextClock::Character);
                    } else {
                        ensure!(delay == 0, "nonzero inline text waits are unresolved");
                        clock_read = Some(TextClock::Wait);
                    }
                }
                b'F' => result.font_face = string_parameter(bytes, &mut at)?,
                b's' => result.skip_mask = number(bytes, &mut at)? & 255,
                b'T' => result.background_mode = 1,
                b'O' => result.background_mode = 2,
                b'q' => {
                    result.outline_rasterizer = match bytes.get(at) {
                        Some(b'+') => true,
                        Some(b'-') => false,
                        _ => anyhow::bail!("numeric font quality control is unresolved"),
                    };
                    at += 1;
                }
                b'P' => {
                    result.punctuation[0] = string_parameter(bytes, &mut at)?;
                    for index in 1..3 {
                        if bytes.get(at) != Some(&b'/') {
                            break;
                        }
                        at += 1;
                        result.punctuation[index] = string_parameter(bytes, &mut at)?;
                    }
                }
                b'E' => {
                    for component in &mut result.edge_color {
                        *component = number(bytes, &mut at)? as u8;
                    }
                    for offset in &mut result.edge_offset {
                        *offset = number(bytes, &mut at)?;
                    }
                }
                b'H' => result.font_height = number(bytes, &mut at)?,
                b'f' => result.font_weight = number(bytes, &mut at)?,
                b'Y' | b'R' => result.line_advance = number(bytes, &mut at)?,
                b'l' => result.line_limit = number(bytes, &mut at)?,
                b'A' => result.fullwidth_ascii = false,
                b'Z' | b'z' => {
                    result.fullwidth_ascii = true;
                    result.fullwidth_spaces = command == b'z';
                }
                b'X' => {
                    if bytes.get(at) == Some(&b'Z') {
                        at += 1;
                        result.full_advance = number(bytes, &mut at)?;
                    } else {
                        result.half_advance = number(bytes, &mut at)?;
                        result.full_advance = result.half_advance.wrapping_mul(2);
                        ensure!(
                            at == 0 || bytes[at - 1] != b',',
                            "optional X text parameter is unresolved"
                        );
                    }
                }
                b'c' => {}
                _ => anyhow::bail!("unsupported text control at byte {}", at - 2),
            }
            if command != b'c' {
                continue;
            }
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
        Ok((result, clock_read, at))
    }
}

fn string_parameter(bytes: &[u8], at: &mut usize) -> Result<Vec<u8>> {
    let start = *at;
    while *at < bytes.len() && bytes[*at] != b'/' {
        ensure!(*at - start < 254, "text-control string exceeds 254 bytes");
        ensure!(bytes[*at] != 0, "embedded NUL in text controls");
        *at += 1;
    }
    let result = bytes[start..*at].to_vec();
    ensure!(
        result.len() < 254 || *at == bytes.len(),
        "continuation after a 254-byte text-control string is unresolved"
    );
    if bytes.get(*at) == Some(&b'/') {
        *at += 1;
    }
    Ok(result)
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
            b"_W1",
            b"_q3",
            b"_w1_W0",
            b"_Pabc\0",
        ] {
            assert!(style.configure(bytes).is_err());
            assert_eq!(style, TextStyle::default());
        }
    }
}
