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
