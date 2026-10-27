//! A coroutine for the original flag-8 text scheduler path.
use super::*;
use crate::text_layout::TextLayout;
use shiinario_core::coroutine::{Co, Coroutine, Resume};
use std::{cell::RefCell, rc::Rc};

type VmOperation = Box<dyn FnOnce(&mut BinaryVm)>;
type PlatformReply = Box<dyn FnOnce(&mut BinaryVm, u32) -> Result<()>>;

/// A slot in the native global table of 257 inline strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TextStringIndex(u16);

impl TryFrom<u32> for TextStringIndex {
    type Error = anyhow::Error;

    fn try_from(value: u32) -> Result<Self> {
        ensure!(
            value <= 256,
            "inline text string index {value} out of bounds"
        );
        Ok(Self(value as u16))
    }
}

enum TextYield {
    WithVm(VmOperation),
    Platform {
        request: Box<PlatformRequest>,
        reply: PlatformReply,
    },
    Tick,
    Callback {
        task: u32,
    },
}

pub(super) struct AsyncText {
    pub owner: u32,
    pub surface: u32,
    pub address: u32,
    pub offset: usize,
    pub location: Location,
    pub ticking: bool,
    pub immediate: bool,
    pub staged_destination: Option<Destination>,
    // Resolve the whole string once. A legacy caption can differ from the story.
    encoding: crate::TextEncoding,
    coroutine: Option<Coroutine<TextYield>>,
    reply: Option<PlatformReply>,
}
impl AsyncText {
    pub fn new(
        owner: u32,
        surface: u32,
        address: u32,
        location: Location,
        encoding: crate::TextEncoding,
    ) -> Self {
        Self {
            owner,
            surface,
            address,
            offset: 0,
            location,
            ticking: false,
            immediate: false,
            staged_destination: None,
            encoding,
            coroutine: Some(Coroutine::new(run_text)),
            reply: None,
        }
    }
}

pub(super) struct StagedText {
    text: Option<AsyncText>,
    context: TextContext,
    completed: bool,
}
pub(super) struct TextCallback {
    previous: u32,
    stack_pointer: usize,
    text: AsyncText,
}

struct TextCo(Co<TextYield>);

impl TextCo {
    // VM access ends before the coroutine suspends. No VM borrow spans an await.
    async fn with_vm<T: 'static>(
        &self,
        operation: impl FnOnce(&mut BinaryVm) -> Result<T> + 'static,
    ) -> Result<T> {
        let result = Rc::new(RefCell::new(None));
        let reply = result.clone();
        self.0
            .yield_(TextYield::WithVm(Box::new(move |vm| {
                *reply.borrow_mut() = Some(operation(vm));
            })))
            .await;
        result
            .borrow_mut()
            .take()
            .context("missing VM operation result")?
    }

    async fn request(&self, request: PlatformRequest) -> Result<u32> {
        self.request_with_reply(request, |_, _| Ok(())).await
    }

    async fn request_with_reply(
        &self,
        request: PlatformRequest,
        commit: impl FnOnce(&mut BinaryVm, u32) -> Result<()> + 'static,
    ) -> Result<u32> {
        let result = Rc::new(RefCell::new(None));
        let reply = result.clone();
        self.0
            .yield_(TextYield::Platform {
                request: Box::new(request),
                reply: Box::new(move |vm, value| {
                    commit(vm, value)?;
                    *reply.borrow_mut() = Some(value);
                    Ok(())
                }),
            })
            .await;
        let value = result
            .borrow_mut()
            .take()
            .context("missing text platform reply")?;
        Ok(value)
    }
}

// The compiler retains local variables and the execution position across awaits.
// Each outer iteration is one original text scheduler pass.
async fn run_text(co: Co<TextYield>) -> Result<()> {
    let co = TextCo(co);
    let mut timed = co
        .with_vm(|vm| Ok(!vm.async_text.as_ref().unwrap().immediate))
        .await?;
    loop {
        if co
            .with_vm(|vm| {
                Ok(!vm.async_text.as_ref().unwrap().immediate && vm.text_style.skip_mask != 0)
            })
            .await?
        {
            let input = co
                .request(PlatformRequest::TextInput { clear: false })
                .await?;
            let skip = co
                .with_vm(move |vm| {
                    let mask = vm.text_style.skip_mask;
                    Ok((mask & 1 != 0 && input & 0x20 != 0)
                        || (mask & 2 != 0 && input & 0x10 != 0)
                        || (mask & 4 != 0 && input & 0x100 != 0)
                        || (mask & 8 != 0 && input & 0x10000 != 0))
                })
                .await?;
            if skip {
                timed = false;
            }
        }
        loop {
            match co.with_vm(BinaryVm::text_item).await? {
                TextItem::End => return Ok(()),
                TextItem::Control { clock } => {
                    if clock {
                        co.request(PlatformRequest::ClockMilliseconds).await?;
                    }
                    continue;
                }
                TextItem::Glyph => {}
            }
            if timed
                && co
                    .with_vm(|vm| Ok(vm.text_style.character_delay != 0))
                    .await?
            {
                let now = co.request(PlatformRequest::ClockMilliseconds).await?;
                let waiting = co
                    .with_vm(move |vm| {
                        Ok(now.wrapping_sub(vm.text_style.character_epoch)
                            < vm.text_style.character_delay)
                    })
                    .await?;
                if waiting {
                    co.0.yield_(TextYield::Tick).await;
                    break;
                }
                co.request_with_reply(PlatformRequest::ClockMilliseconds, |vm, now| {
                    vm.text_style.character_epoch = now;
                    Ok(())
                })
                .await?;
            }
            let Some(TextGlyph {
                layout,
                offset,
                mut request,
                position,
                character,
            }) = co.with_vm(BinaryVm::text_glyph).await?
            else {
                return Ok(());
            };
            let task = co.with_vm(|vm| Ok(vm.text_style.glyph_task)).await?;
            if task != u32::MAX {
                co.0.yield_(TextYield::Callback { task }).await;
                request = co
                    .with_vm(move |vm| {
                        Ok(if vm.text_callback_result & 1 != 0 {
                            None
                        } else {
                            vm.text_glyph_request(
                                vm.async_text.as_ref().unwrap().surface,
                                position,
                                character,
                            )
                        })
                    })
                    .await?;
            }
            if let Some(request) = request {
                // Commit layout when the host accepts the glyph, as before.
                co.request_with_reply(request, move |vm, _| {
                    vm.commit_text_glyph(layout, offset, true)
                })
                .await?;
            } else {
                co.with_vm(move |vm| vm.commit_text_glyph(layout, offset, false))
                    .await?;
            }
            if timed
                && co
                    .with_vm(|vm| Ok(vm.text_style.character_delay != 0))
                    .await?
            {
                co.0.yield_(TextYield::Tick).await;
                break;
            }
        }
    }
}

enum TextItem {
    End,
    Control { clock: bool },
    Glyph,
}

struct TextGlyph {
    layout: TextLayout,
    offset: usize,
    request: Option<PlatformRequest>,
    position: [u32; 2],
    character: char,
}

impl BinaryVm {
    fn begin_text_callback(&mut self, task: u32) -> Result<Event> {
        ensure!(
            self.text_callback.is_none(),
            "recursive text callback is unresolved"
        );
        ensure!(
            task != self.current_task,
            "text callback cannot invoke its owner task"
        );
        let &(base, pc) = self
            .task_entries
            .get(&task)
            .context("text callback task is not defined")?;
        let previous = self.current_task;
        let context = self.text_context;
        let text = self
            .async_text
            .take()
            .context("text callback has no text coroutine")?;
        self.select_task(task);
        self.select_text_context(context);
        self.text_callback = Some(TextCallback {
            previous,
            stack_pointer: self.sp,
            text,
        });
        self.text_callback_result = 0;
        self.switch_scenario(base);
        self.pc = pc;
        self.context_flags = 1;
        Ok(Event::TextCallback { task, context })
    }
    pub(super) fn finish_text_callback(&mut self) {
        let callback = self.text_callback.take().unwrap();
        self.sp = callback.stack_pointer;
        self.select_task(callback.previous);
        self.async_text = Some(callback.text);
    }
    fn text_snapshot(&self) -> TextContext {
        TextContext {
            style: self.text_style.clone(),
            image_offset: self.text_image_offset,
            cursor: self.text_cursor,
            layout: self.text_layout.clone(),
        }
    }
    pub(super) fn stage_text(&mut self, address: u32, location: Location) -> Result<()> {
        ensure!(
            self.async_text.is_none(),
            "text coroutine is already active"
        );
        let source = self.string_bytes(address)?;
        let encoding = crate::TextEncoding::detect_with_preference(&source, self.text_encoding);
        self.text_layout.begin(self.text_cursor);
        self.staged_texts.insert(
            self.text_context,
            StagedText {
                text: Some(AsyncText::new(
                    self.current_task,
                    u32::MAX,
                    address,
                    location,
                    encoding,
                )),
                context: self.text_snapshot(),
                completed: false,
            },
        );
        Ok(())
    }
    pub(super) fn resume_staged_text(
        &mut self,
        task: u32,
        destination: Destination,
        location: Location,
    ) -> Result<()> {
        ensure!(
            self.async_text.is_none(),
            "text coroutine is already active"
        );
        let staged = self
            .staged_texts
            .get_mut(&self.text_context)
            .context("text has not been staged")?;
        self.text_style = staged.context.style.clone();
        self.text_image_offset = staged.context.image_offset;
        self.text_cursor = staged.context.cursor;
        self.text_layout = staged.context.layout.clone();
        if staged.completed {
            return self.write(destination, 0);
        }
        let mut text = staged
            .text
            .take()
            .context("staged text is already active")?;
        text.surface = if task == self.current_task {
            self.text_surface
        } else {
            self.tasks.get(&task).map_or(0, |task| task.text_surface)
        };
        text.owner = self.current_task;
        text.location = location;
        text.staged_destination = Some(destination);
        self.async_text = Some(text);
        self.context_flags |= 8;
        Ok(())
    }
    pub(super) fn text_response(&mut self, event: &Event, value: u32) -> Result<()> {
        let Some(text) = self.async_text.as_mut().filter(|text| text.ticking) else {
            return Ok(());
        };
        if !matches!(event, Event::Platform { .. }) {
            return Ok(());
        }
        let reply = text
            .reply
            .take()
            .context("text continuation has no platform request")?;
        reply(self, value)
    }

    fn text_request(&mut self, request: PlatformRequest) -> Event {
        let event = Event::Platform {
            location: self.async_text.as_ref().unwrap().location.clone(),
            request,
        };
        self.pending = Some((event.clone(), None));
        event
    }

    pub(super) fn text_step(&mut self) -> Result<Event> {
        match self.text_execute() {
            Ok(event) => Ok(event),
            Err(error) => {
                let text = self.async_text.as_ref().unwrap();
                let message = format!(
                    "{}:{:#x}: {error:#}; opcode=0x{:04x}, task={}, text_byte={:#x}",
                    text.location.scenario,
                    text.location.offset,
                    if text.staged_destination.is_some() {
                        0xa1
                    } else if text.immediate {
                        0x84
                    } else {
                        0x83
                    },
                    text.owner,
                    text.offset
                );
                self.failure = Some(message.clone());
                bail!("{message}")
            }
        }
    }

    fn text_execute(&mut self) -> Result<Event> {
        let text = self.async_text.as_mut().context("missing text context")?;
        ensure!(
            text.owner == self.current_task,
            "text task changed during a pending tick"
        );
        let mut coroutine = text.coroutine.take().context("missing text coroutine")?;
        let staged = text.staged_destination.is_some();
        let result = self.resume_text(&mut coroutine);
        if let Some(text) = &mut self.async_text {
            text.coroutine = Some(coroutine);
        } else if let Some(callback) = &mut self.text_callback {
            callback.text.coroutine = Some(coroutine);
        } else if staged
            && let Some(text) = self
                .staged_texts
                .get_mut(&self.text_context)
                .and_then(|staged| staged.text.as_mut())
        {
            text.coroutine = Some(coroutine);
        }
        result
    }

    fn resume_text(&mut self, coroutine: &mut Coroutine<TextYield>) -> Result<Event> {
        loop {
            match coroutine.resume()? {
                Resume::Yielded(TextYield::WithVm(operation)) => operation(self),
                Resume::Yielded(TextYield::Platform { request, reply }) => {
                    self.async_text.as_mut().unwrap().reply = Some(reply);
                    return Ok(self.text_request(*request));
                }
                Resume::Yielded(TextYield::Tick) => return self.text_tick(false),
                Resume::Complete => return self.text_tick(true),
                Resume::Yielded(TextYield::Callback { task }) => {
                    return self.begin_text_callback(task);
                }
            }
        }
    }

    fn text_tick(&mut self, completed: bool) -> Result<Event> {
        let text = self.async_text.as_mut().unwrap();
        let event = Event::TextTick {
            task: text.owner,
            completed,
            cursor: [self.text_cursor[1], self.text_cursor[2]],
        };
        if let Some(destination) = text.staged_destination.take() {
            self.write(destination, if completed { 0 } else { 2 })?;
            let text = self.async_text.take();
            let context = self.text_snapshot();
            self.staged_texts.insert(
                self.text_context,
                StagedText {
                    text: if completed { None } else { text },
                    context,
                    completed,
                },
            );
            self.context_flags &= !8;
        } else if completed {
            self.context_flags &= !8;
            self.async_text = None;
        } else {
            text.ticking = false;
        }
        self.scheduler.dispatching = false;
        self.scheduler.pass_live |= self.context_flags & 1 != 0;
        self.scheduler.scan = self.current_task + 1;
        Ok(event)
    }

    fn text_bytes(&self) -> Result<Vec<u8>> {
        let text = self.async_text.as_ref().context("missing text context")?;
        ensure!(text.offset <= 65536, "text exceeds 64 KiB");
        let address = text
            .address
            .checked_add(text.offset as u32)
            .context("text address overflow")?;
        let bytes = self.string_bytes(address)?;
        ensure!(bytes.len() <= 65536 - text.offset, "text exceeds 64 KiB");
        Ok(bytes)
    }

    /// Precondition: this task owns an active text coroutine.
    /// Postcondition: a valid control updates its state and consumes only its bytes.
    /// An invalid control leaves the control state, cursor, and text offset unchanged.
    fn text_item(&mut self) -> Result<TextItem> {
        let bytes = self.text_bytes()?;
        if bytes.is_empty() {
            return Ok(TextItem::End);
        }
        if bytes.starts_with(b"+") && !bytes.starts_with(b"++") {
            // v2.48 4336e6: define +index followed by up to 254 raw bytes.
            // A slash ends the definition. Reaching the cap leaves the next
            // source byte for the text parser, even when that byte is a slash.
            let mut consumed = 1;
            let index =
                TextStringIndex::try_from(crate::text::parse_number(&bytes, &mut consumed, true)?)?;
            let source = &bytes[consumed..];
            let length = source
                .iter()
                .take(254)
                .position(|&byte| byte == b'/')
                .unwrap_or(source.len().min(254));
            let terminated = length < 254 && source.get(length) == Some(&b'/');
            let slot = self.text_strings.entry(index).or_insert([0; 256]);
            slot[..length].copy_from_slice(&source[..length]);
            slot[length] = 0;
            self.async_text.as_mut().unwrap().offset += consumed + length + usize::from(terminated);
            return Ok(TextItem::Control { clock: false });
        }
        if bytes.starts_with(b"~") && !bytes.starts_with(b"~~") {
            // v2.48 4337ac: an omitted number is zero. An immediate + or -
            // selects addition and is removed before parsing the number.
            // Both signs therefore add, matching the original implementation.
            let relative = matches!(bytes.get(1), Some(b'+' | b'-'));
            let mut consumed = if relative { 2 } else { 1 };
            let value = crate::text::parse_number(&bytes, &mut consumed, true)?;
            self.text_marker = if relative {
                self.text_marker.wrapping_add(value)
            } else {
                value
            };
            self.async_text.as_mut().unwrap().offset += consumed;
            return Ok(TextItem::Control { clock: false });
        }
        if !bytes.starts_with(b"_") || bytes.starts_with(b"__") {
            return Ok(TextItem::Glyph);
        }
        if bytes.starts_with(b"_r") {
            let forced = bytes.get(2) == Some(&b'!');
            if forced {
                self.text_layout.wrapped = false;
            }
            if !self.text_layout.wrapped {
                self.text_cursor[1] = self.text_cursor[0];
                self.text_cursor[2] =
                    self.text_cursor[2].wrapping_add(self.text_style.line_advance);
                self.text_layout.cursor = self.text_cursor;
            }
            self.async_text.as_mut().unwrap().offset += if forced { 3 } else { 2 };
            return Ok(TextItem::Control { clock: false });
        }
        let (style, clock, consumed) = self.text_style.prefix(&bytes, self.version)?;
        self.async_text.as_mut().unwrap().offset += consumed;
        if let Some(clock) = clock {
            self.pending_text_style = Some((style, clock));
            Ok(TextItem::Control { clock: true })
        } else {
            self.text_style = style;
            Ok(TextItem::Control { clock: false })
        }
    }

    fn text_glyph(&mut self) -> Result<Option<TextGlyph>> {
        let text = self.async_text.as_ref().context("missing text context")?;
        let surface = text.surface;
        let offset = text.offset;
        let encoding = text.encoding;
        let bytes = self.text_bytes()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let escaped = matches!(
            bytes[0],
            b'_' | b'$' | b'*' | b'+' | b'@' | b'{' | b'}' | b'~'
        );
        let start = usize::from(escaped);
        if escaped {
            ensure!(
                bytes.get(1) == Some(&bytes[0]),
                "unresolved inline text command {:#x}",
                bytes[0]
            );
        }
        let length = encoding.character_length(bytes[start]);
        ensure!(
            start + length <= bytes.len(),
            "truncated {} text character",
            encoding.codec().name()
        );
        let glyph = &bytes[start..start + length];
        ensure!(
            !self.text_style.fullwidth_ascii
                || length == 2
                || (glyph == b" " && !self.text_style.fullwidth_spaces),
            "single-byte text conversion is unresolved"
        );
        let consumed = start + length;
        let next = if consumed == bytes.len() {
            &[]
        } else {
            let end = consumed + encoding.character_length(bytes[consumed]);
            ensure!(
                end <= bytes.len(),
                "truncated following {} character",
                encoding.codec().name()
            );
            &bytes[consumed..end]
        };
        let (decoded, _, bad) = encoding.codec().decode(glyph);
        ensure!(!bad, "invalid {} text character", encoding.codec().name());
        let mut chars = decoded.chars();
        let character = chars.next().context("empty decoded glyph")?;
        ensure!(
            chars.next().is_none() && !character.is_control(),
            "unresolved text character {character:?}"
        );
        let mut layout = self.text_layout.clone();
        let position = layout.place(
            glyph,
            next,
            [self.text_style.half_advance, self.text_style.full_advance],
            self.text_style.line_advance,
            self.text_style.line_limit,
            &self.text_style.punctuation,
        )?;
        let request = self.text_glyph_request(surface, position, character);
        Ok(Some(TextGlyph {
            layout,
            offset: offset + consumed,
            request,
            position,
            character,
        }))
    }

    fn text_glyph_request(
        &self,
        surface: u32,
        position: [u32; 2],
        character: char,
    ) -> Option<PlatformRequest> {
        if self.text_style.image[0] != u32::MAX {
            Some(PlatformRequest::DrawImageGlyph {
                image: self.text_style.image[0],
                frame: self.text_style.image[1],
                position: [
                    position[0].wrapping_add(self.text_image_offset[0]) as i32,
                    position[1].wrapping_add(self.text_image_offset[1]) as i32,
                ],
                character,
                style: self.text_style.clone(),
            })
        } else if surface != u32::MAX {
            Some(PlatformRequest::DrawGlyph {
                surface,
                position: position.map(|v| v as i32),
                character,
                style: self.text_style.clone(),
            })
        } else {
            None
        }
    }

    fn commit_text_glyph(&mut self, layout: TextLayout, offset: usize, drawn: bool) -> Result<()> {
        let text = self.async_text.as_mut().context("missing text context")?;
        if drawn && self.text_style.effects & 1 != 0 {
            self.text_style.opacity = self.text_style.shadow_opacity;
        }
        self.text_cursor = layout.cursor;
        self.text_layout = layout;
        text.offset = offset;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_string_definitions_match_native_boundaries_and_preserve_padding() {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        let cases = [
            (b"+7hello/after".to_vec(), 7, b"hello".to_vec(), 8),
            (b"+E4508".to_vec(), 0, b"E4508".to_vec(), 6),
            (b"+256upper/".to_vec(), 256, b"upper".to_vec(), 10),
            (b"+x100Z/".to_vec(), 256, b"Z".to_vec(), 7),
            (b"+0/".to_vec(), 0, vec![], 3),
            (b"+".to_vec(), 0, vec![], 1),
            (
                [b"+0".as_slice(), &[b'a'; 254], b"/Z"].concat(),
                0,
                vec![b'a'; 254],
                256,
            ),
            (
                [b"+1".as_slice(), &[b'a'; 255], b"/"].concat(),
                1,
                vec![b'a'; 254],
                256,
            ),
        ];
        for (source, index, expected, consumed) in cases {
            let mut vm = BinaryVm::new("definition.scn", vec![0; 7]).unwrap();
            let address = 0x2000_0000;
            vm.allocations
                .insert(address, [source.as_slice(), &[0]].concat());
            vm.async_text = Some(AsyncText::new(
                0,
                0,
                address,
                vm.location(),
                vm.text_encoding,
            ));
            let index = TextStringIndex::try_from(index).unwrap();
            vm.text_strings.insert(index, [0xa5; 256]);
            let cursor = vm.text_cursor;
            assert!(matches!(
                vm.text_item().unwrap(),
                TextItem::Control { clock: false }
            ));
            assert_eq!(vm.async_text.as_ref().unwrap().offset, consumed);
            assert_eq!(vm.text_cursor, cursor);
            let slot = vm.text_strings[&index];
            assert_eq!(&slot[..expected.len()], expected);
            assert_eq!(slot[expected.len()], 0);
            assert!(slot[expected.len() + 1..].iter().all(|&byte| byte == 0xa5));
            digest.update((consumed as u32).to_le_bytes());
            digest.update(slot);
            vm.select_text_context(1);
            assert_eq!(vm.text_strings[&index], slot);
        }
        // Original 4336e6 and 431d60 executed for all eight cases.
        assert_eq!(
            format!("{:x}", digest.finalize()),
            "903d19c2ce20075b455338e11a529a5e67eb13bf38c600d32487e920547e2ffe"
        );
        for source in [b"+257bad/".as_slice(), b"+-1bad/", b"+{1}bad/"] {
            let mut vm = BinaryVm::new("invalid.scn", vec![0; 7]).unwrap();
            let address = 0x2000_0000;
            vm.allocations.insert(address, [source, &[0]].concat());
            vm.async_text = Some(AsyncText::new(
                0,
                0,
                address,
                vm.location(),
                vm.text_encoding,
            ));
            assert!(vm.text_item().is_err());
            assert!(vm.text_strings.is_empty());
            assert_eq!(vm.async_text.as_ref().unwrap().offset, 0);
        }
    }

    #[test]
    fn escaped_plus_is_a_glyph_and_unescaped_plus_defines_the_following_bytes() {
        let mut vm = BinaryVm::new("definition.scn", vec![0; 7]).unwrap();
        let address = 0x2000_0000;
        vm.allocations.insert(address, b"++\0".to_vec());
        vm.async_text = Some(AsyncText::new(
            0,
            0,
            address,
            vm.location(),
            vm.text_encoding,
        ));
        vm.text_style.fullwidth_ascii = false;
        assert!(matches!(vm.text_item().unwrap(), TextItem::Glyph));
        let glyph = vm.text_glyph().unwrap().unwrap();
        assert_eq!(glyph.character, '+');
        assert_eq!(glyph.offset, 2);
        assert!(vm.text_strings.is_empty());

        // The localized HANAOTO source uses an omitted index before E4508.
        let (source, _, bad) = encoding_rs::BIG5.encode("+E4508，很不好意思啊。");
        assert!(!bad);
        vm.allocations
            .insert(address, [source.as_ref(), &[0]].concat());
        assert!(matches!(
            vm.text_item().unwrap(),
            TextItem::Control { clock: false }
        ));
        assert_eq!(vm.async_text.as_ref().unwrap().offset, source.len());
        let slot = &vm.text_strings[&TextStringIndex::try_from(0).unwrap()];
        assert_eq!(&slot[..source.len() - 1], &source[1..]);
        assert!(matches!(vm.text_item().unwrap(), TextItem::End));
    }

    #[test]
    fn inline_markers_match_native_numbers_and_remain_available_to_opcode_0069() {
        // v2.48 4337ac and 431d60. Offsets include the leading tilde.
        for (source, initial, expected, consumed) in [
            (b"~!".as_slice(), 99, 0, 1),
            (b"~7A", 99, 7, 2),
            (b"~+2B", u32::MAX, 1, 3),
            (b"~-3C", 7, 10, 3),
            (b"~ xF,D", 99, 15, 5),
            (b"~ -4E", 99, (-4i32) as u32, 4),
            (b"~--5F", 7, 2, 4),
            (b"~", 99, 0, 1),
        ] {
            let mut vm = BinaryVm::new("marker.scn", vec![0x69, 0, 12, 0, 0]).unwrap();
            let address = 0x2000_0000;
            vm.allocations.insert(address, [source, &[0]].concat());
            vm.async_text = Some(AsyncText::new(
                0,
                0,
                address,
                vm.location(),
                vm.text_encoding,
            ));
            vm.text_marker = initial;
            let cursor = vm.text_cursor;
            let layout = vm.text_layout.clone();
            assert!(matches!(
                vm.text_item().unwrap(),
                TextItem::Control { clock: false }
            ));
            assert_eq!(vm.text_marker, expected, "{source:?}");
            assert_eq!(vm.async_text.as_ref().unwrap().offset, consumed);
            assert_eq!(vm.text_cursor, cursor);
            assert_eq!(vm.text_layout, layout);
            vm.step().unwrap();
            assert_eq!(vm.banks[&12][0], expected);
        }
        let mut vm = BinaryVm::new("marker.scn", vec![0; 7]).unwrap();
        let address = 0x2000_0000;
        vm.allocations.insert(address, b"~{1}\0".to_vec());
        vm.async_text = Some(AsyncText::new(
            0,
            0,
            address,
            vm.location(),
            vm.text_encoding,
        ));
        vm.text_marker = 42;
        assert!(vm.text_item().is_err());
        assert_eq!(vm.text_marker, 42);
        assert_eq!(vm.async_text.as_ref().unwrap().offset, 0);
        vm.allocations.insert(address, b"~~\0".to_vec());
        vm.text_style.fullwidth_ascii = false;
        assert!(matches!(vm.text_item().unwrap(), TextItem::Glyph));
        assert_eq!(vm.text_glyph().unwrap().unwrap().character, '~');
        assert_eq!(vm.text_marker, 42);
    }

    #[test]
    fn hanaoto_big5_dialogue_continues_after_a_marker_without_a_number() {
        let text = "那裡不行~！那、那裡到底是……啊啊！";
        let (encoded, _, bad) = encoding_rs::BIG5.encode(text);
        assert!(!bad);
        let mut code = vec![0x84, 0, 4, 0, 0, 0, 0, 0x10];
        code.extend_from_slice(&encoded);
        code.extend([0, 0, 0, 4, 0, 0, 0, 0]);
        let mut vm = BinaryVm::new("hanaoto.scn", code).unwrap();
        vm.set_text_encoding(crate::TextEncoding::Big5);
        let mut glyphs = String::new();
        let ended = (0..1000).any(|_| match vm.scheduled_step().unwrap() {
            Event::End => true,
            Event::SchedulerPoll => {
                vm.respond(1).unwrap();
                false
            }
            Event::Platform {
                request: PlatformRequest::DrawGlyph { character, .. },
                ..
            } => {
                glyphs.push(character);
                vm.respond(1).unwrap();
                false
            }
            Event::Platform { request, .. } => panic!("unexpected text request: {request:?}"),
            _ => false,
        });
        assert!(ended);
        assert_eq!(glyphs, text.replace('~', ""));
        assert_eq!(vm.text_marker, 0);
    }

    #[test]
    fn short_big5_sources_keep_the_encoding_of_the_previous_story() {
        let mut vm = BinaryVm::new("localized.scn", vec![0; 7]).unwrap();
        let (source, _, bad) = encoding_rs::BIG5.encode("善意的虛偽，勝於引起爭端的事實。");
        assert!(!bad);
        vm.detect_text_encoding(&source);
        assert_eq!(vm.text_encoding, crate::TextEncoding::Big5);
        // These two Big5 characters also form four valid CP932 halfwidth glyphs.
        let short = b"\xb4\xab\xc0\xc9\0";
        assert!(
            encoding_rs::SHIFT_JIS
                .decode_without_bom_handling_and_without_replacement(short)
                .is_some()
        );
        vm.detect_text_encoding(short);
        vm.detect_text_encoding(b"; ASCII comment\r\n");
        assert_eq!(vm.text_encoding, crate::TextEncoding::Big5);
        let address = 0x2000_0000;
        vm.allocations.insert(address, short.to_vec());
        vm.async_text = Some(AsyncText::new(
            0,
            0,
            address,
            vm.location(),
            vm.text_encoding,
        ));
        for expected in ['換', '檔'] {
            let glyph = vm.text_glyph().unwrap().unwrap();
            assert_eq!(glyph.character, expected);
            vm.commit_text_glyph(glyph.layout, glyph.offset, false)
                .unwrap();
        }
        assert!(vm.text_glyph().unwrap().is_none());
        let (japanese, _, _) = encoding_rs::SHIFT_JIS.encode("日本語の文章です。");
        vm.detect_text_encoding(&japanese);
        assert_eq!(vm.text_encoding, crate::TextEncoding::Cp932);
    }

    #[test]
    fn mixed_save_captions_keep_the_story_encoding_for_the_next_text() {
        for opcode in [0x0083u16, 0x0084, 0x00a0] {
            let mut code = Vec::new();
            for (codec, caption) in [
                (encoding_rs::SHIFT_JIS, "「え、いや……」_..."),
                (encoding_rs::BIG5, "換檔"),
            ] {
                let (bytes, _, bad) = codec.encode(caption);
                assert!(!bad);
                code.extend(opcode.to_le_bytes());
                if opcode != 0x00a0 {
                    code.extend(immediate(0));
                }
                code.push(0x10);
                code.extend_from_slice(&bytes);
                code.push(0);
                if opcode == 0x00a0 {
                    for _ in 0..=caption.chars().count() {
                        code.extend(0x00a1u16.to_le_bytes());
                        code.extend(immediate(0));
                        code.extend([12, 0, 0]);
                    }
                }
            }
            code.extend(0u16.to_le_bytes());
            code.extend(immediate(0));
            let mut vm =
                BinaryVm::with_version("mixed-save.scn", code, EngineVersion::V2_48).unwrap();
            vm.set_text_encoding(crate::TextEncoding::Big5);
            vm.text_style.fullwidth_ascii = false;
            let mut glyphs = String::new();
            let ended = (0..2000).any(|_| match vm.scheduled_step().unwrap() {
                Event::End => true,
                Event::SchedulerPoll => {
                    vm.respond(1).unwrap();
                    false
                }
                Event::Platform {
                    request: PlatformRequest::DrawGlyph { character, .. },
                    ..
                } => {
                    glyphs.push(character);
                    vm.respond(1).unwrap();
                    false
                }
                Event::Platform {
                    request: PlatformRequest::ClockMilliseconds | PlatformRequest::TextInput { .. },
                    ..
                } => {
                    vm.respond(0).unwrap();
                    false
                }
                Event::Platform { request, .. } => panic!("unexpected text request: {request:?}"),
                _ => false,
            });
            assert!(ended, "text opcode {opcode:#06x}");
            assert_eq!(glyphs, "「え、いや……」..換檔");
            assert_eq!(vm.text_encoding, crate::TextEncoding::Big5);
        }
    }

    #[test]
    fn malformed_text_remains_an_error_when_neither_codec_accepts_it() {
        let bytes = b"\x81\x30";
        assert!(crate::TextEncoding::detect_unambiguous(bytes).is_none());
        let mut code = vec![0x84, 0, 4, 0, 0, 0, 0, 0x10];
        code.extend(bytes);
        code.push(0);
        let mut vm = BinaryVm::new("malformed.scn", code).unwrap();
        vm.set_text_encoding(crate::TextEncoding::Big5);
        let cursor = vm.text_cursor;
        let error = (0..50)
            .find_map(|_| match vm.scheduled_step() {
                Err(error) => Some(error),
                Ok(Event::SchedulerPoll) => {
                    vm.respond(1).unwrap();
                    None
                }
                Ok(Event::Platform { request, .. }) => {
                    panic!("malformed text issued a platform request: {request:?}")
                }
                _ => None,
            })
            .unwrap();
        assert!(error.to_string().contains("invalid Big5 text character"));
        assert_eq!(vm.text_cursor, cursor);
        assert_eq!(vm.text_encoding, crate::TextEncoding::Big5);
    }

    #[test]
    fn immediate_surface_text_draws_without_input_or_character_delays() {
        for surface in [0, 7, u32::MAX] {
            let mut code = 0x0084u16.to_le_bytes().to_vec();
            code.extend(immediate(surface));
            code.extend(b"\x10AB\0");
            code.extend(0u16.to_le_bytes());
            code.extend(immediate(0));
            let mut vm = BinaryVm::new("immediate.scn", code).unwrap();
            vm.text_surface = 9;
            vm.text_style.fullwidth_ascii = false;
            vm.text_style.character_delay = 1000;
            vm.text_style.skip_mask = 8;
            let mut glyphs = Vec::new();
            let mut ended = false;
            for _ in 0..50 {
                match vm.scheduled_step().unwrap() {
                    Event::Platform {
                        request:
                            PlatformRequest::DrawGlyph {
                                surface: target,
                                character,
                                ..
                            },
                        ..
                    } => {
                        assert_eq!(target, surface);
                        glyphs.push(character);
                        vm.respond(1).unwrap();
                    }
                    Event::Platform { request, .. } => panic!("unexpected request: {request:?}"),
                    Event::SchedulerPoll => vm.respond(1).unwrap(),
                    Event::End => {
                        ended = true;
                        break;
                    }
                    _ => {}
                }
            }
            assert!(ended);
            assert_eq!(
                glyphs,
                if surface == u32::MAX {
                    vec![]
                } else {
                    vec!['A', 'B']
                }
            );
            assert_eq!(vm.text_cursor[1], 2 * vm.text_style.half_advance);
            assert_eq!(vm.text_surface, 9);
        }
    }

    #[test]
    fn glyph_callbacks_run_before_drawing_and_can_change_the_image_or_skip_it() {
        for result in [0u32, 1] {
            let mut code = 0x0083u16.to_le_bytes().to_vec();
            code.extend(immediate(0));
            code.extend(b"\x10AB\0");
            code.extend(0u16.to_le_bytes());
            code.extend(immediate(0));
            let entry = code.len();
            code.extend(0x0391u16.to_le_bytes());
            code.extend([14, 2, 0]);
            code.extend(0x00b4u16.to_le_bytes());
            code.extend(immediate(7));
            code.extend([14, 2, 0]);
            code.extend(0u16.to_le_bytes());
            code.extend(immediate(result));
            let mut vm = BinaryVm::new("glyph-callback.scn", code).unwrap();
            vm.define_task(10, vm.script_base, entry, false);
            vm.text_style.fullwidth_ascii = false;
            vm.text_style.glyph_task = 10;
            let mut frames = Vec::new();
            let mut callbacks = 0;
            let mut ended = false;
            for _ in 0..100 {
                match vm.scheduled_step().unwrap() {
                    Event::TextCallback {
                        task: 10,
                        context: 0,
                    } => callbacks += 1,
                    Event::Platform {
                        request:
                            PlatformRequest::DrawImageGlyph {
                                image: 7, frame, ..
                            },
                        ..
                    } => {
                        frames.push(frame);
                        vm.respond(1).unwrap();
                    }
                    Event::Platform {
                        request: PlatformRequest::DrawGlyph { .. },
                        ..
                    } => panic!("callback did not replace the drawing target"),
                    Event::Platform { .. } | Event::SchedulerPoll => vm.respond(1).unwrap(),
                    Event::End => {
                        ended = true;
                        break;
                    }
                    _ => {}
                }
            }
            assert!(ended);
            assert_eq!(callbacks, 2);
            assert_eq!(frames, if result == 0 { vec![1, 2] } else { vec![] });
            assert_eq!(vm.current_task, 0);
            assert_eq!(vm.sp, CELLS);
            assert_eq!(vm.tasks[&10].sp, CELLS);
            assert_eq!(vm.banks[&14][2], 2);
        }
    }
    #[test]
    fn big5_glyphs_preserve_source_offsets_and_double_byte_advances() {
        let mut vm = BinaryVm::new("big5.scn", vec![0; 7]).unwrap();
        vm.set_text_encoding(crate::TextEncoding::Big5);
        let (bytes, _, bad) = encoding_rs::BIG5.encode("繁體中文");
        assert!(!bad);
        let address = 0x2000_0000u32;
        let mut data = bytes.to_vec();
        data.push(0);
        vm.allocations.insert(address, data);
        vm.async_text = Some(AsyncText::new(
            0,
            0,
            address,
            vm.location(),
            vm.text_encoding,
        ));
        let advance = vm.text_style.full_advance;
        for (index, expected) in "繁體中文".chars().enumerate() {
            let glyph = vm.text_glyph().unwrap().unwrap();
            assert_eq!(glyph.offset, (index + 1) * 2);
            assert!(
                matches!(glyph.request, Some(PlatformRequest::DrawGlyph { character, .. }) if character == expected)
            );
            vm.commit_text_glyph(glyph.layout, glyph.offset, false)
                .unwrap();
        }
        assert_eq!(vm.text_cursor[1], 4 * advance);
        assert!(vm.text_glyph().unwrap().is_none());
        let (source, _, _) =
            encoding_rs::BIG5.encode(";備註\r\n$WAIT,1\r\n善意的虛偽，勝於引起爭端的事實。\r\n");
        let parsed = crate::TextScript::parse("localized.txt", &source).unwrap();
        assert_eq!(parsed.records[0].location().offset, 7);
        assert!(
            matches!(&parsed.records[1], crate::Record::Dialogue { text, .. } if text == "善意的虛偽，勝於引起爭端的事實。")
        );
    }
    #[test]
    fn staged_text_returns_to_the_script_between_timed_glyphs() {
        let mut code = 0x00a0u16.to_le_bytes().to_vec();
        code.extend(b"\x10AB\0");
        for index in 0..4u16 {
            code.extend(0x00a1u16.to_le_bytes());
            code.extend(immediate(0));
            code.push(12);
            code.extend(index.to_le_bytes());
        }
        code.extend(0u16.to_le_bytes());
        code.extend(immediate(0));
        let mut vm = BinaryVm::new("staged.scn", code).unwrap();
        vm.text_style.fullwidth_ascii = false;
        vm.text_style.character_delay = 10;
        let mut now = 0u32;
        let mut glyphs = Vec::new();
        let mut ended = false;
        for _ in 0..100 {
            match vm.scheduled_step().unwrap() {
                Event::Platform {
                    request: PlatformRequest::ClockMilliseconds,
                    ..
                } => {
                    now += 10;
                    vm.respond(now).unwrap();
                }
                Event::Platform {
                    request:
                        PlatformRequest::DrawGlyph {
                            character, surface, ..
                        },
                    ..
                } => {
                    assert_eq!(surface, 0);
                    glyphs.push(character);
                    vm.respond(1).unwrap();
                }
                Event::Platform { .. } | Event::SchedulerPoll => vm.respond(1).unwrap(),
                Event::End => {
                    ended = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(ended);
        assert_eq!(glyphs, ['A', 'B']);
        assert_eq!(&vm.banks[&12][..4], &[2, 2, 0, 0]);
        assert!(vm.async_text.is_none());
        assert_eq!(vm.context_flags & 8, 0);
    }

    fn immediate(value: u32) -> Vec<u8> {
        let mut bytes = vec![4];
        bytes.extend(value.to_le_bytes());
        bytes
    }

    fn text_vm(bytes: &[u8]) -> BinaryVm {
        let mut code = 0u16.to_le_bytes().to_vec();
        code.extend(immediate(0));
        let address = SCRIPT_BASE + code.len() as u32;
        code.extend(bytes);
        code.push(0);
        let mut vm = BinaryVm::new("text.scn", code).unwrap();
        vm.text_style.fullwidth_ascii = false;
        vm.context_flags |= 8;
        vm.text_layout.begin(vm.text_cursor);
        vm.async_text = Some(AsyncText::new(
            0,
            0,
            address,
            vm.location(),
            vm.text_encoding,
        ));
        vm
    }

    fn next_text(vm: &mut BinaryVm) -> Event {
        loop {
            let event = vm.scheduled_step().unwrap();
            if event == Event::SchedulerPoll {
                vm.respond(1).unwrap();
            } else {
                return event;
            }
        }
    }

    #[test]
    fn failed_glyph_reply_can_retry_and_success_commits_before_resume() {
        let mut vm = text_vm(b"AB");
        let request = next_text(&mut vm);
        assert!(matches!(
            request,
            Event::Platform {
                request: PlatformRequest::DrawGlyph { character: 'A', .. },
                ..
            }
        ));
        assert!(vm.respond(0).is_err());
        assert_eq!(vm.text_cursor, [0, 0, 0]);
        assert_eq!(vm.async_text.as_ref().unwrap().offset, 0);
        assert_eq!(vm.scheduled_step().unwrap(), request);
        vm.respond(1).unwrap();
        assert_eq!(vm.text_cursor, [0, 8, 0]);
        assert_eq!(vm.async_text.as_ref().unwrap().offset, 1);
        assert!(matches!(
            next_text(&mut vm),
            Event::Platform {
                request: PlatformRequest::DrawGlyph {
                    character: 'B',
                    position: [8, 0],
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn other_tasks_change_shared_style_only_between_text_passes() {
        let mut vm = text_vm(b"AB");
        vm.text_style.character_delay = 10;
        let mut worker = 0x84u16.to_le_bytes().to_vec();
        worker.extend(immediate(u32::MAX));
        worker.extend(b"\x10_A_c12,34,56\0");
        worker.extend(0u16.to_le_bytes());
        worker.extend(immediate(0));
        vm.scenarios.insert(
            0x4000_0000,
            Scenario {
                name: "worker.scn".into(),
                data: worker,
            },
        );
        vm.define_task(1, 0x4000_0000, 0, true);
        vm.rescan_tasks();
        let mut clock = 0u32;
        let mut drawn = Vec::new();
        let mut ended = false;
        for _ in 0..100 {
            match vm.scheduled_step().unwrap() {
                Event::End => {
                    ended = true;
                    break;
                }
                Event::SchedulerPoll => vm.respond(1).unwrap(),
                Event::Platform {
                    request: PlatformRequest::ClockMilliseconds,
                    ..
                } => {
                    clock += 10;
                    vm.respond(clock).unwrap();
                }
                request @ Event::Platform {
                    request: PlatformRequest::DrawGlyph { .. },
                    ..
                } => {
                    assert_eq!(vm.current_task(), 0);
                    assert_eq!(vm.scheduled_step().unwrap(), request);
                    if let Event::Platform {
                        request:
                            PlatformRequest::DrawGlyph {
                                character, style, ..
                            },
                        ..
                    } = request
                    {
                        drawn.push((character, style.color));
                    }
                    vm.respond(1).unwrap();
                }
                Event::BinaryInstruction { .. } | Event::TextTick { .. } => {}
                event => panic!("unexpected event: {event:?}"),
            }
        }
        assert!(ended);
        assert_eq!(drawn, [('A', [255; 3]), ('B', [12, 34, 56])]);
    }

    #[test]
    fn coroutine_errors_keep_the_source_offset_and_remain_sticky() {
        for bytes in [b"A_?".as_slice(), b"\x82"] {
            let mut vm = text_vm(bytes);
            let error = loop {
                match vm.scheduled_step() {
                    Err(error) => break error.to_string(),
                    Ok(Event::SchedulerPoll) | Ok(Event::Platform { .. }) => vm.respond(1).unwrap(),
                    event => panic!("unexpected result: {event:?}"),
                }
            };
            assert!(error.contains("opcode=0x0083, task=0, text_byte="));
            assert_eq!(vm.scheduled_step().unwrap_err().to_string(), error);
            assert_eq!(vm.step().unwrap_err().to_string(), error);
            assert!(vm.async_text.is_some());
        }
    }

    #[test]
    fn image_text_executes_and_surface_drawing_resumes() {
        let mut code = Vec::new();
        let immediate = |value: u32| {
            let mut out = vec![4];
            out.extend(value.to_le_bytes());
            out
        };
        for (target, text) in [(17, b"_AA".as_slice()), (u32::MAX, b"B")] {
            code.extend(0xb4u16.to_le_bytes());
            code.extend(immediate(target));
            code.extend(immediate(0));
            code.extend(0x83u16.to_le_bytes());
            code.extend(immediate(0));
            code.push(0x10);
            code.extend(text);
            code.push(0);
        }
        code.extend(0u16.to_le_bytes());
        code.extend(immediate(0));
        let mut vm = BinaryVm::new("image-text.scn", code).unwrap();
        let mut drawn = Vec::new();
        for step in 0..100 {
            assert!(step < 99);
            match vm.scheduled_step().unwrap() {
                Event::End => break,
                Event::SchedulerPoll => vm.respond(1).unwrap(),
                Event::Platform {
                    request:
                        PlatformRequest::DrawGlyph {
                            character,
                            position,
                            ..
                        }
                        | PlatformRequest::DrawImageGlyph {
                            character,
                            position,
                            ..
                        },
                    ..
                } => {
                    drawn.push((character, position));
                    vm.respond(1).unwrap();
                }
                Event::BinaryInstruction { .. } | Event::TextTick { .. } => {}
                event => panic!("{event:?}"),
            }
        }
        assert_eq!(drawn, vec![('A', [0, 0]), ('B', [8, 0])]);
    }
}
