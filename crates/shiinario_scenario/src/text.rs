//! Verified non-drawing subset of the engine's inline text controls.
use anyhow::{Result, ensure};
use shiinario_core::EngineVersion;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextEncoding {
    #[default]
    Cp932,
    Big5,
}
impl TextEncoding {
    /// Precondition: none. The source can contain arbitrary bytes.
    /// Postcondition: return Some only when exactly one supported codec accepts the source.
    pub fn detect_unambiguous(bytes: &[u8]) -> Option<Self> {
        let accepts = |encoding: Self| {
            encoding
                .codec()
                .decode_without_bom_handling_and_without_replacement(bytes)
                .is_some()
        };
        match (accepts(Self::Cp932), accepts(Self::Big5)) {
            (true, false) => Some(Self::Cp932),
            (false, true) => Some(Self::Big5),
            _ => None,
        }
    }
    /// Prefer CP932 when there is no established source encoding.
    pub fn detect(bytes: &[u8]) -> Self {
        Self::detect_with_preference(bytes, Self::Cp932)
    }
    /// Keep the established encoding for ambiguous or ASCII-only sources.
    /// Change it only when the other encoding alone can decode the source.
    pub fn detect_with_preference(bytes: &[u8], preference: Self) -> Self {
        Self::detect_unambiguous(bytes).unwrap_or(preference)
    }
    pub(crate) fn character_length(self, first: u8) -> usize {
        let double = match self {
            Self::Cp932 => matches!(first, 0x81..=0x9f | 0xe0..=0xfc),
            Self::Big5 => matches!(first, 0x81..=0xfe),
        };
        if double { 2 } else { 1 }
    }
    pub(crate) fn codec(self) -> &'static encoding_rs::Encoding {
        match self {
            Self::Cp932 => encoding_rs::SHIFT_JIS,
            Self::Big5 => encoding_rs::BIG5,
        }
    }
}

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
    pub shadow_color: [u8; 3],
    pub shadow_offset: [u32; 2],
    pub shadow_opacity: u32,
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
    // DrawImageGlyph carries the binding separately in serialized events.
    #[serde(skip)]
    pub image: [u32; 2],
    #[serde(skip)]
    pub glyph_task: u32,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: [255; 3],
            opacity: 256,
            effects: 0,
            shadow_color: [0; 3],
            shadow_offset: [1; 2],
            shadow_opacity: 256,
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
            image: [u32::MAX, 0],
            glyph_task: u32::MAX,
        }
    }
}
impl TextStyle {
    /// Validate the complete control stream before changing the retained style.
    pub fn configure(&self, bytes: &[u8]) -> Result<(Self, Option<TextClock>)> {
        self.configure_for_version(bytes, EngineVersion::default())
    }

    pub(crate) fn configure_for_version(
        &self,
        bytes: &[u8],
        version: EngineVersion,
    ) -> Result<(Self, Option<TextClock>)> {
        let (style, clock, _) = self.parse(bytes, false, version)?;
        Ok((style, clock))
    }

    pub(crate) fn prefix(
        &self,
        bytes: &[u8],
        version: EngineVersion,
    ) -> Result<(Self, Option<TextClock>, usize)> {
        self.parse(bytes, true, version)
    }

    fn parse(
        &self,
        bytes: &[u8],
        one: bool,
        version: EngineVersion,
    ) -> Result<(Self, Option<TextClock>, usize)> {
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
                // v2.36's 425fc0 dispatches 'n' to the plain return at 42697b:
                // only the command byte is consumed. v2.47 uses it to switch
                // text contexts (432466), which is still unresolved.
                b'n' if version == EngineVersion::V2_36 => {}
                // v2.49: 4385d4 parses a context index (437890 defaults an
                // omitted number to zero), then swaps it with the active
                // context via 437670. The VM currently only exposes context
                // zero, so selecting zero preserves its style and cursor.
                b'n' if version == EngineVersion::V2_49 => {
                    let context = parse_number(bytes, &mut at, true)?;
                    ensure!(
                        context == 0,
                        "nonzero inline text context _n{context} is unresolved"
                    );
                }
                b'e' => result.effects = number(bytes, &mut at)?,
                b'g' => {
                    result.image[0] = number(bytes, &mut at)?;
                    ensure!(
                        result.image[0] < 256 || result.image[0] == u32::MAX,
                        "image text slot out of bounds"
                    );
                    if at > 0 && bytes[at - 1] == b',' {
                        result.image[1] = number(bytes, &mut at)?;
                    }
                }
                b't' => {
                    // v2.48 432624 stores the optional per-glyph task index.
                    ensure!(
                        bytes.get(at) == Some(&b'/'),
                        "unsupported text task control"
                    );
                    at += 1;
                    let task = number(bytes, &mut at)?;
                    ensure!(
                        task == u32::MAX || task < 1000,
                        "text callback task {task} out of bounds"
                    );
                    result.glyph_task = task;
                }
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
                b'S' => {
                    if bytes.get(at) == Some(&b'a') {
                        at += 1;
                        result.shadow_opacity = number(bytes, &mut at)?;
                    } else {
                        for component in &mut result.shadow_color {
                            *component = number(bytes, &mut at)? as u8;
                        }
                        for offset in &mut result.shadow_offset {
                            *offset = number(bytes, &mut at)?;
                        }
                        if at > 0 && bytes[at - 1] == b',' {
                            result.shadow_opacity = number(bytes, &mut at)?;
                        }
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
                _ => anyhow::bail!(
                    "unsupported text control _{} ({command:#04x}) at byte {}",
                    char::from(command),
                    at - 2
                ),
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
    parse_number(bytes, at, false)
}

fn parse_number(bytes: &[u8], at: &mut usize, allow_omitted: bool) -> Result<u32> {
    while bytes.get(*at) == Some(&b' ') {
        *at += 1;
    }
    ensure!(
        bytes.get(*at) != Some(&b'{'),
        "text-control number expressions are unresolved at byte {}",
        *at
    );
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
    ensure!(
        digits != 0 || allow_omitted,
        "missing text-control number at byte {}",
        *at
    );
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
    fn source_encoding_detection_preserves_cp932_and_accepts_valid_big5() {
        let (big5, _, bad) = encoding_rs::BIG5.encode("善意的虛偽，勝於引起爭端的事實。");
        assert!(!bad);
        assert_eq!(TextEncoding::detect(&big5), TextEncoding::Big5);
        assert_eq!(
            TextEncoding::detect_unambiguous(&big5),
            Some(TextEncoding::Big5)
        );
        let (japanese, _, bad) = encoding_rs::SHIFT_JIS.encode("日本語の文章です。");
        assert!(!bad);
        assert_eq!(TextEncoding::detect(&japanese), TextEncoding::Cp932);
        assert_eq!(TextEncoding::detect(b"ASCII"), TextEncoding::Cp932);
        assert_eq!(TextEncoding::detect_unambiguous(b"ASCII"), None);
        assert_eq!(TextEncoding::detect_unambiguous(&[0x81]), None);
        assert_eq!(TextEncoding::detect(&[0x81]), TextEncoding::Cp932);
        assert_eq!(TextEncoding::Big5.character_length(0xb4), 2);
        assert_eq!(TextEncoding::Cp932.character_length(0xb4), 1);
    }
    #[test]
    fn image_font_controls_and_disabled_character_callback_parse_in_sequence() {
        let style = TextStyle::default();
        let (style, _) = style.configure(b"_g7,8_t/-1").unwrap();
        assert_eq!(style.image, [7, 8]);
        let (style, _) = style.configure(b"_g-1").unwrap();
        assert_eq!(style.image, [u32::MAX, 8]);
        assert!(style.configure(b"_g256").is_err());
        assert_eq!(style.configure(b"_t/1").unwrap().0.glyph_task, 1);
        assert!(style.configure(b"_t/1000").is_err());
    }
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
