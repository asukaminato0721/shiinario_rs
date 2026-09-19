//! Fixed-advance glyph placement recovered from 00432f00. This layer receives
//! already converted CP932 glyph bytes; it does not execute inline commands.
use anyhow::{Result, ensure};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TextLayout {
    /// Line origin X, current X, current Y, with the original wrapping arithmetic.
    pub cursor: [u32; 3],
    pub previous: [u32; 2],
    pub wrapped: bool,
    pub wrapped_after: bool,
    pub punctuation_overhang: bool,
    pub processed_bytes: u32,
}

impl TextLayout {
    /// 431730 resets per-string bookkeeping but retains the prior wrap flags.
    pub fn begin(&mut self, cursor: [u32; 3]) {
        self.cursor = cursor;
        self.previous = [cursor[1], cursor[2]];
        self.punctuation_overhang = false;
        self.processed_bytes = 0;
    }

    pub fn new(cursor: [u32; 3]) -> Self {
        Self {
            cursor,
            previous: [cursor[1], cursor[2]],
            wrapped: true,
            wrapped_after: false,
            punctuation_overhang: false,
            processed_bytes: 0,
        }
    }

    /// Place one converted 1/2-byte glyph and return its position. `next` is the
    /// next raw CP932 character, or empty at the end of the source string.
    /// Proportional metrics and hanging-indent controls need a separate path.
    pub fn place(
        &mut self,
        glyph: &[u8],
        next: &[u8],
        advances: [u32; 2],
        line_advance: u32,
        line_limit: u32,
        punctuation: &[Vec<u8>; 3],
    ) -> Result<[u32; 2]> {
        ensure!((1..=2).contains(&glyph.len()), "invalid CP932 glyph length");
        ensure!(next.len() <= 2, "invalid next CP932 character length");
        ensure!(
            advances[0] != u32::MAX,
            "proportional text layout is unresolved"
        );
        let advance = advances[glyph.len() - 1];
        if self.cursor[1].wrapping_add(advance) >= line_limit && contains(&punctuation[1], glyph) {
            self.newline(line_advance);
        }
        if self.punctuation_overhang {
            self.punctuation_overhang = false;
        } else if (self.cursor[1] == self.cursor[0] || self.wrapped_after) && self.wrapped {
            if contains(&punctuation[2], glyph) {
                if contains(&punctuation[2], next) || !contains(&punctuation[0], next) {
                    self.hang_punctuation();
                }
            } else if contains(&punctuation[0], glyph) {
                self.hang_punctuation();
            }
            self.wrapped_after = false;
        }
        let position = [self.cursor[1], self.cursor[2]];
        self.cursor[1] = self.cursor[1].wrapping_add(advance);
        self.processed_bytes = self.processed_bytes.wrapping_add(glyph.len() as u32);
        self.wrapped = false;
        if self.cursor[1] >= line_limit {
            self.newline(line_advance);
            self.wrapped_after = true;
        }
        Ok(position)
    }

    fn hang_punctuation(&mut self) {
        self.cursor[1] = self.previous[0];
        self.cursor[2] = self.previous[1];
        self.punctuation_overhang = true;
    }

    fn newline(&mut self, line_advance: u32) {
        self.previous = [self.cursor[1], self.cursor[2]];
        self.cursor[1] = self.cursor[0];
        self.cursor[2] = self.cursor[2].wrapping_add(line_advance);
        self.wrapped = true;
    }
}

fn contains(list: &[u8], glyph: &[u8]) -> bool {
    if glyph.is_empty() {
        return true;
    }
    list.windows(glyph.len()).any(|bytes| bytes == glyph)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_layout_matches_original_positions_and_punctuation_state() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/text-layout-probe.json"
        ))
        .unwrap();
        let decode = |hex: &str| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let cursor: [u32; 3] = serde_json::from_value(case["cursor"].clone()).unwrap();
            let mut layout = TextLayout::new(cursor);
            let punctuation =
                std::array::from_fn(|i| decode(case["punctuation"][i].as_str().unwrap()));
            let advances: [u32; 2] = serde_json::from_value(case["advances"].clone()).unwrap();
            let glyphs = case["glyphs"].as_array().unwrap();
            let raw = decode(case["text"].as_str().unwrap());
            let mut offset = 0;
            for expected in glyphs {
                // Conversion is intentionally outside this layout-only test.
                let glyph = decode(expected["bytes"].as_str().unwrap());
                let width = if matches!(raw[offset], 0x81..=0x9f | 0xe0..=0xfc) {
                    2
                } else {
                    1
                };
                offset += width;
                let next = if offset == raw.len() {
                    &[]
                } else {
                    let width = if matches!(raw[offset], 0x81..=0x9f | 0xe0..=0xfc) {
                        2
                    } else {
                        1
                    };
                    &raw[offset..offset + width]
                };
                let actual = layout
                    .place(
                        &glyph,
                        next,
                        advances,
                        case["line_advance"].as_u64().unwrap() as u32,
                        case["limit"].as_u64().unwrap() as u32,
                        &punctuation,
                    )
                    .unwrap();
                assert_eq!(serde_json::json!(actual), expected["position"], "{case}");
            }
            assert_eq!(serde_json::json!(layout.cursor), case["final_cursor"]);
            assert_eq!(serde_json::json!(layout.previous), case["previous"]);
            assert_eq!(u64::from(layout.wrapped), case["wrapped"].as_u64().unwrap());
            assert_eq!(
                u64::from(layout.wrapped_after),
                case["wrapped_after"].as_u64().unwrap()
            );
            assert_eq!(
                u64::from(layout.punctuation_overhang),
                case["punctuation_overhang"].as_u64().unwrap()
            );
            assert_eq!(
                u64::from(layout.processed_bytes),
                case["processed_bytes"].as_u64().unwrap()
            );
        }
        let mut layout = TextLayout::new([0, 0, 0]);
        let before = layout.clone();
        assert!(
            layout
                .place(b"", b"", [8, 16], 16, 640, &Default::default())
                .is_err()
        );
        assert_eq!(layout, before);
        assert!(
            layout
                .place(b"a", b"", [u32::MAX, 16], 16, 640, &Default::default())
                .is_err()
        );
        assert_eq!(layout, before);
    }
}
