//! Retained single-line text controls with CP932 output and bounded storage.
#[cfg(not(target_arch = "wasm32"))]
use crate::{
    resources::Surface,
    text_render::{Canvas, TextRenderer},
};
use anyhow::{Context, Result, ensure};
use shiinario_scenario::PlatformRequest;
#[cfg(not(target_arch = "wasm32"))]
use shiinario_scenario::{SharedMemory, TextStyle};
use std::collections::BTreeMap;

struct Field {
    text: String,
    #[cfg(not(target_arch = "wasm32"))]
    rect: [i32; 4],
    caret: usize,
    select_all: bool,
}
#[derive(Default)]
pub(crate) struct Edits {
    fields: BTreeMap<u32, Field>,
    focused: Option<u32>,
    #[cfg(not(target_arch = "wasm32"))]
    renderer: TextRenderer,
    preedit: String,
}
impl Edits {
    pub fn respond(&mut self, request: &PlatformRequest) -> Result<Option<u32>> {
        match request {
            PlatformRequest::CreateEdit { text, rect } => {
                ensure!(self.fields.len() < 256, "edit control slots exhausted");
                ensure!(
                    rect[2] > 0
                        && rect[3] > 0
                        && rect[2] <= 16384
                        && rect[3] <= 16384
                        && i64::from(rect[2]) * i64::from(rect[3]) <= 4_000_000,
                    "edit rectangle exceeds bounds"
                );
                ensure!(
                    text.len() <= 4096 && !text.contains('\0'),
                    "edit text exceeds bounds"
                );
                let handle = (1..=256)
                    .map(|i| 0x6000_0000 + i)
                    .find(|i| !self.fields.contains_key(i))
                    .context("edit slots exhausted")?;
                self.fields.insert(
                    handle,
                    Field {
                        text: text.clone(),
                        #[cfg(not(target_arch = "wasm32"))]
                        rect: *rect,
                        caret: text.len(),
                        select_all: false,
                    },
                );
                Ok(Some(handle))
            }
            PlatformRequest::DestroyEdit { window } => {
                let removed = self.fields.remove(window).is_some();
                if self.focused == Some(*window) {
                    self.focused = None;
                    self.preedit.clear();
                }
                Ok(Some(u32::from(removed)))
            }
            PlatformRequest::SetWindowTitle { window, title }
                if self.fields.contains_key(window) =>
            {
                self.set_text(*window, title)?;
                Ok(Some(1))
            }
            _ => Ok(None),
        }
    }
    pub fn set_text(&mut self, window: u32, text: &str) -> Result<()> {
        ensure!(
            text.len() <= 4096 && !text.contains('\0'),
            "edit text exceeds bounds"
        );
        let field = self
            .fields
            .get_mut(&window)
            .context("unknown edit control")?;
        field.text = text.into();
        field.caret = text.len();
        field.select_all = false;
        Ok(())
    }
    pub fn text(&self, window: u32, capacity: u32) -> Result<Vec<u8>> {
        ensure!(
            capacity > 0 && capacity <= 256,
            "invalid window text capacity"
        );
        let field = self.fields.get(&window).context("unknown edit control")?;
        let mut bytes = Vec::new();
        for ch in field.text.chars() {
            let mut utf8 = [0; 4];
            let (encoded, _, bad) = encoding_rs::SHIFT_JIS.encode(ch.encode_utf8(&mut utf8));
            let next = if bad {
                b"?".as_slice()
            } else {
                encoded.as_ref()
            };
            if bytes.len() + next.len() < capacity as usize {
                bytes.extend_from_slice(next);
            } else {
                return Ok(bytes);
            }
        }
        Ok(bytes)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn has_focus(&self) -> bool {
        self.focused.is_some()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn click(&mut self, point: [i32; 2]) {
        self.focused = self
            .fields
            .iter()
            .rev()
            .find(|(_, f)| {
                point[0] >= f.rect[0]
                    && point[1] >= f.rect[1]
                    && i64::from(point[0]) < i64::from(f.rect[0]) + i64::from(f.rect[2])
                    && i64::from(point[1]) < i64::from(f.rect[1]) + i64::from(f.rect[3])
            })
            .map(|(&id, _)| id);
        if let Some(id) = self.focused {
            let field = self.fields.get_mut(&id).unwrap();
            field.caret = field.text.len();
        }
        self.preedit.clear();
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn composition(&mut self, text: String) {
        self.preedit = text.chars().take(256).collect();
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn insert(&mut self, text: &str) {
        if let Some(field) = self.focused.and_then(|id| self.fields.get_mut(&id)) {
            let text: String = text.chars().filter(|c| !c.is_control()).collect();
            if field.text.len() + text.len() <= 4096 {
                if field.select_all {
                    field.text.clear();
                    field.caret = 0;
                    field.select_all = false;
                }
                field.text.insert_str(field.caret, &text);
                field.caret += text.len();
            }
            self.preedit.clear();
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn key(&mut self, vk: u32, control: bool) {
        if !self.preedit.is_empty() {
            return;
        }
        if vk == 9 && !self.fields.is_empty() {
            let next = self
                .fields
                .keys()
                .copied()
                .find(|id| self.focused.is_none_or(|current| *id > current))
                .unwrap_or(*self.fields.first_key_value().unwrap().0);
            self.focused = Some(next);
        } else if let Some(field) = self.focused.and_then(|id| self.fields.get_mut(&id)) {
            if vk == 65 && control {
                field.select_all = true;
            } else if matches!(vk, 8 | 46) && field.select_all {
                field.text.clear();
                field.caret = 0;
                field.select_all = false;
            } else {
                let before = field.text[..field.caret]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
                let after = field.text[field.caret..]
                    .chars()
                    .next()
                    .map_or(field.caret, |c| field.caret + c.len_utf8());
                match vk {
                    8 => {
                        field.text.replace_range(before..field.caret, "");
                        field.caret = before;
                    }
                    46 => {
                        field.text.replace_range(field.caret..after, "");
                    }
                    37 => field.caret = before,
                    39 => field.caret = after,
                    36 => field.caret = 0,
                    35 => field.caret = field.text.len(),
                    _ => {}
                }
                field.select_all = false;
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn paint(&mut self, background: &Surface) -> Result<Surface> {
        if self.fields.is_empty() {
            return Ok(background.clone());
        }
        let mut surface = background.clone();
        for (&handle, field) in &self.fields {
            let [x, y, width, height] = field.rect;
            ensure!(
                width > 0 && height > 0 && width <= 16384 && height <= 16384,
                "edit rectangle exceeds bounds"
            );
            let memory = SharedMemory::zeroed(width as usize * height as usize * 4)?;
            memory.write(0, &[255; 4].repeat(width as usize * height as usize))?;
            let style = TextStyle {
                color: [0; 3],
                font_height: (height - 4).clamp(8, 16) as u32,
                fullwidth_ascii: false,
                ..TextStyle::default()
            };
            let advance = |ch: char| if ch.is_ascii() { 8 } else { 16 };
            let text_width = field.text[..field.caret].chars().map(advance).sum::<i32>();
            let scroll = (text_width - (width - 8)).max(0);
            let mut at = 2 - scroll;
            for ch in field.text.chars() {
                self.renderer.draw(
                    Canvas {
                        pixels: &memory,
                        size: [width as u32, height as u32],
                        stride: width as usize * 4,
                        rgba: true,
                    },
                    [at, 2],
                    ch,
                    &style,
                )?;
                at += advance(ch);
            }
            if self.focused == Some(handle) {
                let mut at = text_width + 2 - scroll;
                for ch in self.preedit.chars() {
                    self.renderer.draw(
                        Canvas {
                            pixels: &memory,
                            size: [width as u32, height as u32],
                            stride: width as usize * 4,
                            rgba: true,
                        },
                        [at, 2],
                        ch,
                        &style,
                    )?;
                    at += advance(ch);
                }
                for row in 2..height - 2 {
                    if at >= 1 && at < width - 1 {
                        memory.write(
                            (row as usize * width as usize + at as usize) * 4,
                            &[0, 0, 0, 255],
                        )?;
                    }
                }
            }
            let pixels = memory.read(0, memory.len())?;
            for row in 0..height {
                for col in 0..width {
                    let px = i64::from(x) + i64::from(col);
                    let py = i64::from(y) + i64::from(row);
                    if px >= 0
                        && py >= 0
                        && px < i64::from(surface.width)
                        && py < i64::from(surface.height)
                    {
                        let offset = (py as usize * surface.width as usize + px as usize) * 4;
                        let value = if row == 0 || col == 0 || row == height - 1 || col == width - 1
                        {
                            &[96, 96, 96, 255][..]
                        } else {
                            let at = (row as usize * width as usize + col as usize) * 4;
                            &pixels[at..at + 4]
                        };
                        surface.rgba[offset..offset + 4].copy_from_slice(value);
                    }
                }
            }
        }
        Ok(surface)
    }
}
