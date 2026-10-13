//! A coroutine for the original flag-8 text scheduler path.
use super::*;
use crate::text_layout::TextLayout;
use shiinario_core::coroutine::{Co, Coroutine, Resume};
use std::{cell::RefCell, rc::Rc};

type VmOperation = Box<dyn FnOnce(&mut BinaryVm)>;
type PlatformReply = Box<dyn FnOnce(&mut BinaryVm, u32) -> Result<()>>;

enum TextYield {
    WithVm(VmOperation),
    Platform {
        request: Box<PlatformRequest>,
        reply: PlatformReply,
    },
    Tick,
}

pub(super) struct AsyncText {
    pub owner: u32,
    pub surface: u32,
    pub address: u32,
    pub offset: usize,
    pub location: Location,
    pub ticking: bool,
    coroutine: Option<Coroutine<TextYield>>,
    reply: Option<PlatformReply>,
}
impl AsyncText {
    pub fn new(owner: u32, surface: u32, address: u32, location: Location) -> Self {
        Self {
            owner,
            surface,
            address,
            offset: 0,
            location,
            ticking: false,
            coroutine: Some(Coroutine::new(run_text)),
            reply: None,
        }
    }
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
    let mut timed = true;
    loop {
        if co.with_vm(|vm| Ok(vm.text_style.skip_mask != 0)).await? {
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
                request,
            }) = co.with_vm(BinaryVm::text_glyph).await?
            else {
                return Ok(());
            };
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
}

impl BinaryVm {
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
                    "{}:{:#x}: {error:#}; opcode=0x0083, task={}, text_byte={:#x}",
                    text.location.scenario, text.location.offset, text.owner, text.offset
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
        let result = self.resume_text(&mut coroutine);
        if let Some(text) = &mut self.async_text {
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
                Resume::Yielded(TextYield::Tick) => return Ok(self.text_tick(false)),
                Resume::Complete => return Ok(self.text_tick(true)),
            }
        }
    }

    fn text_tick(&mut self, completed: bool) -> Event {
        let text = self.async_text.as_mut().unwrap();
        let event = Event::TextTick {
            task: text.owner,
            completed,
            cursor: [self.text_cursor[1], self.text_cursor[2]],
        };
        if completed {
            self.context_flags &= !8;
            self.async_text = None;
        } else {
            text.ticking = false;
        }
        self.scheduler.dispatching = false;
        self.scheduler.pass_live |= self.context_flags & 1 != 0;
        self.scheduler.scan = self.current_task + 1;
        event
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

    fn text_item(&mut self) -> Result<TextItem> {
        let bytes = self.text_bytes()?;
        if bytes.is_empty() {
            return Ok(TextItem::End);
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
        let length = cp932_len(bytes[start]);
        ensure!(
            start + length <= bytes.len(),
            "truncated CP932 text character"
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
            let end = consumed + cp932_len(bytes[consumed]);
            ensure!(end <= bytes.len(), "truncated following CP932 character");
            &bytes[consumed..end]
        };
        let (decoded, _, bad) = encoding_rs::SHIFT_JIS.decode(glyph);
        ensure!(!bad, "invalid CP932 text character");
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
        let request = if self.text_image[0] != u32::MAX {
            Some(PlatformRequest::DrawImageGlyph {
                image: self.text_image[0],
                frame: self.text_image[1],
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
        };
        Ok(Some(TextGlyph {
            layout,
            offset: offset + consumed,
            request,
        }))
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

fn cp932_len(first: u8) -> usize {
    if matches!(first, 0x81..=0x9f | 0xe0..=0xfc) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        vm.async_text = Some(AsyncText::new(0, 0, address, vm.location()));
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
