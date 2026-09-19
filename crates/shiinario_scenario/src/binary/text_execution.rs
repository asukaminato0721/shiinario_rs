//! Continuations for the original flag-8 text scheduler path.
use super::*;
use crate::text_layout::TextLayout;

#[derive(Clone, Copy, Debug)]
pub(super) enum TextPhase {
    Begin,
    Input,
    Process,
    CheckDelay,
    Epoch,
    Glyph,
    Drawing,
    StyleClock,
    Finish(bool),
}

pub(super) struct AsyncText {
    pub owner: u32,
    pub surface: u32,
    pub address: u32,
    pub offset: usize,
    pub location: Location,
    pub ticking: bool,
    pub timed: bool,
    pub phase: TextPhase,
    pub glyph: Option<(TextLayout, usize)>,
}
impl AsyncText {
    pub fn new(owner: u32, surface: u32, address: u32, location: Location) -> Self {
        Self { owner, surface, address, offset: 0, location, ticking: false,
            timed: true, phase: TextPhase::Begin, glyph: None }
    }
}

impl BinaryVm {
    pub(super) fn text_response(&mut self, event: &Event, value: u32) -> Result<()> {
        let Some(text) = self.async_text.as_mut().filter(|text| text.ticking) else {
            return Ok(());
        };
        let Event::Platform { request, .. } = event else { return Ok(()); };
        match (text.phase, request) {
            (TextPhase::Input, PlatformRequest::TextInput { clear: false }) => {
                let mask = self.text_style.skip_mask;
                if (mask & 1 != 0 && value & 0x20 != 0)
                    || (mask & 2 != 0 && value & 0x10 != 0)
                    || (mask & 4 != 0 && value & 0x100 != 0)
                    || (mask & 8 != 0 && value & 0x10000 != 0) {
                    text.timed = false;
                }
                text.phase = TextPhase::Process;
            }
            (TextPhase::CheckDelay, PlatformRequest::ClockMilliseconds) => {
                text.phase = if value.wrapping_sub(self.text_style.character_epoch)
                    < self.text_style.character_delay {
                    TextPhase::Finish(false)
                } else { TextPhase::Epoch };
            }
            (TextPhase::Epoch, PlatformRequest::ClockMilliseconds) => {
                self.text_style.character_epoch = value;
                text.phase = TextPhase::Glyph;
            }
            (TextPhase::StyleClock, PlatformRequest::ClockMilliseconds) => {
                // respond() commits pending_text_style with this clock value.
                text.phase = TextPhase::Process;
            }
            (TextPhase::Drawing, PlatformRequest::DrawGlyph { .. }) => {
                let (layout, offset) = text.glyph.take().context("missing pending glyph")?;
                self.text_cursor = layout.cursor;
                self.text_layout = layout;
                text.offset = offset;
                text.phase = if text.timed && self.text_style.character_delay != 0 {
                    TextPhase::Finish(false)
                } else { TextPhase::Process };
            }
            _ => bail!("unexpected text response: {request:?}"),
        }
        Ok(())
    }

    fn text_request(&mut self, request: PlatformRequest) -> Event {
        let event = Event::Platform {
            location: self.async_text.as_ref().unwrap().location.clone(), request,
        };
        self.pending = Some((event.clone(), None));
        event
    }

    pub(super) fn text_step(&mut self) -> Result<Event> {
        match self.text_execute() {
            Ok(event) => Ok(event),
            Err(error) => {
                let text = self.async_text.as_ref().unwrap();
                let message = format!("{}:{:#x}: {error:#}; opcode=0x0083, task={}, text_byte={:#x}",
                    text.location.scenario, text.location.offset, text.owner, text.offset);
                self.failure = Some(message.clone());
                bail!("{message}")
            }
        }
    }

    fn text_execute(&mut self) -> Result<Event> {
        loop {
            let text = self.async_text.as_ref().context("missing text context")?;
            ensure!(text.owner == self.current_task, "text task changed during a pending tick");
            let phase = text.phase;
            let surface = text.surface;
            let offset = text.offset;
            match phase {
                TextPhase::Begin => {
                    self.async_text.as_mut().unwrap().phase = if self.text_style.skip_mask != 0 {
                        TextPhase::Input
                    } else { TextPhase::Process };
                }
                TextPhase::Input => return Ok(self.text_request(PlatformRequest::TextInput { clear: false })),
                TextPhase::CheckDelay | TextPhase::Epoch => {
                    return Ok(self.text_request(PlatformRequest::ClockMilliseconds));
                }
                TextPhase::Finish(completed) => {
                    let event = Event::TextTick { task: text.owner, completed,
                        cursor: [self.text_cursor[1], self.text_cursor[2]] };
                    if completed {
                        self.context_flags &= !8;
                        self.async_text = None;
                    } else {
                        self.async_text.as_mut().unwrap().ticking = false;
                    }
                    self.scheduler.dispatching = false;
                    self.scheduler.pass_live |= self.context_flags & 1 != 0;
                    self.scheduler.scan = self.current_task + 1;
                    return Ok(event);
                }
                TextPhase::Process | TextPhase::Glyph => {
                    ensure!(offset <= 65536, "text exceeds 64 KiB");
                    let address = text.address.checked_add(offset as u32).context("text address overflow")?;
                    let bytes = self.string_bytes(address)?;
                    ensure!(bytes.len() <= 65536 - offset, "text exceeds 64 KiB");
                    if bytes.is_empty() {
                        self.async_text.as_mut().unwrap().phase = TextPhase::Finish(true);
                        continue;
                    }
                    if matches!(phase, TextPhase::Process) {
                        if bytes.starts_with(b"_") && !bytes.starts_with(b"__") {
                            let (style, clock, consumed) = self.text_style.prefix(&bytes)?;
                            self.async_text.as_mut().unwrap().offset += consumed;
                            if let Some(clock) = clock {
                                self.pending_text_style = Some((style, clock));
                                self.async_text.as_mut().unwrap().phase = TextPhase::StyleClock;
                                return Ok(self.text_request(PlatformRequest::ClockMilliseconds));
                            }
                            self.text_style = style;
                            continue;
                        }
                        self.async_text.as_mut().unwrap().phase =
                            if text.timed && self.text_style.character_delay != 0 {
                                TextPhase::CheckDelay
                            } else { TextPhase::Glyph };
                        continue;
                    }
                    let escaped = matches!(bytes[0], b'_' | b'$' | b'*' | b'+' | b'@' | b'{' | b'}' | b'~');
                    let start = usize::from(escaped);
                    if escaped {
                        ensure!(bytes.get(1) == Some(&bytes[0]), "unresolved inline text command {:#x}", bytes[0]);
                    }
                    let length = cp932_len(bytes[start]);
                    ensure!(start + length <= bytes.len(), "truncated CP932 text character");
                    let glyph = &bytes[start..start+length];
                    ensure!(!self.text_style.fullwidth_ascii || length == 2
                        || (glyph == b" " && !self.text_style.fullwidth_spaces),
                        "single-byte text conversion is unresolved");
                    let consumed = start + length;
                    let next = if consumed == bytes.len() { &[] } else {
                        let end = consumed + cp932_len(bytes[consumed]);
                        ensure!(end <= bytes.len(), "truncated following CP932 character");
                        &bytes[consumed..end]
                    };
                    let (decoded, _, bad) = encoding_rs::SHIFT_JIS.decode(glyph);
                    ensure!(!bad, "invalid CP932 text character");
                    let mut chars = decoded.chars();
                    let character = chars.next().context("empty decoded glyph")?;
                    ensure!(chars.next().is_none() && !character.is_control(), "unresolved text character {character:?}");
                    let mut layout = self.text_layout.clone();
                    let position = layout.place(glyph, next,
                        [self.text_style.half_advance,self.text_style.full_advance],
                        self.text_style.line_advance,self.text_style.line_limit,&self.text_style.punctuation)?;
                    if surface == u32::MAX {
                        self.text_cursor = layout.cursor;
                        self.text_layout = layout;
                        self.async_text.as_mut().unwrap().offset += consumed;
                        self.async_text.as_mut().unwrap().phase = TextPhase::Process;
                        continue;
                    }
                    let text = self.async_text.as_mut().unwrap();
                    text.glyph = Some((layout,offset+consumed));
                    text.phase = TextPhase::Drawing;
                    return Ok(self.text_request(PlatformRequest::DrawGlyph {
                        surface,position: position.map(|v| v as i32),character,style: self.text_style.clone(),
                    }));
                }
                TextPhase::Drawing | TextPhase::StyleClock => bail!("text continuation has no platform request"),
            }
        }
    }
}

fn cp932_len(first: u8) -> usize {
    if matches!(first, 0x81..=0x9f | 0xe0..=0xfc) { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(hex: &str) -> Vec<u8> {
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i+2],16).unwrap()).collect()
    }
    #[test]
    fn asynchronous_control_text_matches_original_scheduler_passes() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../docs/validation/text-scheduler-probe.json")).unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let mut vm = BinaryVm::new("text.scn",decode(case["programs"][0].as_str().unwrap())).unwrap();
            vm.set_dispatch_quantum(case["quantum"].as_u64().unwrap() as u32).unwrap();
            vm.scenarios.insert(0x40000000,Scenario { name: "worker.scn".into(),data:decode(case["programs"][1].as_str().unwrap()) });
            vm.define_task(1,0x40000000,0,true);
            vm.thread_limit = 2;
            let mut events = Vec::new();
            loop {
                assert!(events.len()<100);
                let event=vm.scheduled_step().unwrap();
                match event {
                    Event::End => break,
                    Event::SchedulerPoll => {
                        events.push(serde_json::json!({"event":"poll"})); vm.respond(1).unwrap();
                    }
                    Event::TextTick { task,completed,cursor } => {
                        assert!(completed);
                        events.push(serde_json::json!({"event":"text-tick","task":task,"status":1,"cursor":cursor}));
                        events.push(serde_json::json!({"event":"text-status","task":task,"status":0,"cursor":cursor}));
                    }
                    Event::BinaryInstruction { location,.. } | Event::MouseButtonMapping {location,..} => {
                        events.push(serde_json::json!({"event":"instruction","task":vm.current_task,
                            "pc":location.offset,"sp":1000,"local":0,"shared":0}));
                    }
                    other => panic!("{other:?}"),
                }
            }
            assert_eq!(serde_json::json!(events),case["events"],"{}",case["name"]);
        }
    }
}
