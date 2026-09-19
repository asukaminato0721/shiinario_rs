//! Scenario records retain original CP932 byte offsets. Binary SCN and story TXT
//! are distinct formats; unknown instructions are never treated as no-ops.
mod binary;
mod expression;
mod format;
mod memory;
mod text;
pub mod text_layout;
pub use binary::{
    BinaryVm, ImageDraw, MaskTransition, MouseButtonMapping, PlatformRequest, SoundCommand,
    SurfaceBlend, SurfaceCopy, SurfacePoint, boot_binary,
};
pub use memory::SharedMemory;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Location {
    pub scenario: String,
    pub offset: usize,
    pub line: Option<usize>,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum Record {
    Command {
        location: Location,
        name: String,
        args: Vec<String>,
    },
    Dialogue {
        location: Location,
        text: String,
    },
}
impl Record {
    pub fn location(&self) -> &Location {
        match self {
            Self::Command { location, .. } | Self::Dialogue { location, .. } => location,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct TextScript {
    pub name: String,
    pub records: Vec<Record>,
    pub byte_len: usize,
}
impl TextScript {
    pub fn parse(name: impl Into<String>, data: &[u8]) -> Result<Self> {
        let name = name.into();
        ensure!(data.len() <= 16 * 1024 * 1024, "scenario exceeds size cap");
        ensure!(!data.contains(&0), "binary data in text scenario");
        let mut records = Vec::new();
        let mut offset = 0;
        let mut paragraph: Option<(Location, String)> = None;
        fn flush(records: &mut Vec<Record>, paragraph: &mut Option<(Location, String)>) {
            if let Some((location, text)) = paragraph.take() {
                records.push(Record::Dialogue { location, text });
            }
        }
        for (line, raw) in data.split_inclusive(|&b| b == b'\n').enumerate() {
            let content = raw.strip_suffix(b"\n").unwrap_or(raw);
            let content = content.strip_suffix(b"\r").unwrap_or(content);
            let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(content);
            ensure!(!bad, "{}:{offset:#x}: invalid CP932", name);
            let text = text.trim_end();
            let location = Location {
                scenario: name.clone(),
                offset,
                line: Some(line + 1),
            };
            if text.is_empty() || text.starts_with(';') {
                flush(&mut records, &mut paragraph);
            } else if let Some(command) = text.strip_prefix('$') {
                flush(&mut records, &mut paragraph);
                let mut fields = command.split(',');
                let command = fields.next().unwrap();
                ensure!(
                    !command.is_empty()
                        && command.bytes().all(|c| c.is_ascii_uppercase() || c == b'_'),
                    "{}:{offset:#x}: invalid command name",
                    name
                );
                records.push(Record::Command {
                    location,
                    name: command.to_owned(),
                    args: fields.map(str::to_owned).collect(),
                });
            } else {
                match &mut paragraph {
                    Some((_, p)) => {
                        p.push('\n');
                        p.push_str(text);
                    }
                    None => paragraph = Some((location, text.into())),
                }
            }
            offset += raw.len();
        }
        flush(&mut records, &mut paragraph);
        Ok(Self {
            name,
            records,
            byte_len: data.len(),
        })
    }
    pub fn inventory(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for r in &self.records {
            if let Record::Command { name, .. } = r {
                *out.entry(name.clone()).or_default() += 1;
            }
        }
        out
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct StringCandidate {
    pub offset: usize,
    pub tag: u8,
    pub text: String,
}
/// Research aid only: candidate tagged strings are not an instruction decoder.
pub fn binary_strings(data: &[u8]) -> Vec<StringCandidate> {
    let mut strings = Vec::new();
    let mut p = 0;
    while p < data.len() {
        if matches!(data[p], 0x10 | 0x12) {
            let end = data[p + 1..]
                .iter()
                .take(4096)
                .position(|&b| b == 0)
                .map(|n| p + 1 + n);
            if let Some(end) = end {
                let raw = &data[p + 1..end];
                let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(raw);
                if !bad && !raw.is_empty() && text.chars().all(|c| !c.is_control()) {
                    strings.push(StringCandidate {
                        offset: p,
                        tag: data[p],
                        text: text.into_owned(),
                    });
                    p = end;
                }
            }
        }
        p += 1;
    }
    strings
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum AudioChannel {
    Music,
    Effect(u32),
    Voice,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event")]
pub enum Event {
    /// Host poll between scheduler passes or priority-task dispatches.
    SchedulerPoll,
    ArchiveSearchPath {
        location: Location,
        name: String,
    },
    BinaryInstruction {
        location: Location,
        opcode: u16,
    },
    Platform {
        location: Location,
        request: PlatformRequest,
    },
    PlatformReply {
        value: u32,
        simulated: bool,
    },
    SurfaceDigest {
        id: u32,
        bgr_sha256: String,
    },
    ScenarioDigest {
        name: String,
        sha256: String,
    },
    AssetSizesReply {
        sizes: [u32; 2],
    },
    PointReply {
        point: [i32; 2],
        simulated: bool,
    },
    ImageBoundsReply {
        bounds: [i32; 4],
    },
    PlatformBytesReply {
        bytes: Vec<u8>,
        simulated: bool,
    },
    MouseButtonMapping {
        location: Location,
        value: u32,
    },
    Dialogue {
        location: Location,
        text: String,
    },
    Image {
        location: Location,
        layer: u32,
        path: Option<String>,
        frame: u32,
        x: i32,
        y: i32,
    },
    Draw {
        location: Location,
        duration_ms: u64,
        mode: i32,
    },
    Audio {
        location: Location,
        channel: AudioChannel,
        path: Option<String>,
        mode: i32,
        volume: Option<i32>,
    },
    Wait {
        remaining_ms: u64,
    },
    AwaitInput,
    End,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Advance,
    Tick(u64),
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    None,
    Input,
    Time(u64),
}
/// Limited story-text execution for research. EX, EFECT, DRAW_EX and MUSIC_FADE
/// stop until their script-library semantics have been recovered and verified.
pub struct TextVm {
    script: TextScript,
    pc: usize,
    wait: Wait,
    clock_ms: u64,
    stopped: Option<String>,
}
impl TextVm {
    pub fn new(script: TextScript) -> Self {
        Self {
            script,
            pc: 0,
            wait: Wait::None,
            clock_ms: 0,
            stopped: None,
        }
    }
    pub fn location(&self) -> Location {
        self.script
            .records
            .get(self.pc)
            .map(|r| r.location().clone())
            .unwrap_or(Location {
                scenario: self.script.name.clone(),
                offset: self.script.byte_len,
                line: None,
            })
    }
    pub fn input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Advance => {
                if self.wait == Wait::Input {
                    self.wait = Wait::None;
                }
            }
            Input::Tick(ms) => {
                self.clock_ms = self.clock_ms.checked_add(ms).context("clock overflow")?;
                if let Wait::Time(deadline) = self.wait
                    && self.clock_ms >= deadline
                {
                    self.wait = Wait::None;
                }
            }
        }
        Ok(())
    }
    pub fn step(&mut self) -> Result<Event> {
        if let Some(message) = &self.stopped {
            bail!("{message}");
        }
        match self.wait {
            Wait::Input => return Ok(Event::AwaitInput),
            Wait::Time(deadline) => {
                return Ok(Event::Wait {
                    remaining_ms: deadline - self.clock_ms,
                });
            }
            Wait::None => {}
        }
        while let Some(record) = self.script.records.get(self.pc).cloned() {
            let result = self.execute(&record);
            match result {
                Ok(Some(event)) => {
                    self.pc += 1;
                    return Ok(event);
                }
                Ok(None) => {
                    self.pc += 1;
                }
                Err(error) => {
                    let l = record.location();
                    let message = format!(
                        "{}:{:#x} (line {:?}, record {}, clock {} ms): {error:#}",
                        l.scenario, l.offset, l.line, self.pc, self.clock_ms
                    );
                    self.stopped = Some(message.clone());
                    bail!("{message}");
                }
            }
        }
        Ok(Event::End)
    }
    fn execute(&mut self, r: &Record) -> Result<Option<Event>> {
        let Record::Command {
            location,
            name,
            args,
        } = r
        else {
            let Record::Dialogue { location, text } = r else {
                unreachable!()
            };
            self.wait = Wait::Input;
            return Ok(Some(Event::Dialogue {
                location: location.clone(),
                text: text.clone(),
            }));
        };
        let location = location.clone();
        let arg = |i: usize| {
            args.get(i)
                .with_context(|| format!("${name}: missing argument {}", i + 1))
        };
        let num = |i: usize| -> Result<i32> {
            arg(i)?
                .parse()
                .with_context(|| format!("${name}: invalid integer argument {}", i + 1))
        };
        let unsigned = |i: usize| -> Result<u32> {
            u32::try_from(num(i)?).context("negative index or duration")
        };
        let path = |i: usize| -> Result<Option<String>> {
            Ok(if arg(i)?.is_empty() {
                None
            } else {
                Some(arg(i)?.clone())
            })
        };
        Ok(match name.as_str() {
            "LABEL" => {
                ensure!(args.len() == 1, "$LABEL: expected one argument");
                num(0)?;
                None
            }
            "L_BG" => {
                ensure!(args.len() == 2, "$L_BG: expected two arguments");
                Some(Event::Image {
                    location,
                    layer: 0,
                    path: path(0)?,
                    frame: unsigned(1)?,
                    x: 0,
                    y: 0,
                })
            }
            "L_CHR" => {
                ensure!(args.len() == 5, "$L_CHR: expected five arguments");
                Some(Event::Image {
                    location,
                    layer: unsigned(0)?,
                    path: path(1)?,
                    frame: unsigned(4)?,
                    x: num(2)?,
                    y: num(3)?,
                })
            }
            "DRAW" => {
                ensure!(args.len() == 2, "$DRAW: expected two arguments");
                let duration_ms = unsigned(0)? as u64;
                let mode = num(1)?;
                ensure!(matches!(mode, 0 | 1), "unsupported draw mode {mode}");
                if duration_ms > 0 {
                    self.wait = Wait::Time(
                        self.clock_ms
                            .checked_add(duration_ms)
                            .context("clock overflow")?,
                    );
                }
                Some(Event::Draw {
                    location,
                    duration_ms,
                    mode,
                })
            }
            "VOICE" | "MUSIC" => {
                ensure!(
                    args.is_empty() || args.len() == 2,
                    "${name}: expected zero or two arguments"
                );
                Some(Event::Audio {
                    location,
                    channel: if name == "VOICE" {
                        AudioChannel::Voice
                    } else {
                        AudioChannel::Music
                    },
                    path: if args.is_empty() { None } else { path(0)? },
                    mode: if args.is_empty() { 0 } else { num(1)? },
                    volume: None,
                })
            }
            "SE" => {
                ensure!(
                    args.len() == 3 || args.len() == 4,
                    "$SE: expected three or four arguments"
                );
                Some(Event::Audio {
                    location,
                    channel: AudioChannel::Effect(unsigned(2)?),
                    path: path(0)?,
                    mode: num(1)?,
                    volume: if args.len() == 4 { Some(num(3)?) } else { None },
                })
            }
            _ => bail!("unsupported executable command ${name} with arguments {args:?}"),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cp932_offsets_and_paragraphs() {
        let (raw, _, _) =
            encoding_rs::SHIFT_JIS.encode(";comment\r\n日本語\r\n次\r\n\r\n$LABEL,7\r\n");
        let s = TextScript::parse("fixture.txt", &raw).unwrap();
        assert_eq!(s.records.len(), 2);
        assert_eq!(s.records[0].location().offset, 10);
        assert_eq!(s.records[1].location().offset, 24);
        assert!(matches!(&s.records[0],Record::Dialogue{text,..} if text=="日本語\n次"));
    }
    #[test]
    fn deterministic_waits() {
        let s = TextScript::parse("test", b"$DRAW,10,1\nHello\n").unwrap();
        let mut v = TextVm::new(s);
        assert!(matches!(v.step().unwrap(), Event::Draw { .. }));
        v.input(Input::Advance).unwrap();
        assert_eq!(v.step().unwrap(), Event::Wait { remaining_ms: 10 });
        v.input(Input::Tick(9)).unwrap();
        assert_eq!(v.step().unwrap(), Event::Wait { remaining_ms: 1 });
        v.input(Input::Tick(1)).unwrap();
        assert!(matches!(v.step().unwrap(), Event::Dialogue { .. }));
        assert_eq!(v.step().unwrap(), Event::AwaitInput);
        v.input(Input::Advance).unwrap();
        assert_eq!(v.step().unwrap(), Event::End);
    }
    #[test]
    fn unknown_is_sticky() {
        let s = TextScript::parse("test", b"$UNKNOWN,1\nHello\n").unwrap();
        let mut v = TextVm::new(s);
        let first = v.step().unwrap_err().to_string();
        assert!(first.contains("test:0x0"));
        assert_eq!(first, v.step().unwrap_err().to_string());
    }
    #[test]
    fn binary_does_not_guess() {
        assert!(
            boot_binary("start.scn", &[0x9d, 4, 4, 0])
                .unwrap_err()
                .to_string()
                .contains("0x049d")
        );
        assert!(boot_binary("bad", &[0]).is_err());
    }
}
