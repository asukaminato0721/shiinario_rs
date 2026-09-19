//! Verified subset of the supplied v2.47 engine. See docs/SCN_RESEARCH.md.
use crate::{Event, Location, SharedMemory};
use anyhow::{Context, Result, bail, ensure};
mod text_execution;
use text_execution::{AsyncText, TextPhase};

/// The original engine stores the full operand and treats every nonzero value as
/// swapped. Input bits 0, 1 and 2 represent the first three physical mouse buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseButtonMapping {
    pub value: u32,
}
impl MouseButtonMapping {
    pub fn map_buttons(self, physical: u8) -> u8 {
        let buttons = physical & 7;
        if self.value == 0 {
            buttons
        } else {
            ((buttons & 1) << 1) | ((buttons & 2) >> 1) | (buttons & 4)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SurfacePoint {
    pub id: u32,
    pub x: i32,
    pub y: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SurfaceBlend {
    pub destination: SurfacePoint,
    pub sources: [SurfacePoint; 2],
    pub size: [i32; 2],
    pub weights: [u32; 2],
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SurfaceCopy {
    pub destination: SurfacePoint,
    pub source: SurfacePoint,
    pub size: [i32; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SurfaceCapture {
    pub image: u32,
    pub frame: u32,
    pub destination: [i32; 2],
    pub size: [i32; 2],
    pub source: SurfacePoint,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SurfaceStretch {
    pub destination: SurfacePoint,
    pub destination_size: [i32; 2],
    pub source: SurfacePoint,
    pub source_size: [i32; 2],
    pub mode: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MaskTransition {
    pub destination: u32,
    pub first: u32,
    pub second: Option<u32>,
    pub mask: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImageDraw {
    pub image: u32,
    pub frame: u32,
    pub flags: u32,
    pub layer: u32,
    pub x: i32,
    pub y: i32,
    pub extra: [u32; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum SoundCommand {
    Play { flags: u32 },
    Stop,
    Volume { attenuation: i32 },
    Pan { attenuation: i32 },
    Frequency { hz: u32 },
    Status,
}

/// Requests are explicit so a headless trace cannot invent operating-system results.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PlatformRequest {
    /// Controls plus bit 16 for the WM_CHAR-style latch. A clear only resets
    /// that latch; it does not consume physical button/key state.
    TextInput {
        clear: bool,
    },
    DrawGlyph {
        surface: u32,
        position: [i32; 2],
        character: char,
        style: crate::TextStyle,
    },
    Sound {
        id: u32,
        command: SoundCommand,
    },
    ReadKeyState {
        key: u32,
    },
    ReadControls,
    CursorPosition,
    MapCursor {
        point: [i32; 2],
    },
    HitTestImages {
        x: i32,
        y: i32,
        items: Vec<ImageDraw>,
    },
    ImageBounds {
        id: u32,
        frame: u32,
    },
    DrawImages {
        id: u32,
        items: Vec<ImageDraw>,
    },
    /// Logical left/top/right/bottom; request repaint without clearing.
    InvalidateRect {
        rect: [i32; 4],
    },
    BlendSurfaces(SurfaceBlend),
    CopySurface(SurfaceCopy),
    StretchSurface(SurfaceStretch),
    CaptureSurface(SurfaceCapture),
    MaskTransition(MaskTransition),
    SurfacePixels {
        id: u32,
    },
    FillSurface {
        id: u32,
        rect: [i32; 4],
        color: [u8; 3],
    },
    SetAudioStreamVolume {
        handle: u32,
        percent: u32,
    },
    GetAudioStreamVolume {
        handle: u32,
    },
    FinishAudioFade {
        handle: u32,
        interval: u32,
        step: i32,
        target: u32,
    },
    StopAudioStream {
        handle: u32,
    },
    ReleaseAudioStream {
        handle: u32,
    },
    PlayAudioStream {
        handle: u32,
        flags: u32,
    },
    CreateAudioStream {
        address: u32,
        flags: u32,
    },
    AssetSizes {
        name: String,
    },
    ReadAssetInto {
        name: String,
        address: u32,
    },
    LoadAsset {
        name: String,
    },
    LoadScenario {
        id: u32,
        name: String,
        activate: bool,
    },
    InitializeAudio {
        flags: u32,
    },
    InitializeGraphics,
    ReleaseGraphics,
    SetFullscreen {
        enabled: bool,
    },
    CreateSurface {
        id: u32,
        width: u32,
        height: u32,
        flags: u32,
    },
    ReleaseSurface {
        id: u32,
    },
    CpuFeatures,
    ReleaseImage {
        id: u32,
    },
    LoadImage {
        id: u32,
        name: String,
    },
    LoadSound {
        id: u32,
        name: String,
        flags: u32,
    },
    CreateImage {
        id: u32,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        frames: u32,
    },
    FillImage {
        id: u32,
        frame: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        color: u32,
    },
    /// Low 32 bits of the host monotonic clock in milliseconds.
    ClockMilliseconds,
    /// Hold the current task until wrapping clock minus epoch reaches duration.
    /// A false reply keeps this request pending, without scheduling another task.
    WaitTaskTimer {
        epoch: u32,
        duration: u32,
    },
    ReadIniInteger {
        file: String,
        section: String,
        key: String,
        default: u32,
    },
    FileExists {
        path: String,
    },
    ReadRegistryValue {
        root: u32,
        path: String,
        name: String,
    },
    ReadRegistryString {
        root: u32,
        path: String,
        name: String,
    },
    ProjectDirectory,
    PumpMessages,
    DisableIme,
    DeviceCaps {
        device: u32,
        index: i32,
    },
    GetClassLong {
        window: u32,
        index: i32,
    },
    SetClassLong {
        window: u32,
        index: i32,
        value: u32,
    },
    FindWindow {
        class: String,
        title: String,
    },
    SetWindowTitle {
        window: u32,
        title: String,
    },
}

const CELLS: usize = 1000;
const SCRIPT_BASE: u32 = 0x1000_0000;

#[derive(Debug, Clone, Copy)]
enum Destination {
    Discard,
    Bank(u8, usize),
    Memory(u32, usize),
}

enum MemoryRange {
    Shared(u32, std::ops::Range<usize>),
    Script(std::ops::Range<usize>),
    Scenario(u32, std::ops::Range<usize>),
    Bank(u8, std::ops::Range<usize>),
    TaskBank(u32, u8, std::ops::Range<usize>),
    Heap(u32, std::ops::Range<usize>),
}

fn bank_base(tag: u8) -> u32 {
    0x1100_0000 + u32::from(tag) * 0x10000
}

fn task_bank_base(task: u32, tag: u8) -> u32 {
    if task == 0 || !matches!(tag, 8 | 12) {
        bank_base(tag)
    } else {
        0x8000_0000 + task * 0x2000 + if tag == 12 { 0x1000 } else { 0 }
    }
}

struct TaskState {
    base: u32,
    pc: usize,
    sp: usize,
    flags: u32,
    stack: Vec<u32>,
    locals: Vec<u32>,
    scopes: Vec<NamedScope>,
    parameters: Vec<u32>,
}
impl TaskState {
    fn new(base: u32, pc: usize) -> Self {
        Self {
            base,
            pc,
            sp: CELLS,
            flags: 0,
            stack: vec![0; CELLS],
            locals: vec![0; CELLS],
            scopes: Vec::new(),
            parameters: Vec::new(),
        }
    }
    fn bank(&self, tag: u8) -> &[u32] {
        if tag == 8 { &self.stack } else { &self.locals }
    }
    fn bank_mut(&mut self, tag: u8) -> &mut [u32] {
        if tag == 8 {
            &mut self.stack
        } else {
            &mut self.locals
        }
    }
}

#[derive(Default)]
struct Scheduler {
    started: bool,
    scan: u32,
    pass_live: bool,
    dispatching: bool,
    used: u32,
    yielded: bool,
}

struct NamedScope {
    allocation: u32,
    names: Vec<Vec<u8>>,
}
struct Scenario {
    name: String,
    data: Vec<u8>,
}

struct Cursor<'a> {
    data: &'a [u8],
    pc: usize,
}
impl Cursor<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.pc.checked_add(N).context("operand offset overflow")?;
        let bytes = self
            .data
            .get(self.pc..end)
            .context("truncated SCN operand")?;
        self.pc = end;
        Ok(bytes.try_into()?)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }
    fn word(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes()?))
    }
    fn dword(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    fn variable_name(&mut self) -> Result<Vec<u8>> {
        let mut name = Vec::new();
        loop {
            match self.byte()? {
                0 | b'}' => break,
                b' ' | b'\t' => continue,
                b'[' => bail!("array expression in named variable is unresolved"),
                b => name.push(b),
            }
            ensure!(name.len() <= 31, "named variable exceeds 31 bytes");
        }
        ensure!(!name.is_empty(), "empty named variable");
        Ok(name)
    }
    fn declaration_name(&mut self) -> Result<Vec<u8>> {
        let mut name = Vec::new();
        loop {
            match self.byte()? {
                0 => break,
                b'[' => bail!("array declaration is unresolved"),
                byte => name.push(byte),
            }
            ensure!(name.len() <= 31, "named variable exceeds 31 bytes");
        }
        ensure!(!name.is_empty(), "empty named variable");
        Ok(name)
    }
}

pub struct BinaryVm {
    name: String,
    data: Vec<u8>,
    script_base: u32,
    scenarios: std::collections::BTreeMap<u32, Scenario>,
    next_scenario_base: u32,
    pc: usize,
    steps: usize,
    mouse_mapping: MouseButtonMapping,
    failure: Option<String>,
    // Banks 8 and 12 belong to the selected task; all other banks are shared.
    banks: std::collections::BTreeMap<u8, Vec<u32>>,
    sp: usize,
    thread_limit: u32,
    thread_limit_override: u32,
    draw_list: Vec<ImageDraw>,
    file_read_marker: u32,
    file_write_marker: u32,
    input_latches: [u32; 2],
    save_encoding: u32,
    media_flags: u32,
    viewport: [u32; 2],
    pending: Option<(Event, Option<Destination>)>,
    allocations: std::collections::BTreeMap<u32, Vec<u8>>,
    shared_regions: std::collections::BTreeMap<u32, SharedMemory>,
    surface_regions: std::collections::BTreeMap<u32, u32>,
    next_shared: u32,
    // Entry points are separate from the task's resumable program counter.
    task_entries: std::collections::BTreeMap<u32, (u32, usize)>,
    current_task: u32,
    tasks: std::collections::BTreeMap<u32, TaskState>,
    scheduler: Scheduler,
    dispatch_quantum: u32,
    callback_tasks: std::collections::BTreeMap<u16, u32>,
    ended: bool,
    named_scopes: Vec<NamedScope>,
    parameters: Vec<u32>,
    registry_root: u32,
    registry_path: String,
    ini_path: String,
    pending_bytes: Option<u32>,
    pending_bounds: Option<[Destination; 4]>,
    pending_point: Option<[Destination; 2]>,
    pending_sizes: Option<[Destination; 2]>,
    // Some(true) resets this task's epoch; Some(false) reads elapsed time.
    pending_timer: Option<bool>,
    task_timers: std::collections::BTreeMap<u32, u32>,
    context_flags: u32,
    message_mode: u32,
    background_mode: u32,
    archive_paths: Vec<String>,
    text_style: crate::text::TextStyle,
    best_effort: bool,
    text_image: [u32; 2],
    text_image_offset: [u32; 2],
    /// Line-start X, current X, current Y in the default text context.
    text_cursor: [u32; 3],
    text_layout: crate::text_layout::TextLayout,
    async_text: Option<AsyncText>,
    pending_text_style: Option<(crate::text::TextStyle, crate::text::TextClock)>,
}
impl BinaryVm {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Result<Self> {
        ensure!(data.len() <= 16 * 1024 * 1024, "scenario exceeds size cap");
        Ok(Self {
            name: name.into(),
            data,
            script_base: SCRIPT_BASE,
            scenarios: Default::default(),
            next_scenario_base: 0x4000_0000,
            pc: 0,
            steps: 0,
            mouse_mapping: MouseButtonMapping::default(),
            failure: None,
            banks: [2, 6, 8, 10, 12, 14]
                .into_iter()
                .map(|tag| (tag, vec![0; CELLS]))
                .collect(),
            sp: CELLS,
            thread_limit: 1,
            thread_limit_override: 0,
            draw_list: Vec::new(),
            file_read_marker: 0,
            file_write_marker: 0,
            input_latches: [0; 2],
            save_encoding: 1,
            media_flags: 0,
            viewport: [800, 600],
            pending: None,
            allocations: Default::default(),
            shared_regions: Default::default(),
            surface_regions: Default::default(),
            next_shared: 0x9000_0000,
            task_entries: Default::default(),
            current_task: 0,
            tasks: Default::default(),
            scheduler: Scheduler::default(),
            dispatch_quantum: 1,
            callback_tasks: Default::default(),
            ended: false,
            named_scopes: Vec::new(),
            parameters: Vec::new(),
            registry_root: 0,
            registry_path: String::new(),
            ini_path: String::new(),
            pending_bytes: None,
            pending_bounds: None,
            pending_point: None,
            pending_sizes: None,
            pending_timer: None,
            task_timers: Default::default(),
            text_style: Default::default(),
            best_effort: false,
            text_image: [u32::MAX, 0],
            text_image_offset: [0; 2],
            text_cursor: [0; 3],
            text_layout: crate::text_layout::TextLayout::new([0; 3]),
            async_text: None,
            pending_text_style: None,
            context_flags: 1,
            message_mode: 0,
            background_mode: 0,
            archive_paths: Vec::new(),
        })
    }
    pub fn current_task(&self) -> u32 {
        self.current_task
    }
    /// Permit only explicitly identified presentation omissions. Unknown operand
    /// boundaries and control-flow operations still fail.
    pub fn set_best_effort(&mut self, enabled: bool) {
        self.best_effort = enabled;
    }
    /// Number of instructions per native dispatcher invocation (INI Turbo).
    pub fn set_dispatch_quantum(&mut self, value: u32) -> Result<()> {
        ensure!(value > 0, "dispatcher quantum must be positive");
        self.dispatch_quantum = value;
        Ok(())
    }
    fn task_flags(&self, id: u32) -> u32 {
        if id == self.current_task {
            self.context_flags
        } else {
            self.tasks.get(&id).map_or(0, |t| t.flags)
        }
    }
    fn rescan_tasks(&mut self) {
        if self.thread_limit_override == 0 {
            self.thread_limit = (0..CELLS as u32)
                .rev()
                .find(|&id| self.task_flags(id) & 1 != 0)
                .unwrap_or(0)
                + 1;
        }
    }
    fn define_task(&mut self, id: u32, base: u32, pc: usize, activate: bool) {
        let task = self
            .tasks
            .entry(id)
            .or_insert_with(|| TaskState::new(base, pc));
        task.base = base;
        task.pc = pc;
        task.sp = CELLS;
        task.flags = u32::from(activate);
        task.parameters.clear();
        self.task_entries.insert(id, (base, pc));
    }
    fn select_task(&mut self, id: u32) {
        if id == self.current_task {
            return;
        }
        let task = self.tasks.remove(&id).expect("defined runnable task");
        let old = TaskState {
            base: self.script_base,
            pc: self.pc,
            sp: self.sp,
            flags: self.context_flags,
            stack: std::mem::replace(self.banks.get_mut(&8).unwrap(), task.stack),
            locals: std::mem::replace(self.banks.get_mut(&12).unwrap(), task.locals),
            scopes: std::mem::replace(&mut self.named_scopes, task.scopes),
            parameters: std::mem::replace(&mut self.parameters, task.parameters),
        };
        self.tasks.insert(self.current_task, old);
        self.switch_scenario(task.base);
        self.pc = task.pc;
        self.sp = task.sp;
        self.context_flags = task.flags;
        self.current_task = id;
    }
    fn scheduler_poll(&mut self) -> Event {
        let event = Event::SchedulerPoll;
        self.pending = Some((event.clone(), None));
        event
    }
    /// Advance the original cooperative scheduler, including explicit host polls.
    /// Outstanding platform requests must be answered before another task runs.
    pub fn scheduled_step(&mut self) -> Result<Event> {
        if self.failure.is_some() || self.ended || self.pending.is_some() {
            return self.step();
        }
        if self.async_text.as_ref().is_some_and(|text| text.ticking) {
            return self.text_step();
        }
        if !self.scheduler.started {
            self.scheduler.started = true;
            return Ok(self.scheduler_poll());
        }
        if self.scheduler.dispatching && self.scheduler.yielded {
            self.scheduler.dispatching = false;
            if self.context_flags & 4 != 0 {
                return Ok(self.scheduler_poll());
            }
            self.scheduler.pass_live |= self.context_flags & 1 != 0;
            self.scheduler.scan = self.current_task + 1;
        }
        while !self.scheduler.dispatching {
            if self.scheduler.scan >= self.thread_limit {
                if !self.scheduler.pass_live {
                    self.ended = true;
                    return Ok(Event::End);
                }
                self.scheduler.scan = 0;
                self.scheduler.pass_live = false;
                return Ok(self.scheduler_poll());
            }
            let flags = self.task_flags(self.scheduler.scan);
            ensure!(
                flags & 2 == 0,
                "task {}: unsupported timer/transition flags {flags:#x}",
                self.scheduler.scan
            );
            if flags & 1 == 0 {
                self.scheduler.scan += 1;
                continue;
            }
            self.select_task(self.scheduler.scan);
            if flags & 8 != 0 {
                let text = self
                    .async_text
                    .as_mut()
                    .context("text task has no text context")?;
                ensure!(
                    text.owner == self.current_task,
                    "shared concurrent text contexts are unresolved"
                );
                text.ticking = true;
                text.phase = TextPhase::Begin;
                return self.text_step();
            }
            self.scheduler.dispatching = true;
            self.scheduler.used = 0;
            self.scheduler.yielded = false;
        }
        let event = self.step()?;
        self.scheduler.used += 1;
        self.scheduler.yielded = self.scheduler.used >= self.dispatch_quantum
            || self.message_mode != 0
            || self.context_flags == 0
            || self.context_flags & 8 != 0
            || matches!(
                &event,
                Event::Platform {
                    request: PlatformRequest::PumpMessages,
                    ..
                }
            );
        Ok(event)
    }
    pub fn location(&self) -> Location {
        Location {
            scenario: self.name.clone(),
            offset: self.pc,
            line: None,
        }
    }
    /// Read-only scenario memory for inspection after self-modifying scripts.
    pub fn scenario_buffers(&self) -> impl Iterator<Item = (&str, &[u8])> {
        std::iter::once((self.name.as_str(), self.data.as_slice())).chain(
            self.scenarios
                .values()
                .map(|s| (s.name.as_str(), s.data.as_slice())),
        )
    }
    pub fn mouse_mapping(&self) -> MouseButtonMapping {
        self.mouse_mapping
    }
    pub fn set_background_mode(&mut self, value: u32) {
        self.background_mode = value;
    }
    pub fn set_viewport(&mut self, width: u32, height: u32) -> Result<()> {
        ensure!(width > 0 && height > 0, "empty viewport");
        self.viewport = [width, height];
        Ok(())
    }
    /// Complete the outstanding platform request. Repeated step calls before a
    /// response return the same request, without executing the next instruction.
    pub fn respond(&mut self, value: u32) -> Result<()> {
        ensure!(
            self.pending_sizes.is_none(),
            "platform request requires asset sizes"
        );
        ensure!(
            self.pending_point.is_none(),
            "platform request requires a point reply"
        );
        ensure!(
            self.pending_bounds.is_none(),
            "platform request requires image bounds"
        );
        ensure!(
            self.pending_bytes.is_none(),
            "platform request requires a byte-string reply"
        );
        if let Some((Event::Platform { location, request }, _)) = &self.pending {
            if matches!(request, PlatformRequest::WaitTaskTimer { .. }) {
                ensure!(value <= 1, "timer wait requires a Boolean reply");
                if value == 0 {
                    return Ok(());
                }
            }
            ensure!(
                !matches!(
                    request,
                    PlatformRequest::LoadScenario { .. }
                        | PlatformRequest::LoadAsset { .. }
                        | PlatformRequest::ReadAssetInto { .. }
                        | PlatformRequest::SurfacePixels { .. }
                ),
                "asset/scenario loading requires a byte-buffer reply"
            );
            if matches!(
                request,
                PlatformRequest::CreateAudioStream { .. }
                    | PlatformRequest::DrawGlyph { .. }
                    | PlatformRequest::CreateSurface { .. }
                    | PlatformRequest::ReleaseSurface { .. }
                    | PlatformRequest::LoadImage { .. }
                    | PlatformRequest::CreateImage { .. }
                    | PlatformRequest::FillImage { .. }
                    | PlatformRequest::LoadSound { .. }
            ) {
                ensure!(
                    value != 0,
                    "{}:{:#x}: resource operation failed: {request:?}",
                    location.scenario,
                    location.offset
                );
            }
            if matches!(
                request,
                PlatformRequest::InitializeAudio { .. } | PlatformRequest::InitializeGraphics
            ) {
                ensure!(value <= 1, "device initialization requires a Boolean reply");
            }
        }
        let (event, destination) = self.pending.take().context("no pending platform request")?;
        self.text_response(&event, value)?;
        if let Some((mut style, clock)) = self.pending_text_style.take() {
            match clock {
                crate::text::TextClock::Character => style.character_epoch = value,
                crate::text::TextClock::Wait => style.wait_epoch = value,
            }
            self.text_style = style;
        }
        let value = match self.pending_timer.take() {
            Some(true) => {
                self.task_timers.insert(self.current_task, value);
                value
            }
            Some(false) => {
                value.wrapping_sub(*self.task_timers.get(&self.current_task).unwrap_or(&0))
            }
            None => value,
        };
        if let Event::Platform {
            request: PlatformRequest::CreateAudioStream { address, .. },
            ..
        } = &event
        {
            // 453010 changes OGV's compressed length field after using it to
            // find loop metadata. The host reads the original bytes first.
            let range = self.memory_range(address + 8, 4)?;
            self.memory_write(range, b"WAVE");
        }
        if matches!(
            &event,
            Event::Platform {
                request: PlatformRequest::LoadSound { .. },
                ..
            }
        ) && self.background_mode != 0
        {
            self.media_flags |= 0x8000;
        }
        if matches!(
            event,
            Event::SchedulerPoll
                | Event::Platform {
                    request: PlatformRequest::PumpMessages,
                    ..
                }
        ) && value == 0
        {
            self.ended = true;
        }
        if let Some(destination) = destination {
            self.write(destination, value);
        }
        if let Event::Platform {
            request:
                PlatformRequest::CreateSurface { id, .. } | PlatformRequest::ReleaseSurface { id },
            ..
        } = event
            && let Some(base) = self.surface_regions.remove(&id)
        {
            self.shared_regions.remove(&base);
        }
        Ok(())
    }
    /// Bind the actual host pixel storage, preserving pointer identity on
    /// repeated queries. Host drawing and VM writes observe the same bytes.
    pub fn respond_surface_pixels(&mut self, memory: Option<SharedMemory>) -> Result<u32> {
        let Some((
            Event::Platform {
                request: PlatformRequest::SurfacePixels { id },
                ..
            },
            destination,
        )) = &self.pending
        else {
            bail!("no pending surface memory request");
        };
        let id = *id;
        let destination = destination.context("surface pointer requires a destination")?;
        let old = self.surface_regions.get(&id).copied();
        let address = if let Some(memory) = memory {
            if let Some(base) = old.filter(|base| self.shared_regions[base].same_region(&memory)) {
                base
            } else {
                let retained: usize = self
                    .shared_regions
                    .iter()
                    .filter(|(base, _)| Some(**base) != old)
                    .map(|(_, m)| m.len())
                    .sum();
                ensure!(
                    retained + memory.len() <= 256 * 1024 * 1024,
                    "mapped resources exceed 256 MiB"
                );
                let base = self.next_shared;
                let next = base
                    .checked_add(((memory.len() as u32 + 4095) & !4095) + 4096)
                    .context("shared memory address overflow")?;
                ensure!(next < 0xf000_0000, "shared memory address space exhausted");
                self.shared_regions.insert(base, memory);
                self.next_shared = next;
                self.surface_regions.insert(id, base);

                base
            }
        } else {
            self.surface_regions.remove(&id);
            0
        };
        // Write while the previous region still exists: the script may have
        // placed the destination inside the region being replaced.
        self.write(destination, address);
        if let Some(old) = old.filter(|old| *old != address) {
            self.shared_regions.remove(&old);
        }
        self.pending = None;
        Ok(address)
    }
    /// Read a whole VM allocation for a pending resource operation.
    pub fn allocation_bytes(&self, address: u32) -> Result<&[u8]> {
        self.allocations
            .get(&address)
            .map(Vec::as_slice)
            .context("resource address is not an allocation base")
    }
    /// First-fit placement among live allocations. Each has a trailing guard
    /// page; released local scopes must not consume virtual addresses forever.
    fn allocation_address(&self, size: usize) -> Result<u32> {
        ensure!(
            size > 0 && size <= 16 * 1024 * 1024,
            "allocation size outside 1..16 MiB"
        );
        let span = |len: usize| ((len as u32 + 4095) & !4095) + 4096;
        let needed = span(size);
        let mut address = 0x2000_0000u32;
        for (&base, bytes) in &self.allocations {
            if address.checked_add(needed).is_some_and(|end| end <= base) {
                break;
            }
            address = base
                .checked_add(span(bytes.len()))
                .context("allocation address overflow")?;
        }
        ensure!(
            address
                .checked_add(needed)
                .is_some_and(|end| end < 0x3000_0000),
            "VM allocation address space exhausted"
        );
        Ok(address)
    }
    /// Install a decoded archive entry and return its VM address to the script.
    pub fn respond_asset(&mut self, data: Vec<u8>) -> Result<u32> {
        let Some((
            Event::Platform {
                request: PlatformRequest::LoadAsset { .. },
                ..
            },
            Some(destination),
        )) = self.pending.as_ref()
        else {
            bail!("no pending asset load");
        };
        let destination = *destination;
        ensure!(
            !data.is_empty() && data.len() <= 16 * 1024 * 1024,
            "asset allocation outside 1..16 MiB"
        );
        let address = self.allocation_address(data.len())?;
        self.allocations.insert(address, data);
        self.write(destination, address);
        self.pending = None;
        Ok(address)
    }
    /// Install scenario bytes; 0001 also activates the target task, 0002 does not.
    pub fn respond_scenario(&mut self, data: Vec<u8>) -> Result<()> {
        let Some((
            Event::Platform {
                request: PlatformRequest::LoadScenario { id, name, activate },
                ..
            },
            _,
        )) = &self.pending
        else {
            bail!("no pending scenario load");
        };
        ensure!(
            !data.is_empty() && data.len() <= 16 * 1024 * 1024,
            "invalid scenario size"
        );
        let total = self.data.len()
            + self.scenarios.values().map(|s| s.data.len()).sum::<usize>()
            + data.len();
        ensure!(
            total <= 256 * 1024 * 1024,
            "scenario memory exceeds 256 MiB"
        );
        let base = self.next_scenario_base;
        let next = base
            .checked_add((data.len() as u32 + 4095) & !4095)
            .filter(|&address| address <= 0x7000_0000)
            .context("scenario address space exhausted")?;
        self.scenarios.insert(
            base,
            Scenario {
                name: name.clone(),
                data,
            },
        );
        let (id, activate) = (*id, *activate);
        self.task_entries.insert(id, (base, 0));
        if id == self.current_task {
            self.switch_scenario(base);
            self.pc = 0;
            self.sp = CELLS;
            if activate {
                self.parameters.clear();
                self.context_flags = 1;
            }
        } else {
            let task = self
                .tasks
                .entry(id)
                .or_insert_with(|| TaskState::new(base, 0));
            task.base = base;
            task.pc = 0;
            task.sp = CELLS;
            if activate {
                task.parameters.clear();
                task.flags = 1;
            }
        }
        if activate {
            self.rescan_tasks();
        }
        self.next_scenario_base = next;
        self.pending = None;
        Ok(())
    }
    fn scenario_size(&self, base: u32) -> Result<usize> {
        if base == self.script_base {
            Ok(self.data.len())
        } else {
            Ok(self
                .scenarios
                .get(&base)
                .context("unknown scenario base")?
                .data
                .len())
        }
    }
    fn switch_scenario(&mut self, base: u32) {
        if base == self.script_base {
            return;
        }
        let scenario = self
            .scenarios
            .remove(&base)
            .expect("validated scenario base");
        let old = Scenario {
            name: std::mem::replace(&mut self.name, scenario.name),
            data: std::mem::replace(&mut self.data, scenario.data),
        };
        self.scenarios.insert(self.script_base, old);
        self.script_base = base;
    }
    pub fn respond_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let address = self
            .pending_bytes
            .context("no pending byte-string request")?;
        ensure!(
            bytes.len() < 256 && !bytes.contains(&0),
            "platform string exceeds 255 bytes or contains NUL"
        );
        let range = self.memory_range(address, bytes.len() + 1)?;
        let mut terminated = bytes.to_vec();
        terminated.push(0);
        self.memory_write(range, &terminated);
        self.pending_bytes = None;
        self.pending = None;
        Ok(())
    }
    /// Complete a frame metadata query with signed left/top/right/bottom edges.
    pub fn respond_image_bounds(&mut self, bounds: [i32; 4]) -> Result<()> {
        let destinations = self
            .pending_bounds
            .take()
            .context("no pending image bounds query")?;
        for (destination, value) in destinations.into_iter().zip(bounds) {
            self.write(destination, value as u32);
        }
        self.pending = None;
        Ok(())
    }
    pub fn respond_asset_sizes(&mut self, sizes: [u32; 2]) -> Result<()> {
        let destinations = self
            .pending_sizes
            .take()
            .context("no pending asset sizes query")?;
        for (destination, value) in destinations.into_iter().zip(sizes) {
            self.write(destination, value);
        }
        self.pending = None;
        Ok(())
    }
    pub fn respond_asset_into(&mut self, bytes: &[u8]) -> Result<()> {
        let Some((
            Event::Platform {
                request: PlatformRequest::ReadAssetInto { address, .. },
                ..
            },
            _,
        )) = &self.pending
        else {
            bail!("no pending asset read into memory");
        };
        let range = self.memory_range(*address, bytes.len())?;
        self.memory_write(range, bytes);
        self.pending = None;
        Ok(())
    }
    pub fn respond_point(&mut self, point: [i32; 2]) -> Result<()> {
        let destinations = self
            .pending_point
            .take()
            .context("no pending point query")?;
        for (destination, value) in destinations.into_iter().zip(point) {
            self.write(destination, value as u32);
        }
        self.pending = None;
        Ok(())
    }
    fn destination(&self, cursor: &mut Cursor<'_>) -> Result<Destination> {
        let tag = cursor.byte()?;
        match tag & 0x7f {
            4 => {
                cursor.dword()?;
                Ok(Destination::Discard)
            }
            bank @ (2 | 6 | 8 | 10 | 12 | 14) => {
                let index = usize::from(cursor.word()?);
                let index = if bank == 8 {
                    self.sp.checked_add(index).context("stack index overflow")?
                } else {
                    index
                };
                ensure!(
                    index < CELLS,
                    "operand bank {bank:#x} index {index} out of bounds"
                );
                Ok(Destination::Bank(bank, index))
            }
            3 | 5 | 7 | 9 | 11 | 13 | 15 => {
                let (address, width) = self.indirect_address(tag, cursor)?;
                self.memory_range(address, width)?;
                Ok(Destination::Memory(address, width))
            }
            0x12 | 0x13 => {
                let name = cursor.variable_name()?;
                let mut address = self.named_address(&name)?;
                if tag & 0x7f == 0x13 {
                    address = u32::from_le_bytes(self.memory_read(address, 4)?.try_into().unwrap());
                    if tag & 0x80 != 0 {
                        address = address.wrapping_add(self.script_base);
                    }
                }
                self.memory_range(address, 4)?;
                Ok(Destination::Memory(address, 4))
            }
            _ => bail!("unsupported destination tag {tag:#04x}"),
        }
    }
    fn read(&self, cursor: &mut Cursor<'_>) -> Result<u32> {
        let start = cursor.pc;
        let tag = cursor.byte()?;
        if tag & 0x40 != 0 {
            return self.operand_address(tag, cursor);
        }
        self.read_value(tag, start, cursor)
    }
    fn operand_address(&self, tag: u8, cursor: &mut Cursor<'_>) -> Result<u32> {
        match tag & 0x3f {
            4 => Ok(self.script_base.wrapping_add(cursor.dword()?)),
            bank @ (2 | 6 | 8 | 10 | 12 | 14) => {
                let index = usize::from(cursor.word()?) + if bank == 8 { self.sp } else { 0 };
                // Taking the address of the empty stack's end is valid;
                // dereferencing it still requires an in-bounds memory range.
                ensure!(index <= CELLS, "bank address index {index} out of bounds");
                Ok(task_bank_base(self.current_task, bank)
                    + (index * if bank == 6 { 1 } else { 4 }) as u32)
            }
            0x12 => self.named_address(&cursor.variable_name()?),
            _ => bail!("unsupported address operand tag {tag:#04x}"),
        }
    }
    fn read_value(&self, tag: u8, start: usize, cursor: &mut Cursor<'_>) -> Result<u32> {
        let value = match tag & 0x7f {
            4 => cursor.dword()?,
            bank @ (2 | 6 | 8 | 10 | 12 | 14) => {
                let index = usize::from(cursor.word()?);
                let index = if bank == 8 {
                    self.sp.checked_add(index).context("stack index overflow")?
                } else {
                    index
                };
                *self.banks[&bank].get(index).with_context(|| {
                    format!("operand bank {bank:#x} index {index} out of bounds")
                })?
            }
            0x11 if tag == 0x11 => {
                let length = cursor.data[cursor.pc..]
                    .iter()
                    .position(|&b| b == 0)
                    .context("unterminated inline expression")?;
                let bytes = &cursor.data[cursor.pc..cursor.pc + length];
                let value = crate::expression::evaluate(bytes, |variable| {
                    self.expression_variable(variable)
                })?;
                cursor.pc += length + 1;
                return Ok(value);
            }
            0x10 if tag == 0x10 => {
                let address = self.script_base + cursor.pc as u32;
                let size = cursor.data[cursor.pc..]
                    .iter()
                    .position(|&b| b == 0)
                    .context("unterminated inline string")?;
                cursor.pc += size + 1;
                return Ok(address);
            }
            3 | 5 | 7 | 9 | 11 | 13 | 15 => {
                let (address, width) = self.indirect_address(tag, cursor)?;
                let bytes = self.memory_read(address, width)?;
                return Ok(if width == 1 {
                    u32::from(bytes[0])
                } else {
                    u32::from_le_bytes(bytes.try_into().unwrap())
                });
            }
            0x12 | 0x13 => {
                let address = self.named_address(&cursor.variable_name()?)?;
                let value = u32::from_le_bytes(self.memory_read(address, 4)?.try_into().unwrap());
                if tag & 0x7f == 0x13 {
                    let address =
                        value.wrapping_add(if tag & 0x80 != 0 { self.script_base } else { 0 });
                    return Ok(u32::from_le_bytes(
                        self.memory_read(address, 4)?.try_into().unwrap(),
                    ));
                }
                value
            }
            _ => bail!("unsupported operand tag {tag:#04x} at {start:#x}"),
        };
        Ok(if tag & 0x80 != 0 {
            value.wrapping_add(self.script_base)
        } else {
            value
        })
    }
    fn string(&self, value: u32) -> Result<String> {
        let bytes = self.string_bytes(value)?;
        let (text, _, bad) = encoding_rs::SHIFT_JIS.decode(&bytes);
        ensure!(!bad, "invalid CP932 string at {value:#x}");
        Ok(text.into_owned())
    }
    fn string_bytes(&self, address: u32) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        for index in 0..65536u32 {
            let address = address
                .checked_add(index)
                .context("string address overflow")?;
            let byte = self.memory_read(address, 1)?[0];
            if byte == 0 {
                return Ok(bytes);
            }
            bytes.push(byte);
        }
        bail!("unterminated string within 64 KiB cap")
    }
    fn expression_variable(&self, variable: crate::expression::Variable<'_>) -> Result<u32> {
        match variable {
            crate::expression::Variable::Named(name) => {
                let address = self.named_address(name)?;
                Ok(u32::from_le_bytes(
                    self.memory_read(address, 4)?.as_slice().try_into()?,
                ))
            }
            crate::expression::Variable::Bank { bank, index } => {
                Ok(self.banks[&bank][self.bank_index(bank, index)?])
            }
        }
    }
    fn named_address(&self, name: &[u8]) -> Result<u32> {
        for scope in self.named_scopes.iter().rev() {
            if let Some(index) = scope.names.iter().position(|n| n == name) {
                return Ok(scope.allocation + (index as u32 * 40) + 32);
            }
        }
        bail!(
            "undefined named variable {:?}",
            String::from_utf8_lossy(name)
        )
    }
    fn write(&mut self, destination: Destination, value: u32) {
        match destination {
            Destination::Bank(bank, index) => {
                self.banks.get_mut(&bank).unwrap()[index] =
                    if bank == 6 { value & 1 } else { value }
            }
            Destination::Memory(address, width) => {
                // Range validation happens when decoding the destination. No
                // allocations can be freed while a platform reply is pending.
                let range = self.memory_range(address, width).unwrap();
                let value = if width == 1 { value & 1 } else { value };
                self.memory_write(range, &value.to_le_bytes()[..width]);
            }
            Destination::Discard => {}
        }
    }
    fn bank_index(&self, bank: u8, index: u16) -> Result<usize> {
        let index = usize::from(index) + if bank == 8 { self.sp } else { 0 };
        ensure!(
            index < CELLS,
            "operand bank {bank:#x} index {index} out of bounds"
        );
        Ok(index)
    }
    fn indirect_address(&self, tag: u8, cursor: &mut Cursor<'_>) -> Result<(u32, usize)> {
        let base = tag & 0x7f;
        let address = if base == 5 {
            cursor.dword()?
        } else {
            let bank = base - 1;
            let index = self.bank_index(bank, cursor.word()?)?;
            self.banks[&bank][index]
        };
        Ok((
            address.wrapping_add(if tag & 0x80 != 0 { self.script_base } else { 0 }),
            if base == 7 { 1 } else { 4 },
        ))
    }
    fn memory_range(&self, address: u32, len: usize) -> Result<MemoryRange> {
        ensure!(
            len <= 16 * 1024 * 1024,
            "memory operation exceeds 16 MiB cap"
        );
        let range = |base: u32, size: usize| {
            let start = address.checked_sub(base)? as usize;
            let end = start.checked_add(len)?;
            (end <= size).then_some(start..end)
        };
        if let Some(r) = range(self.script_base, self.data.len()) {
            return Ok(MemoryRange::Script(r));
        }
        if let Some((&base, scenario)) = self.scenarios.range(..=address).next_back()
            && let Some(r) = range(base, scenario.data.len())
        {
            return Ok(MemoryRange::Scenario(base, r));
        }
        for &bank in self.banks.keys() {
            if let Some(r) = range(
                task_bank_base(self.current_task, bank),
                CELLS * if bank == 6 { 1 } else { 4 },
            ) {
                return Ok(MemoryRange::Bank(bank, r));
            }
        }
        for &id in self.tasks.keys() {
            for tag in [8, 12] {
                if let Some(r) = range(task_bank_base(id, tag), CELLS * 4) {
                    return Ok(MemoryRange::TaskBank(id, tag, r));
                }
            }
        }
        if let Some((&base, data)) = self.allocations.range(..=address).next_back()
            && let Some(r) = range(base, data.len())
        {
            return Ok(MemoryRange::Heap(base, r));
        }
        if let Some((&base, memory)) = self.shared_regions.range(..=address).next_back()
            && let Some(r) = range(base, memory.len())
        {
            return Ok(MemoryRange::Shared(base, r));
        }
        bail!("invalid memory range {address:#x} + {len:#x}")
    }
    fn memory_read(&self, address: u32, len: usize) -> Result<Vec<u8>> {
        Ok(match self.memory_range(address, len)? {
            MemoryRange::Shared(base, r) => self.shared_regions[&base].read(r.start, r.len())?,
            MemoryRange::Script(r) => self.data[r].to_vec(),
            MemoryRange::Scenario(base, r) => self.scenarios[&base].data[r].to_vec(),
            MemoryRange::Heap(base, r) => self.allocations[&base][r].to_vec(),
            MemoryRange::TaskBank(id, tag, r) => r
                .map(|index| self.tasks[&id].bank(tag)[index / 4].to_le_bytes()[index % 4])
                .collect(),
            MemoryRange::Bank(bank, r) => r
                .map(|index| {
                    if bank == 6 {
                        self.banks[&bank][index] as u8
                    } else {
                        self.banks[&bank][index / 4].to_le_bytes()[index % 4]
                    }
                })
                .collect(),
        })
    }
    fn memory_write(&mut self, range: MemoryRange, bytes: &[u8]) {
        match range {
            MemoryRange::Shared(base, r) => self.shared_regions[&base]
                .write(r.start, bytes)
                .expect("validated fixed-size resource range"),
            MemoryRange::Script(r) => self.data[r].copy_from_slice(bytes),
            MemoryRange::Scenario(base, r) => {
                self.scenarios.get_mut(&base).unwrap().data[r].copy_from_slice(bytes)
            }
            MemoryRange::Heap(base, r) => {
                self.allocations.get_mut(&base).unwrap()[r].copy_from_slice(bytes)
            }
            MemoryRange::TaskBank(id, tag, r) => {
                let cells = self.tasks.get_mut(&id).unwrap().bank_mut(tag);
                for (index, &value) in r.zip(bytes) {
                    let mut cell = cells[index / 4].to_le_bytes();
                    cell[index % 4] = value;
                    cells[index / 4] = u32::from_le_bytes(cell);
                }
            }
            MemoryRange::Bank(bank, r) => {
                let cells = self.banks.get_mut(&bank).unwrap();
                for (index, &value) in r.zip(bytes) {
                    if bank == 6 {
                        cells[index] = u32::from(value);
                    } else {
                        let mut cell = cells[index / 4].to_le_bytes();
                        cell[index % 4] = value;
                        cells[index / 4] = u32::from_le_bytes(cell);
                    }
                }
            }
        }
    }
    pub fn step(&mut self) -> Result<Event> {
        if let Some(error) = &self.failure {
            bail!("{error}");
        }
        if self.ended {
            return Ok(Event::End);
        }
        if let Some((event, _)) = &self.pending {
            return Ok(event.clone());
        }
        if self.context_flags & 1 == 0 {
            return Ok(Event::End);
        }
        ensure!(
            self.context_flags & 8 == 0,
            "asynchronous text requires scheduled_step"
        );
        match self.execute() {
            Ok(event) => Ok(event),
            Err(error) => {
                let opcode = self
                    .data
                    .get(self.pc..self.pc + 2)
                    .map(|b| format!("0x{:04x}", u16::from_le_bytes([b[0], b[1]])))
                    .unwrap_or_else(|| "truncated".into());
                let message = format!(
                    "{}:{:#x}: {error:#}; opcode={opcode}, task={}, executed_steps={}, stack_pointer={}, thread_limit={}, mouse_button_mapping={:#x}, next_bytes={:02x?}",
                    self.name,
                    self.pc,
                    self.current_task,
                    self.steps,
                    self.sp,
                    self.thread_limit,
                    self.mouse_mapping.value,
                    &self.data[self.pc..self.data.len().min(self.pc + 24)]
                );
                self.failure = Some(message.clone());
                bail!("{message}");
            }
        }
    }
    fn execute(&mut self) -> Result<Event> {
        let location = self.location();
        let mut cursor = Cursor {
            data: &self.data,
            pc: self.pc,
        };
        let opcode = cursor.word().context("truncated SCN opcode")?;
        let mut writes = Vec::new();
        let mut request = None;
        let mut response_destination = None;
        let mut memory_write = None;
        let mut byte_destination = None;
        let mut bounds_destinations = None;
        let mut point_destinations = None;
        let mut size_destinations = None;
        let mut timer_reply = None;
        let mut next_base = self.script_base;
        let mut definition = None;
        let mut rescan = false;
        let mut event = Event::BinaryInstruction {
            location: location.clone(),
            opcode,
        };
        match opcode {
            0x06a7..=0x06ab | 0x06af => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "unsupported sound slot/handle {id:#x}");
                let command = match opcode {
                    0x06a7 => SoundCommand::Play {
                        flags: self.read(&mut cursor)?,
                    },
                    0x06a8 => SoundCommand::Stop,
                    0x06a9 => SoundCommand::Volume {
                        attenuation: self.read(&mut cursor)? as i32,
                    },
                    0x06aa => SoundCommand::Pan {
                        attenuation: self.read(&mut cursor)? as i32,
                    },
                    0x06ab => SoundCommand::Frequency {
                        hz: self.read(&mut cursor)?,
                    },
                    _ => {
                        response_destination = Some(self.destination(&mut cursor)?);
                        SoundCommand::Status
                    }
                };
                request = Some(PlatformRequest::Sound { id, command });
            }
            0x03e8 => {
                let key = self.read(&mut cursor)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::ReadKeyState { key });
            }
            0x0456 | 0x0492 => {
                let begin = cursor.pc;
                request = Some(if opcode == 0x0492 {
                    let point = [
                        self.read(&mut cursor)? as i32,
                        self.read(&mut cursor)? as i32,
                    ];
                    cursor.pc = begin;
                    PlatformRequest::MapCursor { point }
                } else {
                    PlatformRequest::CursorPosition
                });
                point_destinations = Some([
                    self.destination(&mut cursor)?,
                    self.destination(&mut cursor)?,
                ]);
            }
            0x03e9 => {
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::ReadControls);
            }
            0x07e5 => self.background_mode = 1,
            0x07e6 => self.background_mode = 0,
            0x07e8 => writes.push((self.destination(&mut cursor)?, self.background_mode)),
            0x0000 => {
                let result = self.read(&mut cursor)?;
                self.context_flags = 0;
                rescan = true;
                if result != 0 {
                    self.ended = true;
                }
            }
            0x0001 | 0x0002 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < CELLS as u32, "unsupported scenario task slot {id}");
                let name = self.string(self.read(&mut cursor)?)?;
                request = Some(PlatformRequest::LoadScenario {
                    id,
                    name,
                    activate: opcode == 1,
                });
            }
            0x07d0 => {
                let rect = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                request = Some(PlatformRequest::InvalidateRect { rect });
            }
            0x0568 => {
                let destination = self.read(&mut cursor)?;
                let first = self.read(&mut cursor)?;
                let second = self.read(&mut cursor)?;
                let mask = self.read(&mut cursor)?;
                let flags = self.read(&mut cursor)?;
                ensure!(
                    destination < 256
                        && first < 256
                        && mask < 256
                        && (second < 256 || second == u32::MAX),
                    "mask transition surface slot out of bounds"
                );
                request = Some(PlatformRequest::MaskTransition(MaskTransition {
                    destination,
                    first,
                    second: (second != u32::MAX).then_some(second),
                    mask,
                    flags,
                }));
            }
            0x055e => {
                let image = self.read(&mut cursor)?;
                let frame = self.read(&mut cursor)?;
                let destination = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let size = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let source = SurfacePoint {
                    id: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                };
                request = Some(PlatformRequest::CaptureSurface(SurfaceCapture {
                    image,
                    frame,
                    destination,
                    size,
                    source,
                }));
            }
            0x051e => {
                let destination = SurfacePoint {
                    id: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                };
                let destination_size = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let source = SurfacePoint {
                    id: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                };
                let source_size = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let mode = self.read(&mut cursor)?;
                request = Some(PlatformRequest::StretchSurface(SurfaceStretch {
                    destination,
                    destination_size,
                    source,
                    source_size,
                    mode,
                }));
            }
            0x04e2 => {
                let destination = SurfacePoint {
                    id: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                };
                let size = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let source = SurfacePoint {
                    id: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                };
                ensure!(
                    destination.id < 256 && source.id < 256,
                    "copy surface slot out of bounds"
                );
                request = Some(PlatformRequest::CopySurface(SurfaceCopy {
                    destination,
                    source,
                    size,
                }));
            }
            0x04f6 => {
                let mut point = || -> Result<SurfacePoint> {
                    let id = self.read(&mut cursor)?;
                    ensure!(id < 256, "unsupported blend surface index/flags {id:#x}");
                    Ok(SurfacePoint {
                        id,
                        x: self.read(&mut cursor)? as i32,
                        y: self.read(&mut cursor)? as i32,
                    })
                };
                let destination = point()?;
                let sources = [point()?, point()?];
                let size = [
                    self.read(&mut cursor)? as i32,
                    self.read(&mut cursor)? as i32,
                ];
                let weights = [self.read(&mut cursor)?, self.read(&mut cursor)?];
                request = Some(PlatformRequest::BlendSurfaces(SurfaceBlend {
                    destination,
                    sources,
                    size,
                    weights,
                }));
            }
            0x0528 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "drawing surface slot out of bounds: {id}");
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::SurfacePixels { id });
            }
            0x04d8 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "drawing surface slot out of bounds: {id}");
                let mut rect = [0; 4];
                for value in &mut rect {
                    *value = self.read(&mut cursor)? as i32;
                }
                let mut color = [0; 3];
                for value in &mut color {
                    *value = self.read(&mut cursor)? as u8;
                }
                request = Some(PlatformRequest::FillSurface { id, rect, color });
            }
            0x06e3 => {
                let handle = self.read(&mut cursor)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::GetAudioStreamVolume { handle });
            }
            0x06ec => {
                let handle = self.read(&mut cursor)?;
                let interval = self.read(&mut cursor)?;
                let step = self.read(&mut cursor)? as i32;
                let target = self.read(&mut cursor)?;
                ensure!(target & 0x7fffffff <= 100, "audio fade target exceeds 100");
                ensure!(
                    self.best_effort,
                    "timed audio fading is unresolved; use best-effort playback"
                );
                request = Some(PlatformRequest::FinishAudioFade {
                    handle,
                    interval,
                    step,
                    target,
                });
            }
            0x06df => {
                let handle = self.read(&mut cursor)?;
                let percent = self.read(&mut cursor)?;
                ensure!(
                    percent <= 100,
                    "audio volume percentage out of bounds: {percent}"
                );
                request = Some(PlatformRequest::SetAudioStreamVolume { handle, percent });
            }
            0x06d8 | 0x06da => {
                let handle = self.read(&mut cursor)?;
                request = Some(if opcode == 0x06d8 {
                    PlatformRequest::ReleaseAudioStream { handle }
                } else {
                    PlatformRequest::StopAudioStream { handle }
                });
            }
            0x06d9 => {
                let handle = self.read(&mut cursor)?;
                let flags =
                    self.read(&mut cursor)? | if self.media_flags & 8 != 0 { 0x10 } else { 0 };
                request = Some(PlatformRequest::PlayAudioStream { handle, flags });
            }
            0x06d6 => {
                let address = self.read(&mut cursor)?;
                let flags =
                    self.read(&mut cursor)? | 0x20 | if self.background_mode != 0 { 8 } else { 0 };
                ensure!(
                    flags == 0x21 || flags == 0x29,
                    "unsupported audio stream flags {flags:#x}"
                );
                let data = self.allocation_bytes(address)?;
                ensure!(
                    data.len() >= 12 && data.starts_with(b"OGV\0"),
                    "audio stream requires a loaded OGV allocation"
                );
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::CreateAudioStream { address, flags });
            }
            0x06a6 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "sound slot {id} out of bounds");
                let name = self.string(self.read(&mut cursor)?)?;
                request = Some(PlatformRequest::LoadSound {
                    id,
                    name,
                    flags: self.media_flags | if self.background_mode != 0 { 0x8000 } else { 0 },
                });
            }
            0x055a => {
                request = Some(PlatformRequest::CreateImage {
                    id: self.read(&mut cursor)?,
                    width: self.read(&mut cursor)?,
                    height: self.read(&mut cursor)?,
                    bytes_per_pixel: self.read(&mut cursor)?,
                    frames: self.read(&mut cursor)?,
                });
            }
            0x055b => {
                request = Some(PlatformRequest::FillImage {
                    id: self.read(&mut cursor)?,
                    frame: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)?,
                    y: self.read(&mut cursor)?,
                    width: self.read(&mut cursor)?,
                    height: self.read(&mut cursor)?,
                    color: self.read(&mut cursor)?,
                });
            }
            0x04c7 => {
                let x = self.read(&mut cursor)? as i32;
                let y = self.read(&mut cursor)? as i32;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::HitTestImages {
                    x,
                    y,
                    items: self.draw_list.clone(),
                });
            }
            0x04c8 => {
                let id = self.read(&mut cursor)?;
                let frame = self.read(&mut cursor)?;
                ensure!(id < 256, "image slot {id} out of bounds");
                bounds_destinations = Some([
                    self.destination(&mut cursor)?,
                    self.destination(&mut cursor)?,
                    self.destination(&mut cursor)?,
                    self.destination(&mut cursor)?,
                ]);
                request = Some(PlatformRequest::ImageBounds { id, frame });
            }
            0x04ba => self.draw_list.clear(),
            0x04bd => {
                ensure!(
                    self.draw_list.len() < 1024,
                    "draw list exceeds 1024 entries"
                );
                let item = ImageDraw {
                    image: self.read(&mut cursor)?,
                    frame: self.read(&mut cursor)?,
                    flags: self.read(&mut cursor)?,
                    layer: self.read(&mut cursor)?,
                    x: self.read(&mut cursor)? as i32,
                    y: self.read(&mut cursor)? as i32,
                    extra: [self.read(&mut cursor)?, self.read(&mut cursor)?],
                };
                self.draw_list.push(item);
            }
            0x04c4 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "draw surface index out of bounds");
                request = Some(PlatformRequest::DrawImages {
                    id,
                    items: self.draw_list.clone(),
                });
            }
            0x04b1 => {
                let id = self.read(&mut cursor)?;
                ensure!(id <= 256, "image release slot {id} out of bounds");
                request = Some(PlatformRequest::ReleaseImage { id });
            }
            0x04b0 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "image slot {id} out of bounds");
                let name = self.string(self.read(&mut cursor)?)?;
                request = Some(PlatformRequest::LoadImage { id, name });
            }
            0x0158 => {
                let name = self.string(self.read(&mut cursor)?)?;
                size_destinations = Some([
                    self.destination(&mut cursor)?,
                    self.destination(&mut cursor)?,
                ]);
                request = Some(PlatformRequest::AssetSizes { name });
            }
            0x00c8 => {
                let name = self.string(self.read(&mut cursor)?)?;
                let address = self.read(&mut cursor)?;
                self.memory_range(address, 0)?;
                request = Some(PlatformRequest::ReadAssetInto { name, address });
            }
            0x00c9 => {
                let name = self.string(self.read(&mut cursor)?)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::LoadAsset { name });
            }
            0x00dd => {
                ensure!(
                    self.archive_paths.len() < 256,
                    "archive search list exceeds 256 entries"
                );
                let address = self.read(&mut cursor)?;
                let mut bytes = self.string_bytes(address)?;
                ensure!(
                    bytes.is_ascii(),
                    "non-ASCII archive path case mapping is unresolved"
                );
                ensure!(bytes.len() < 260, "archive path exceeds engine buffer");
                bytes.make_ascii_lowercase();
                let name = String::from_utf8(bytes.clone())?;
                bytes.push(0);
                memory_write = Some((self.memory_range(address, bytes.len())?, bytes));
                if !self.archive_paths.contains(&name) {
                    self.archive_paths.push(name.clone());
                }
                event = Event::ArchiveSearchPath {
                    location: location.clone(),
                    name,
                };
            }
            0x0032 => self.message_mode = 0,
            0x0a5c => {
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::CpuFeatures);
            }
            0x0033 => self.message_mode = 1,
            0x001e => self.context_flags |= 4,
            0x001f => self.context_flags &= !4,
            0x02ee => {
                let selector = self.read(&mut cursor)?;
                ensure!(self.sp > 0, "stack overflow saving execution mode");
                let value = match selector {
                    0 => self.context_flags & 4,
                    1 => self.message_mode,
                    _ => 0,
                };
                self.sp -= 1;
                writes.push((Destination::Bank(8, self.sp), value));
            }
            0x02ef => {
                let selector = self.read(&mut cursor)?;
                ensure!(self.sp < CELLS, "stack underflow restoring execution mode");
                let value = self.banks[&8][self.sp];
                self.sp += 1;
                match selector {
                    0 => self.context_flags |= value,
                    1 => self.message_mode = value,
                    _ => {}
                }
            }
            0x0776 => {
                request = Some(PlatformRequest::SetFullscreen {
                    enabled: self.read(&mut cursor)? != 0,
                })
            }
            0x06c3 => request = Some(PlatformRequest::ReleaseGraphics),
            0x0546 => {
                let operand = self.read(&mut cursor)?;
                let (id, flags) = if operand & 0x8000_0000 != 0 {
                    (operand & 0x3fff_ffff, operand & 0xc000_0000)
                } else {
                    (operand, 0)
                };
                ensure!(id < 256, "surface index {id} out of bounds");
                request = Some(PlatformRequest::CreateSurface {
                    id,
                    width: self.viewport[0],
                    height: self.viewport[1],
                    flags,
                });
            }
            0x06a4 | 0x06c2 => {
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(if opcode == 0x06a4 {
                    PlatformRequest::InitializeAudio {
                        flags: self.media_flags,
                    }
                } else {
                    PlatformRequest::InitializeGraphics
                });
            }
            0x02db => {
                let mut arguments = Vec::new();
                while cursor.data.get(cursor.pc) != Some(&0xff) {
                    ensure!(arguments.len() < 256, "format exceeds 256 operands");
                    arguments.push(self.read(&mut cursor)?);
                }
                cursor.byte()?;
                ensure!(
                    arguments.len() >= 2,
                    "format needs destination and format string"
                );
                let format = self.string_bytes(arguments[1])?;
                let mut bytes = crate::format::integer_format(&format, &arguments[2..])?;
                bytes.push(0);
                memory_write = Some((self.memory_range(arguments[0], bytes.len())?, bytes));
            }
            0x02da => {
                let destination = self.read(&mut cursor)?;
                let format_address = self.read(&mut cursor)?;
                let format = self.string_bytes(format_address)?;
                let arguments = self.read(&mut cursor)?;
                let mut argument_bytes = 0;
                let mut bytes = crate::format::integer_format_with(&format, |index| {
                    argument_bytes = (index + 1) * 4;
                    let address = arguments
                        .checked_add(index as u32 * 4)
                        .context("format argument address overflow")?;
                    Ok(u32::from_le_bytes(
                        self.memory_read(address, 4)?.as_slice().try_into()?,
                    ))
                })?;
                bytes.push(0);
                for (source, length) in [
                    (format_address, format.len() + 1),
                    (arguments, argument_bytes),
                ] {
                    ensure!(
                        length == 0
                            || u64::from(destination) + bytes.len() as u64 <= u64::from(source)
                            || u64::from(source) + length as u64 <= u64::from(destination),
                        "overlapping format buffers are unresolved"
                    );
                }
                memory_write = Some((self.memory_range(destination, bytes.len())?, bytes));
            }
            0x0104 => {
                let address = self.read(&mut cursor)?;
                ensure!(
                    self.string_bytes(address)?.len() < 260,
                    "INI path exceeds engine buffer"
                );
                self.ini_path = self.string(address)?;
            }
            0x0107 => {
                let section = self.string(self.read(&mut cursor)?)?;
                let key = self.string(self.read(&mut cursor)?)?;
                let start = cursor.pc;
                let default = self.read(&mut cursor)?;
                cursor.pc = start;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::ReadIniInteger {
                    file: self.ini_path.clone(),
                    section,
                    key,
                    default,
                });
            }
            0x03bb..=0x03bd => {
                if opcode != 0x03bb {
                    response_destination = Some(self.destination(&mut cursor)?);
                }
                timer_reply = match opcode {
                    0x03bb => Some(true),
                    0x03bc => Some(false),
                    _ => None,
                };
                request = Some(PlatformRequest::ClockMilliseconds);
            }
            0x03be => {
                let duration = self.read(&mut cursor)?;
                let epoch = *self.task_timers.get(&self.current_task).unwrap_or(&0);
                request = Some(PlatformRequest::WaitTaskTimer { epoch, duration });
            }
            0x00b4 | 0x00b6 => {
                let values = [self.read(&mut cursor)?, self.read(&mut cursor)?];
                if opcode == 0x00b4 {
                    ensure!(
                        values[0] == u32::MAX || self.best_effort,
                        "text drawing into image frames is unresolved; use best-effort playback to omit it"
                    );
                    self.text_image = values;
                    if values[0] != u32::MAX {
                        event = Event::CompatibilitySkip {
                            location: location.clone(),
                            opcode,
                            detail: format!(
                                "text pixels in image {} frame {} omitted; text layout still executes",
                                values[0], values[1]
                            ),
                        };
                    }
                } else {
                    self.text_image_offset = values;
                }
            }
            0x00b5 | 0x00b7 => {
                let values = if opcode == 0x00b5 {
                    self.text_image
                } else {
                    self.text_image_offset
                };
                for value in values {
                    writes.push((self.destination(&mut cursor)?, value));
                }
            }
            0x0078 | 0x007a => {
                let x = self.read(&mut cursor)?;
                let y = self.read(&mut cursor)?;
                let start_x = if opcode == 0x007a {
                    self.read(&mut cursor)?
                } else {
                    x
                };
                self.text_cursor = [start_x, x, y];
            }
            0x0079 => {
                let x = self.destination(&mut cursor)?;
                let y = self.destination(&mut cursor)?;
                writes.push((x, self.text_cursor[1]));
                writes.push((y, self.text_cursor[2]));
            }
            0x0083 => {
                let surface = self.read(&mut cursor)?;
                let address = self.read(&mut cursor)?;
                ensure!(
                    surface < 256 || surface == u32::MAX,
                    "text surface out of bounds"
                );
                ensure!(
                    self.async_text.is_none(),
                    "shared concurrent text contexts are unresolved"
                );
                ensure!(
                    self.string_bytes(address)?.len() <= 65536,
                    "text exceeds 64 KiB"
                );
                self.text_layout.begin(self.text_cursor);
                self.async_text = Some(AsyncText::new(
                    self.current_task,
                    surface,
                    address,
                    location.clone(),
                ));
                self.context_flags |= 8;
                if self.text_style.skip_mask & 8 != 0 {
                    request = Some(PlatformRequest::TextInput { clear: true });
                }
            }
            0x0084 => {
                let surface = self.read(&mut cursor)?;
                ensure!(
                    surface == u32::MAX,
                    "text drawing to surfaces is unresolved"
                );
                let controls = self.string_bytes(self.read(&mut cursor)?)?;
                let (style, clock_read) = self.text_style.configure(&controls)?;
                if let Some(clock) = clock_read {
                    self.pending_text_style = Some((style, clock));
                    request = Some(PlatformRequest::ClockMilliseconds);
                } else {
                    self.text_style = style;
                }
            }
            0x0547 => {
                let id = self.read(&mut cursor)?;
                ensure!(id < 256, "surface release slot out of bounds");
                request = Some(PlatformRequest::ReleaseSurface { id });
            }
            0x010e => {
                let path = self.string(self.read(&mut cursor)?)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::FileExists { path });
            }
            0x00fd => {
                let name = self.string(self.read(&mut cursor)?)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::ReadRegistryValue {
                    root: self.registry_root,
                    path: self.registry_path.clone(),
                    name,
                });
            }
            0x02c6 => {
                let destination = self.read(&mut cursor)?;
                let source = self.read(&mut cursor)?;
                let length = self.read(&mut cursor)? as usize;
                let range = self.memory_range(destination, length)?;
                memory_write = Some((range, self.memory_read(source, length)?));
            }
            0x00fe => {
                let name = self.string(self.read(&mut cursor)?)?;
                let address = self.read(&mut cursor)?;
                self.memory_range(address, 256)?;
                byte_destination = Some(address);
                request = Some(PlatformRequest::ReadRegistryString {
                    root: self.registry_root,
                    path: self.registry_path.clone(),
                    name,
                });
            }
            0x0a28 => {
                let address = self.read(&mut cursor)?;
                self.memory_range(address, 256)?;
                byte_destination = Some(address);
                request = Some(PlatformRequest::ProjectDirectory);
            }
            0x0303 | 0x0309 => {
                let base = self.read(&mut cursor)?;
                let offset = if opcode == 0x0309 {
                    self.read(&mut cursor)?
                } else {
                    0
                };
                let value = self.read(&mut cursor)?;
                memory_write = Some((
                    self.memory_range(base.wrapping_add(offset), 4)?,
                    value.to_le_bytes().to_vec(),
                ));
            }
            0x0302 | 0x0308 => {
                let base = self.read(&mut cursor)?;
                let offset = if opcode == 0x0308 {
                    self.read(&mut cursor)?
                } else {
                    0
                };
                let value = u32::from_le_bytes(
                    self.memory_read(base.wrapping_add(offset), 4)?
                        .as_slice()
                        .try_into()?,
                );
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x0304 | 0x030a | 0x030c => {
                let address = self.read(&mut cursor)?;
                let offset = if opcode == 0x0304 {
                    0
                } else {
                    self.read(&mut cursor)?
                };
                let width = if opcode == 0x030c { 2 } else { 1 };
                let bytes = self.memory_read(address.wrapping_add(offset), width)?;
                let value = if width == 1 {
                    u32::from(bytes[0])
                } else {
                    u32::from(u16::from_le_bytes(bytes.try_into().unwrap()))
                };
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x0305 | 0x030b | 0x030d => {
                let address = self.read(&mut cursor)?;
                let offset = if opcode == 0x0305 {
                    0
                } else {
                    self.read(&mut cursor)?
                };
                let width = if opcode == 0x030d { 2 } else { 1 };
                let value = self.read(&mut cursor)?;
                memory_write = Some((
                    self.memory_range(address.wrapping_add(offset), width)?,
                    value.to_le_bytes()[..width].to_vec(),
                ));
            }
            0x03de => {
                let expression = self.string_bytes(self.read(&mut cursor)?)?;
                let value = crate::expression::evaluate_real(&expression, |variable| {
                    self.expression_variable(variable)
                })?;
                let mode = cursor.byte()?;
                let value = if mode == 0 {
                    value as i64 as u32
                } else {
                    (value as f32).to_bits()
                };
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x02d0 => {
                let length = self.string_bytes(self.read(&mut cursor)?)?.len() as u32;
                writes.push((self.destination(&mut cursor)?, length));
            }
            0x02d1 | 0x02d3 => {
                let mut destination = self.read(&mut cursor)?;
                let source = self.read(&mut cursor)?;
                let mut bytes = self.string_bytes(source)?;
                bytes.push(0);
                if opcode == 0x02d3 {
                    destination = destination
                        .checked_add(self.string_bytes(destination)?.len() as u32)
                        .context("string destination overflow")?;
                }
                memory_write = Some((self.memory_range(destination, bytes.len())?, bytes));
            }
            0x0208 => {
                // The marker evaluates its operand but does not retain it.
                self.read(&mut cursor)?;
            }
            0x0209 => {
                let source = cursor.dword()? as usize;
                let mut selector = Cursor {
                    data: cursor.data,
                    pc: source,
                };
                let value = self.read(&mut selector)?;
                let matched_target = cursor.dword()? as usize;
                let mut entries = 0;
                let target = loop {
                    if cursor.data.get(cursor.pc) == Some(&0xff) {
                        cursor.byte()?;
                        break cursor.dword()? as usize;
                    }
                    ensure!(entries < 4096, "case list exceeds 4096 operands");
                    entries += 1;
                    if self.read(&mut cursor)? == value {
                        break matched_target;
                    }
                };
                ensure!(target < self.data.len(), "case target outside scenario");
                cursor.pc = target;
            }
            0x0259 => {
                let table_end = cursor.dword()? as usize;
                let selector = self.read(&mut cursor)?;
                ensure!(
                    table_end >= cursor.pc && table_end <= self.data.len(),
                    "indexed jump table end {table_end:#x} outside scenario"
                );
                let mut index = 0;
                while cursor.pc < table_end {
                    let address = self.read(&mut cursor)?;
                    ensure!(
                        cursor.pc <= table_end,
                        "operand crosses indexed jump table end"
                    );
                    if index == selector {
                        let target = address
                            .checked_sub(self.script_base)
                            .context("indexed jump address outside scenario")?
                            as usize;
                        ensure!(
                            target < self.data.len(),
                            "indexed jump target outside scenario"
                        );
                        cursor.pc = target;
                        break;
                    }
                    index += 1;
                }
            }
            0x0258 => {
                let address = self.read(&mut cursor)?;
                let target = address
                    .checked_sub(self.script_base)
                    .context("jump address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "jump target {target:#x} outside scenario"
                );
                cursor.pc = target;
            }
            0x03d0 => {
                let task = self.read(&mut cursor)?;
                let count = self.read(&mut cursor)? as i32;
                ensure!(
                    task == u32::MAX || task < CELLS as u32,
                    "scope release task out of bounds"
                );
                let scopes = std::iter::once((self.current_task, &mut self.named_scopes)).chain(
                    self.tasks
                        .iter_mut()
                        .map(|(&id, state)| (id, &mut state.scopes)),
                );
                for (id, scopes) in scopes {
                    if task != u32::MAX && task != id {
                        continue;
                    }
                    // Native count -1 is the top index, not the scope count:
                    // retain the oldest scope. Explicit positive counts may pop it.
                    let count = if count == -1 {
                        scopes.len().saturating_sub(1)
                    } else {
                        count.max(0) as usize
                    }
                    .min(scopes.len());
                    for scope in scopes.drain(scopes.len() - count..) {
                        self.allocations.remove(&scope.allocation);
                    }
                }
            }
            0x03cf => {
                let count = cursor.word()? as usize;
                ensure!(count <= 256, "named declaration exceeds 256 variables");
                if count == 0 {
                    if let Some(scope) = self.named_scopes.pop() {
                        self.allocations.remove(&scope.allocation);
                    }
                } else {
                    ensure!(
                        self.named_scopes.len() < 256,
                        "named scope depth exceeds 256"
                    );
                    let mut names = Vec::with_capacity(count);
                    for _ in 0..count {
                        ensure!(cursor.byte()? == 0x12, "unsupported declaration operand");
                        names.push(cursor.declaration_name()?);
                    }
                    let size = count * 40;
                    let mut bytes = vec![0; size];
                    for (i, name) in names.iter().enumerate() {
                        bytes[i * 40..i * 40 + name.len()].copy_from_slice(name);
                        bytes[i * 40 + 36] = 1;
                    }
                    let address = self.allocation_address(bytes.len())?;
                    self.allocations.insert(address, bytes);
                    self.named_scopes.push(NamedScope {
                        allocation: address,
                        names,
                    });
                }
            }
            0x00fa => {
                let root = self.read(&mut cursor)?;
                let address = self.read(&mut cursor)?;
                ensure!(
                    self.string_bytes(address)?.len() < 256,
                    "registry path exceeds engine buffer"
                );
                let path = self.string(address)?;
                self.registry_root = root;
                self.registry_path = path;
            }
            0x0034 => request = Some(PlatformRequest::PumpMessages),
            0x0a8d => request = Some(PlatformRequest::DisableIme),
            0x02f8 => {
                let value = self.read(&mut cursor)?;
                ensure!(self.sp > 0, "stack overflow pushing operand");
                self.sp -= 1;
                writes.push((Destination::Bank(8, self.sp), value));
            }
            0x02f9 => {
                ensure!(self.sp < CELLS, "stack underflow popping operand");
                let value = self.banks[&8][self.sp];
                // Destination operands see the stack after the pop. Restore
                // the pointer until validation succeeds, including indirects.
                self.sp += 1;
                let destination = self.destination(&mut cursor);
                self.sp -= 1;
                writes.push((destination?, value));
                self.sp += 1;
            }
            0x0212 => {
                let end = cursor.dword()?;
                let counter = end
                    .checked_sub(8)
                    .context("loop counter address underflow")?;
                let value = self.read(&mut cursor)?;
                let address = self
                    .script_base
                    .checked_add(counter)
                    .context("loop counter address overflow")?;
                memory_write = Some((self.memory_range(address, 4)?, value.to_le_bytes().to_vec()));
            }
            0x0213 => {
                let counter = cursor.pc;
                let value = cursor.dword()?.wrapping_sub(1);
                let target = cursor.dword()? as usize;
                if value != 0 {
                    ensure!(target < self.data.len(), "loop target outside scenario");
                    cursor.pc = target;
                }
                memory_write = Some((
                    self.memory_range(self.script_base + counter as u32, 4)?,
                    value.to_le_bytes().to_vec(),
                ));
            }
            0x0280 => {
                let count = cursor.word()? as usize;
                let argc = *self
                    .parameters
                    .last()
                    .context("parameter binding outside function")?
                    as usize;
                ensure!(count <= argc, "parameter binding exceeds argument count");
                for i in 0..count {
                    let value = self.parameters[self.parameters.len() - i - 2];
                    writes.push((self.destination(&mut cursor)?, value));
                }
            }
            0x0281 | 0x0283 => {
                let tag = cursor.byte()?;
                let destination = self.operand_address(tag, &mut cursor)?;
                if destination != 0 {
                    self.memory_range(destination, 4)?;
                }
                let operand = self.read(&mut cursor)?;
                let (base, target) = if opcode == 0x0283 {
                    *self.task_entries.get(&operand).with_context(|| {
                        format!("parameterized call to undefined task {operand}")
                    })?
                } else {
                    (
                        self.script_base,
                        operand
                            .checked_sub(self.script_base)
                            .context("function address outside scenario")?
                            as usize,
                    )
                };
                ensure!(
                    target < self.scenario_size(base)?,
                    "function target outside scenario"
                );
                let count = cursor.word()? as usize;
                ensure!(
                    self.parameters.len() + count + 2 <= CELLS,
                    "function parameter stack overflow"
                );
                ensure!(self.sp >= 2, "call stack overflow");
                let mut arguments = Vec::with_capacity(count);
                for _ in 0..count {
                    arguments.push(self.read(&mut cursor)?);
                }
                self.parameters.push(destination);
                self.parameters.extend(arguments.into_iter().rev());
                self.parameters.push(count as u32);
                writes.push((Destination::Bank(8, self.sp - 1), self.script_base));
                writes.push((
                    Destination::Bank(8, self.sp - 2),
                    self.script_base + cursor.pc as u32,
                ));
                self.sp -= 2;
                cursor.pc = target;
                next_base = base;
            }
            0x0285 => {
                let count = *self
                    .parameters
                    .last()
                    .context("function return without frame")? as usize;
                let start = self
                    .parameters
                    .len()
                    .checked_sub(count + 2)
                    .context("invalid function parameter frame")?;
                let destination = self.parameters[start];
                let range = if destination == 0 {
                    None
                } else {
                    Some(self.memory_range(destination, 4)?)
                };
                let value = self.read(&mut cursor)?;
                ensure!(self.sp <= CELLS - 2, "return stack underflow");
                // Native writes the return value before popping the saved PC.
                // Apply an overlapping result to this temporary stack snapshot.
                let stack_address = task_bank_base(self.current_task, 8) + self.sp as u32 * 4;
                let mut saved = self.memory_read(stack_address, 8)?;
                for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
                    let at = u64::from(destination) + i as u64;
                    if at >= u64::from(stack_address) && at < u64::from(stack_address) + 8 {
                        saved[(at - u64::from(stack_address)) as usize] = byte;
                    }
                }
                let address = u32::from_le_bytes(saved[..4].try_into()?);
                let base = u32::from_le_bytes(saved[4..].try_into()?);
                let target = address
                    .checked_sub(base)
                    .context("return address outside scenario")?
                    as usize;
                ensure!(
                    target < self.scenario_size(base)?,
                    "return target outside scenario"
                );
                memory_write = range.map(|range| (range, value.to_le_bytes().to_vec()));
                self.parameters.truncate(start);
                self.sp += 2;
                cursor.pc = target;
                next_base = base;
            }
            0x0262 | 0x0267 => {
                let operand = self.read(&mut cursor)?;
                let (base, target) = if opcode == 0x0267 {
                    *self
                        .task_entries
                        .get(&operand)
                        .with_context(|| format!("call to undefined task {operand}"))?
                } else {
                    (
                        self.script_base,
                        operand
                            .checked_sub(self.script_base)
                            .context("call address outside scenario")?
                            as usize,
                    )
                };
                ensure!(
                    target < self.scenario_size(base)?,
                    "call target {target:#x} outside scenario"
                );
                ensure!(self.sp >= 2, "call stack overflow");
                writes.push((Destination::Bank(8, self.sp - 1), self.script_base));
                writes.push((
                    Destination::Bank(8, self.sp - 2),
                    self.script_base + cursor.pc as u32,
                ));
                self.sp -= 2;
                cursor.pc = target;
                next_base = base;
            }
            0x026c | 0x026d => {
                ensure!(self.sp <= CELLS - 2, "return stack underflow");
                let address = self.banks[&8][self.sp];
                let base = self.banks[&8][self.sp + 1];
                let target = address
                    .checked_sub(base)
                    .context("return address outside scenario")?
                    as usize;
                ensure!(
                    target < self.scenario_size(base)?,
                    "return target {target:#x} outside scenario"
                );
                let count = if opcode == 0x026d {
                    self.sp += 2;
                    let count = self.read(&mut cursor);
                    self.sp -= 2;
                    count? as usize
                } else {
                    0
                };
                let next_sp = (self.sp + 2)
                    .checked_add(count)
                    .filter(|&sp| sp <= CELLS)
                    .context("return argument stack underflow")?;
                cursor.pc = target;
                self.sp = next_sp;
                next_base = base;
            }
            0x000f => {
                let task = self.read(&mut cursor)?;
                ensure!(task < CELLS as u32, "task release slot out of bounds");
                if task == self.current_task {
                    self.context_flags = 0;
                } else if let Some(state) = self.tasks.get_mut(&task) {
                    state.flags = 0;
                }
                self.task_entries.remove(&task);
                // Keep the scenario cache and task-local banks. Original 415460
                // clears entry/PC/flags, but does not reset stack or local data.
                // A later definition supplies a new entry and resets the stack.
                rescan = true;
            }
            0x000c => {
                let task = self.read(&mut cursor)?;
                ensure!(
                    task != self.current_task && task < CELLS as u32,
                    "unsupported task definition {task:#x}"
                );
                let address = self.read(&mut cursor)?;
                let target = address
                    .checked_sub(self.script_base)
                    .context("task address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "task target {target:#x} outside scenario"
                );
                definition = Some((task, self.script_base, target));
            }
            0x076c | 0x076d | 0x078a | 0x07e4 => {
                let task = self.read(&mut cursor)?;
                self.callback_tasks.insert(opcode, task);
            }
            0x02c7 => {
                let address = self.read(&mut cursor)?;
                let size = self.read(&mut cursor)? as usize;
                let value = self.read(&mut cursor)? as u8;
                memory_write = Some((self.memory_range(address, size)?, vec![value; size]));
            }
            0x02bd => {
                let address = self.read(&mut cursor)?;
                if address != 0 {
                    ensure!(
                        self.allocations.remove(&address).is_some(),
                        "free of unknown allocation {address:#x}"
                    );
                }
            }
            0x02bc => {
                let size = self.read(&mut cursor)? as usize;
                ensure!(
                    size > 0 && size <= 16 * 1024 * 1024,
                    "allocation size outside 1..16 MiB"
                );
                let destination = self.destination(&mut cursor)?;
                let address = self.allocation_address(size)?;
                self.allocations.insert(address, vec![0; size]);
                writes.push((destination, address));
            }
            0x09e2 => {
                let device = self.read(&mut cursor)?;
                let index = self.read(&mut cursor)? as i32;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::DeviceCaps { device, index });
            }
            0x049d => {
                let value = self.read(&mut cursor)?;
                self.mouse_mapping.value = value;
                event = Event::MouseButtonMapping {
                    location: location.clone(),
                    value,
                };
            }
            0x000a => {
                let value = self.read(&mut cursor)?.min(CELLS as u32);
                if value != 0 {
                    self.thread_limit = value;
                } else if self.thread_limit_override == 0 {
                    rescan = true;
                }
                self.thread_limit_override = value;
            }
            0xd382 => self.file_read_marker = 0xd382,
            0xaa82 => self.file_write_marker = 0xaa82,
            0x0048 => self.input_latches = [0; 2],
            0x0316 | 0x0317 => {
                let count = self.read(&mut cursor)? as usize;
                let sp = if opcode == 0x0316 {
                    self.sp.checked_sub(count)
                } else {
                    self.sp.checked_add(count)
                };
                self.sp = sp
                    .filter(|&v| v <= CELLS)
                    .context("stack allocation outside 0..1000")?;
            }
            0x0136 => self.save_encoding = self.read(&mut cursor)?,
            0x06ba => self.media_flags = self.read(&mut cursor)?,
            0x06bb => writes.push((self.destination(&mut cursor)?, self.media_flags)),
            0x0bcf => {
                let left = self.read(&mut cursor)?;
                let right = self.read(&mut cursor)?;
                let count = self.read(&mut cursor)?;
                let mut value = 0;
                for index in 0..count {
                    ensure!(index < 65536, "string comparison exceeds 64 KiB cap");
                    let left = self.memory_read(
                        left.checked_add(index).context("string address overflow")?,
                        1,
                    )?[0];
                    let right = self.memory_read(
                        right
                            .checked_add(index)
                            .context("string address overflow")?,
                        1,
                    )?[0];
                    if left != right {
                        value = if left < right { u32::MAX } else { 1 };
                        break;
                    }
                    if left == 0 {
                        break;
                    }
                }
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x0bd0 => {
                let destination = self.read(&mut cursor)?;
                let source = self.read(&mut cursor)?;
                let count = self.read(&mut cursor)? as usize;
                if count != 0 {
                    let range = self.memory_range(destination, count)?;
                    let mut bytes = vec![0; count];
                    let mut consumed = 0;
                    for byte in &mut bytes {
                        let address = source
                            .checked_add(consumed)
                            .context("string source overflow")?;
                        *byte = self.memory_read(address, 1)?[0];
                        consumed += 1;
                        if *byte == 0 {
                            break;
                        }
                    }
                    ensure!(
                        destination == source
                            || u64::from(destination) + count as u64 <= u64::from(source)
                            || u64::from(source) + u64::from(consumed) <= u64::from(destination),
                        "overlapping bounded string copy is unresolved"
                    );
                    memory_write = Some((range, bytes));
                }
            }
            0x02d6 => {
                let left = self.string_bytes(self.read(&mut cursor)?)?;
                let right = self.string_bytes(self.read(&mut cursor)?)?;
                let comparison = left
                    .iter()
                    .map(u8::to_ascii_lowercase)
                    .cmp(right.iter().map(u8::to_ascii_lowercase));
                let value = match comparison {
                    std::cmp::Ordering::Less => u32::MAX,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x02d5 => {
                let value = self.read(&mut cursor)?;
                let tag = *cursor
                    .data
                    .get(cursor.pc)
                    .context("truncated pointer destination")?;
                let value = if tag & 0x80 != 0 {
                    value.wrapping_sub(self.script_base)
                } else {
                    value
                };
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x038e => {
                let value = self.read(&mut cursor)?;
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x0391 | 0x0392 => {
                let saved = cursor.pc;
                let value = self.read(&mut cursor)?;
                cursor.pc = saved;
                let destination = self.destination(&mut cursor)?;
                writes.push((
                    destination,
                    if opcode == 0x0391 {
                        value.wrapping_add(1)
                    } else {
                        value.wrapping_sub(1)
                    },
                ));
            }
            0x0393..=0x0394 | 0x0396..=0x0398 | 0x039a..=0x039b | 0x039e..=0x039f => {
                let first = self.read(&mut cursor)?;
                let saved = cursor.pc;
                let second = self.read(&mut cursor)?;
                cursor.pc = saved;
                let destination = self.destination(&mut cursor)?;
                let value = match opcode {
                    0x393 => second.wrapping_add(first),
                    0x394 => second.wrapping_sub(first),
                    0x396 => first & second,
                    0x397 => first | second,
                    0x398 => first ^ second,
                    0x39a => second.wrapping_shr(first),
                    0x39b => second.wrapping_shl(first),
                    _ => second.wrapping_mul(first),
                };
                writes.push((destination, value));
            }
            0x03a0 => {
                let divisor = self.read(&mut cursor)?;
                ensure!(divisor != 0, "unsigned division by zero");
                let saved = cursor.pc;
                let dividend = self.read(&mut cursor)?;
                // The original also reads the old remainder before writing.
                self.read(&mut cursor)?;
                cursor.pc = saved;
                let mut destinations = Vec::with_capacity(2);
                for _ in 0..2 {
                    let tag = *cursor
                        .data
                        .get(cursor.pc)
                        .context("truncated division destination")?
                        & 0x7f;
                    ensure!(
                        matches!(tag, 2 | 6 | 8 | 10 | 12 | 14 | 18),
                        "indirect division destinations are unresolved"
                    );
                    destinations.push(self.destination(&mut cursor)?);
                }
                writes.push((destinations[0], dividend / divisor));
                writes.push((destinations[1], dividend % divisor));
            }
            0x01f4 | 0x01fe => {
                let left = self.read(&mut cursor)?;
                let comparison = cursor.byte()?;
                let right = self.read(&mut cursor)?;
                let target = cursor.dword()? as usize;
                ensure!(
                    opcode != 0x01fe || comparison <= 5,
                    "unsupported signed comparison {comparison:#x}"
                );
                // 0x1f4 compares unsigned; 0x1fe compares signed. Predicates are
                // the jump condition (the inverse of a compiler's source 'if').
                let less = if opcode == 0x1f4 {
                    left < right
                } else {
                    (left as i32) < (right as i32)
                };
                let greater = if opcode == 0x1f4 {
                    left > right
                } else {
                    (left as i32) > (right as i32)
                };
                let branch = match comparison {
                    0 => left != right,
                    1 => left == right,
                    2 => less,
                    3 => !greater,
                    4 => greater,
                    5 => !less,
                    6 => left & right == 0,
                    7 => left == 0 && right == 0,
                    _ => bail!("unsupported comparison {comparison:#x}"),
                };
                if branch {
                    ensure!(
                        target < self.data.len(),
                        "branch target {target:#x} outside scenario"
                    );
                    cursor.pc = target;
                }
            }
            0x03c0 | 0x09f6 => {
                let values = if opcode == 0x03c0 {
                    [247, 0x01328e21]
                } else {
                    self.viewport
                };
                for value in values {
                    writes.push((self.destination(&mut cursor)?, value));
                }
            }
            0x07ee | 0x07ef => {
                let window = self.read(&mut cursor)?;
                let index = self.read(&mut cursor)? as i32;
                request = Some(if opcode == 0x07ee {
                    response_destination = Some(self.destination(&mut cursor)?);
                    PlatformRequest::GetClassLong { window, index }
                } else {
                    let value = self.read(&mut cursor)?;
                    PlatformRequest::SetClassLong {
                        window,
                        index,
                        value,
                    }
                });
            }
            0x07b2 => {
                let class = self.string(self.read(&mut cursor)?)?;
                let title = self.string(self.read(&mut cursor)?)?;
                response_destination = Some(self.destination(&mut cursor)?);
                request = Some(PlatformRequest::FindWindow { class, title });
            }
            0x07a8 => {
                let window = self.read(&mut cursor)?;
                let title = self.string(self.read(&mut cursor)?)?;
                request = Some(PlatformRequest::SetWindowTitle { window, title });
            }
            _ => bail!("unsupported binary SCN opcode 0x{opcode:04x}"),
        }
        self.pc = cursor.pc;
        self.pending_bounds = bounds_destinations;
        self.pending_point = point_destinations;
        self.pending_sizes = size_destinations;
        self.pending_timer = timer_reply;
        if let Some((range, bytes)) = memory_write {
            self.memory_write(range, &bytes);
        }
        for (destination, value) in writes {
            self.write(destination, value);
        }
        if let Some((id, base, pc)) = definition {
            self.define_task(id, base, pc, false);
        }
        if rescan {
            self.rescan_tasks();
        }
        self.switch_scenario(next_base);
        self.steps += 1;
        if let Some(request) = request {
            event = Event::Platform { location, request };
            self.pending = Some((event.clone(), response_destination));
            self.pending_bytes = byte_destination;
        }
        Ok(event)
    }
}

/// Execution without a platform stops at the first request instead of guessing
/// an OS result or repeatedly emitting a pending request.
pub fn boot_binary(name: &str, data: &[u8]) -> Result<()> {
    let mut vm = BinaryVm::new(name, data.to_vec())?;
    loop {
        match vm.step()? {
            Event::End => return Ok(()),
            Event::Platform { location, request } => bail!(
                "{}:{:#x}: platform host required: {request:?}",
                location.scenario,
                location.offset
            ),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instruction(opcode: u16, operands: &[u8]) -> Vec<u8> {
        let mut code = opcode.to_le_bytes().to_vec();
        code.extend(operands);
        code
    }
    fn immediate(value: u32) -> Vec<u8> {
        let mut bytes = vec![4];
        bytes.extend(value.to_le_bytes());
        bytes
    }

    #[test]
    fn parameterized_library_call_preserves_caller_and_optional_result() {
        for discard in [false, true] {
            let mut args = if discard {
                immediate(0)
            } else {
                vec![12, 1, 0]
            };
            args.extend(immediate(77));
            args.extend(2u16.to_le_bytes());
            args.extend(immediate(11));
            args.extend(immediate(22));
            let mut code = instruction(0x283, &args);
            let return_pc = code.len();
            code.extend(instruction(0, &immediate(0)));
            let mut vm = BinaryVm::new("caller.scn", code).unwrap();
            let mut callee = instruction(0x280, &[2, 0, 12, 3, 0, 12, 4, 0]);
            callee.extend(instruction(0x285, &[12, 4, 0]));
            vm.scenarios.insert(
                0x40000000,
                Scenario {
                    name: "library.scn".into(),
                    data: callee,
                },
            );
            vm.define_task(77, 0x40000000, 0, false);
            vm.banks.get_mut(&12).unwrap()[1] = 99;
            vm.step().unwrap();
            assert_eq!(vm.name, "library.scn");
            assert_eq!(vm.sp, 998);
            vm.step().unwrap();
            assert_eq!(vm.banks[&12][3..5], [11, 22]);
            vm.step().unwrap();
            assert_eq!(vm.name, "caller.scn");
            assert_eq!(vm.pc, return_pc);
            assert_eq!(vm.banks[&12][1], if discard { 99 } else { 22 });
            assert_eq!(vm.sp, 1000);
            assert!(vm.parameters.is_empty());
        }
    }

    #[test]
    fn counted_loop_writes_its_counter_and_exits_at_zero() {
        let mut args = 28u32.to_le_bytes().to_vec();
        args.extend(immediate(2));
        let mut code = instruction(0x212, &args);
        code.extend(instruction(0x2f8, &immediate(7)));
        code.extend(instruction(0x213, &[0, 0, 0, 0, 11, 0, 0, 0]));
        code.extend(instruction(0, &immediate(0)));
        let mut vm = BinaryVm::new("loop.scn", code).unwrap();
        for _ in 0..5 {
            vm.step().unwrap();
        }
        assert_eq!(vm.pc, 28);
        assert_eq!(vm.sp, 998);
        assert_eq!(vm.banks[&8][998..1000], [7, 7]);
        assert_eq!(&vm.data[20..24], &[0; 4]);
    }

    #[test]
    fn file_metadata_and_reads_match_original_and_reject_destination_overflow() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/file-read-probe.json"
        ))
        .unwrap();
        let decode = |hex: &str| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in probe["cases"].as_array().unwrap() {
            let mut vm = BinaryVm::new("file.scn", decode(case["code"].as_str().unwrap())).unwrap();
            vm.allocations.insert(0x20006000, vec![0xa5; 80]);
            let event = vm.step().unwrap();
            assert_eq!(vm.step().unwrap(), event);
            assert!(vm.respond(0).is_err());
            assert_eq!(vm.step().unwrap(), event);
            if case["missing"].as_bool().unwrap() {
                continue;
            }
            match event {
                Event::Platform {
                    request: PlatformRequest::AssetSizes { name },
                    ..
                } => {
                    assert_eq!(name, "fixture.bin");
                    assert!(vm.respond_asset_into(&[1]).is_err());
                    let sizes = [
                        case["sizes"][0].as_u64().unwrap() as u32,
                        case["sizes"][1].as_u64().unwrap() as u32,
                    ];
                    vm.respond_asset_sizes(sizes).unwrap();
                    assert_eq!([vm.banks[&12][0], vm.banks[&12][1]], sizes);
                }
                Event::Platform {
                    request: PlatformRequest::ReadAssetInto { name, address },
                    ..
                } => {
                    assert_eq!((name.as_str(), address), ("fixture.bin", 0x20006000));
                    assert!(vm.respond_asset_sizes([1, 2]).is_err());
                    assert!(vm.respond_asset_into(&[0; 81]).is_err());
                    assert_eq!(vm.memory_read(address, 80).unwrap(), vec![0xa5; 80]);
                    let data: Vec<_> = (0..case["length"].as_u64().unwrap() as u8).collect();
                    vm.respond_asset_into(&data).unwrap();
                    assert_eq!(
                        vm.memory_read(address, 80).unwrap(),
                        decode(case["destination"].as_str().unwrap())
                    );
                }
                _ => panic!("unexpected file event"),
            }
            assert_eq!(vm.pc as u64, case["next_offset"].as_u64().unwrap());
            assert!(vm.pending.is_none());
        }
    }

    #[test]
    fn scope_release_matches_original_across_tasks_and_preserves_oldest_for_minus_one() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/scope-release-probe.json"
        ))
        .unwrap();
        for case in probe["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("scopes.scn", code).unwrap();
            vm.define_task(1, vm.script_base, 0, true);
            let mut allocated = 0;
            for state in case["steps"].as_array().unwrap() {
                vm.select_task(state["task"].as_u64().unwrap() as u32);
                vm.pc = state["offset"].as_u64().unwrap() as usize;
                if vm.data[vm.pc..vm.pc + 2] == [0xcf, 3] {
                    allocated += 1;
                }
                vm.step().unwrap();
                assert_eq!(vm.pc as u64, state["next_offset"].as_u64().unwrap());
                let scopes: Vec<Vec<u32>> = (0..2)
                    .map(|id| {
                        let scopes = if id == vm.current_task {
                            &vm.named_scopes
                        } else {
                            &vm.tasks[&id].scopes
                        };
                        scopes
                            .iter()
                            .map(|scope| {
                                u32::from_le_bytes(
                                    vm.memory_read(scope.allocation + 32, 4)
                                        .unwrap()
                                        .try_into()
                                        .unwrap(),
                                )
                            })
                            .collect()
                    })
                    .collect();
                assert_eq!(serde_json::json!(scopes), state["scopes"]);
                assert_eq!(
                    (allocated - vm.allocations.len()) as u64,
                    state["freed"].as_u64().unwrap()
                );
            }
        }
    }

    #[test]
    fn free_invalidates_pointers_reuses_space_and_preserves_other_allocations() {
        let mut code = instruction(0x02bc, &[immediate(64), vec![12, 0, 0]].concat());
        code.extend(instruction(0x02bd, &[12, 0, 0]));
        code.extend(instruction(
            0x02bc,
            &[immediate(64), vec![12, 1, 0]].concat(),
        ));
        code.extend(instruction(0x02bd, &[12, 1, 0]));
        code.extend(instruction(0x02bd, &[12, 1, 0]));
        let mut vm = BinaryVm::new("free.scn", code).unwrap();
        vm.step().unwrap();
        let pointer = vm.banks[&12][0];
        let other = vm.allocation_address(64).unwrap();
        vm.allocations.insert(other, vec![0xa5; 64]);
        vm.step().unwrap();
        assert_eq!(vm.banks[&12][0], pointer); // operand is not a write destination
        assert!(vm.memory_read(pointer, 1).is_err());
        assert_eq!(vm.memory_read(other, 64).unwrap(), vec![0xa5; 64]);
        vm.step().unwrap();
        assert_eq!(vm.banks[&12][1], pointer);
        vm.step().unwrap();
        assert!(
            vm.step()
                .unwrap_err()
                .to_string()
                .contains("unknown allocation")
        );
        assert_eq!(vm.allocations.len(), 1);
    }

    #[test]
    fn stream_lifecycle_matches_original_operand_boundaries() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/stream-lifecycle-probe.json"
        ))
        .unwrap();
        for case in probe["lifecycle"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("stream.scn", code).unwrap();
            vm.banks.get_mut(&12).unwrap()[0] = 0x70000001;
            let handle = if case["null"].as_bool().unwrap() {
                0
            } else {
                0x70000001
            };
            let expected = match case["opcode"].as_u64().unwrap() {
                0x6d8 => PlatformRequest::ReleaseAudioStream { handle },
                0x6da => PlatformRequest::StopAudioStream { handle },
                0x6d9 => PlatformRequest::PlayAudioStream { handle, flags: 2 },
                _ => unreachable!(),
            };
            let event = vm.step().unwrap();
            assert!(matches!(&event,Event::Platform {request,..} if *request==expected));
            assert_eq!(vm.step().unwrap(), event);
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
        }
    }

    #[test]
    fn released_task_entry_cannot_be_called_and_locals_survive_redefinition() {
        let mut code = instruction(0x000f, &immediate(10));
        code.extend(instruction(0x0267, &immediate(10)));
        let mut vm = BinaryVm::new("release.scn", code).unwrap();
        vm.define_task(10, vm.script_base, 0, true);
        vm.tasks.get_mut(&10).unwrap().locals[0] = 77;
        vm.step().unwrap();
        assert_eq!(vm.task_flags(10), 0);
        assert!(!vm.task_entries.contains_key(&10));
        assert_eq!(vm.thread_limit, 1);
        assert!(
            vm.step()
                .unwrap_err()
                .to_string()
                .contains("undefined task 10")
        );
        vm.define_task(10, vm.script_base, 0, true);
        assert_eq!(vm.tasks[&10].locals[0], 77);
        assert_eq!(vm.tasks[&10].sp, CELLS);
    }

    #[test]
    fn sound_commands_match_original_dispatcher() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/sound-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("sound.scn", code).unwrap();
            let value = case["value"].as_i64().unwrap() as u32;
            let command = match case["opcode"].as_u64().unwrap() {
                0x06a7 => SoundCommand::Play { flags: value },
                0x06a8 => SoundCommand::Stop,
                0x06a9 => SoundCommand::Volume {
                    attenuation: value as i32,
                },
                0x06aa => SoundCommand::Pan {
                    attenuation: value as i32,
                },
                0x06ab => SoundCommand::Frequency { hz: value },
                0x06af => SoundCommand::Status,
                _ => unreachable!(),
            };
            let event = vm.step().unwrap();
            assert!(
                matches!(&event, Event::Platform { request: PlatformRequest::Sound {id:23, command: actual},.. } if *actual == command)
            );
            assert_eq!(vm.step().unwrap(), event);
            vm.respond(case["result"].as_u64().unwrap_or(1) as u32)
                .unwrap();
            assert_eq!(
                vm.location().offset as u64,
                case["next_offset"].as_u64().unwrap()
            );
            if command == SoundCommand::Status {
                assert_eq!(vm.banks[&12][0], case["result"].as_u64().unwrap() as u32);
            }
        }
    }

    #[test]
    fn redraw_preserves_native_signed_edges_and_operand_boundary() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/redraw-probe.json"))
                .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("redraw.scn", bytes).unwrap();
            let Event::Platform {
                request: PlatformRequest::InvalidateRect { rect },
                ..
            } = vm.step().unwrap()
            else {
                panic!("expected redraw request")
            };
            assert_eq!(serde_json::json!(rect), case["rect"]);
            vm.respond(1).unwrap();
            assert_eq!(
                vm.location().offset,
                case["next_offset"].as_u64().unwrap() as usize
            );
        }
    }

    #[test]
    fn draw_list_is_bounded_and_reset_reuses_capacity() {
        let item = instruction(0x04bd, &immediate(0).repeat(8));
        let mut code = item.repeat(1024);
        code.extend(instruction(0x04ba, &[]));
        code.extend(item.repeat(1025));
        let mut vm = BinaryVm::new("queue.scn", code).unwrap();
        for _ in 0..1024 {
            vm.step().unwrap();
        }
        assert_eq!(vm.draw_list.len(), 1024);
        vm.step().unwrap();
        assert!(vm.draw_list.is_empty());
        for _ in 0..1024 {
            vm.step().unwrap();
        }
        assert!(vm.step().unwrap_err().to_string().contains("draw list"));
        assert_eq!(vm.draw_list.len(), 1024);
    }

    #[test]
    fn malformed_function_calls_and_returns_preserve_frames_and_stack() {
        for code in [
            instruction(0x281, &[4, 0, 0, 0, 0]),
            instruction(0x281, &[4, 0, 0, 0, 0, 0x84, 0, 0, 0, 0, 0xff, 0xff]),
            instruction(0x280, &[1, 0, 12, 0, 0]),
            instruction(0x285, &immediate(17)),
        ] {
            let mut vm = BinaryVm::new("bad-function.scn", code).unwrap();
            let before = vm.data.clone();
            assert!(vm.step().is_err());
            assert_eq!(vm.pc, 0);
            assert_eq!(vm.sp, CELLS);
            assert!(vm.parameters.is_empty());
            assert_eq!(vm.data, before);
        }
    }

    #[test]
    fn shared_surface_pointers_match_native_reads_writes_and_identity() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/surface-memory-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let memory = SharedMemory::zeroed(16).unwrap();
            memory.write(0, &decode(&case["initial"])).unwrap();
            let mut vm = BinaryVm::new("pixels.scn", decode(&case["code"])).unwrap();
            for state in case["states"].as_array().unwrap() {
                let event = vm.step().unwrap();
                if matches!(
                    event,
                    Event::Platform {
                        request: PlatformRequest::SurfacePixels { .. },
                        ..
                    }
                ) {
                    assert!(vm.respond(1).is_err());
                    assert_eq!(vm.step().unwrap(), event);
                    vm.respond_surface_pixels(Some(memory.clone())).unwrap();
                }
                assert_eq!(
                    vm.location().offset as u64,
                    state["offset"].as_u64().unwrap()
                );
                assert_eq!(
                    vm.mouse_mapping().value as u64,
                    state["mouse"].as_u64().unwrap()
                );
                assert_eq!(memory.read(0, 16).unwrap(), decode(&state["pixels"]));
            }
        }
    }

    #[test]
    fn shared_surface_replacement_invalidates_pointers_without_losing_pending_requests() {
        let mut operands = immediate(3);
        operands.extend([12, 0, 0]);
        let query = instruction(0x0528, &operands);
        let mut code = query.repeat(2);
        code.extend(instruction(0x0546, &immediate(3)));
        code.extend(query);
        let mut vm = BinaryVm::new("pixels.scn", code).unwrap();
        let memory = SharedMemory::zeroed(16).unwrap();
        vm.step().unwrap();
        let first = vm.respond_surface_pixels(Some(memory.clone())).unwrap();
        memory.write(12, &[1, 2, 3, 4]).unwrap();
        assert_eq!(vm.memory_read(first + 12, 4).unwrap(), [1, 2, 3, 4]);
        assert!(vm.memory_read(first + 13, 4).is_err());
        assert!(memory.write(usize::MAX, &[1]).is_err());
        assert!(memory.read(16, 1).is_err());
        assert!(SharedMemory::zeroed(0).is_err());
        assert!(SharedMemory::zeroed(16 * 1024 * 1024 + 1).is_err());
        vm.step().unwrap();
        // A legal destination can itself point into the old resource.
        vm.pending.as_mut().unwrap().1 = Some(Destination::Memory(first, 4));
        let replacement = SharedMemory::zeroed(16).unwrap();
        let second = vm
            .respond_surface_pixels(Some(replacement.clone()))
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(memory.read(0, 4).unwrap(), second.to_le_bytes());
        assert!(vm.memory_read(first, 1).is_err());
        vm.step().unwrap();
        assert!(vm.respond(0).is_err());
        assert!(vm.memory_read(second, 1).is_ok());
        vm.respond(1).unwrap();
        assert!(vm.memory_read(second, 1).is_err());
        vm.step().unwrap();
        assert_eq!(vm.respond_surface_pixels(None).unwrap(), 0);
        assert_eq!(vm.banks[&12][0], 0);
    }

    #[test]
    fn surface_release_invalidates_old_pointers_only_after_success() {
        let mut args = immediate(20);
        args.extend([12, 0, 0]);
        let mut code = instruction(0x528, &args);
        code.extend(instruction(0x547, &immediate(20)));
        code.extend(instruction(0x528, &args));
        let mut vm = BinaryVm::new("release.scn", code).unwrap();
        vm.step().unwrap();
        let pointer = vm
            .respond_surface_pixels(Some(SharedMemory::zeroed(16).unwrap()))
            .unwrap();
        vm.step().unwrap();
        assert!(vm.respond(0).is_err());
        assert!(vm.memory_read(pointer, 1).is_ok());
        vm.respond(1).unwrap();
        assert!(vm.memory_read(pointer, 1).is_err());
        assert!(vm.shared_regions.is_empty());
        vm.step().unwrap();
        assert_eq!(vm.respond_surface_pixels(None).unwrap(), 0);
        for id in [256, u32::MAX] {
            let mut vm = BinaryVm::new("invalid.scn", instruction(0x547, &immediate(id))).unwrap();
            assert!(vm.step().is_err());
            assert_eq!(vm.pc, 0);
        }
    }

    #[test]
    fn scheduler_matches_original_instruction_order_polls_and_task_memory() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/scheduler-probe.json"
        ))
        .unwrap();
        for case in probe["cases"].as_array().unwrap() {
            let programs: Vec<Vec<u8>> = case["programs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|hex| {
                    let hex = hex.as_str().unwrap();
                    (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                        .collect()
                })
                .collect();
            let mut vm = BinaryVm::new("caller.scn", programs[0].clone()).unwrap();
            vm.set_dispatch_quantum(case["quantum"].as_u64().unwrap() as u32)
                .unwrap();
            vm.scenarios.insert(
                0x4000_0000,
                Scenario {
                    name: "worker.scn".into(),
                    data: programs[1].clone(),
                },
            );
            vm.next_scenario_base = 0x4000_1000;
            vm.define_task(1, 0x4000_0000, 0, case.get("active").is_none());
            vm.thread_limit = 2;
            let mut events = Vec::new();
            loop {
                assert!(events.len() < 1000, "{}", case["name"]);
                let before: Vec<_> = (0..2)
                    .map(|id| {
                        if id == vm.current_task {
                            (vm.sp, vm.banks[&12][0])
                        } else {
                            let task = &vm.tasks[&id];
                            (task.sp, task.locals[0])
                        }
                    })
                    .collect();
                let shared = vm.banks[&10][0];
                let event = vm.scheduled_step().unwrap();
                if event == Event::End {
                    break;
                }
                if event == Event::SchedulerPoll {
                    events.push(serde_json::json!({"event":"poll"}));
                    assert_eq!(vm.scheduled_step().unwrap(), event);
                    vm.respond(1).unwrap();
                    continue;
                }
                let location = match &event {
                    Event::BinaryInstruction { location, .. }
                    | Event::Platform { location, .. }
                    | Event::MouseButtonMapping { location, .. } => location,
                    other => panic!("unexpected event {other:?}"),
                };
                let (sp, local) = before[vm.current_task as usize];
                events.push(
                    serde_json::json!({"event":"instruction", "task":vm.current_task,
                    "pc":location.offset, "sp":sp, "local":local, "shared":shared}),
                );
                if let Event::Platform { request, .. } = &event {
                    assert_eq!(vm.scheduled_step().unwrap(), event);
                    match request {
                        PlatformRequest::LoadScenario { .. } => {
                            vm.respond_scenario(programs[1].clone()).unwrap()
                        }
                        PlatformRequest::PumpMessages => {
                            events.push(serde_json::json!({"event":"poll"}));
                            vm.respond(1).unwrap();
                        }
                        _ => panic!("unexpected request {request:?}"),
                    }
                }
            }
            assert_eq!(
                serde_json::json!(events),
                case["events"],
                "{}",
                case["name"]
            );
            for id in 0..2 {
                vm.select_task(id);
                assert_eq!(
                    serde_json::json!({"sp":vm.sp, "local":vm.banks[&12][0],
                    "popped":vm.banks[&12][1], "flags":vm.context_flags}),
                    case["final"][id as usize],
                    "{} task {id}",
                    case["name"]
                );
            }
            assert_eq!(vm.banks[&10][0], case["shared"].as_u64().unwrap() as u32);
        }
    }

    #[test]
    fn malformed_indexed_jumps_and_memory_operations_stop_without_writes() {
        let mut table = instruction(0x259, &100u32.to_le_bytes());
        table.extend(immediate(0));
        let mut crossing = instruction(0x259, &12u32.to_le_bytes());
        crossing.extend(immediate(1));
        crossing.extend(immediate(0));
        let mut target = instruction(0x259, &16u32.to_le_bytes());
        target.extend(immediate(0));
        target.extend(immediate(0));
        let mut cases = vec![table, crossing, target];
        for opcode in [0x305, 0x2d0, 0xbcf] {
            let mut code = instruction(opcode, &immediate(u32::MAX));
            if opcode == 0xbcf {
                code.extend(immediate(u32::MAX));
                code.extend(immediate(1));
            }
            code.extend(if opcode == 0x305 {
                immediate(77)
            } else {
                vec![12, 0, 0]
            });
            cases.push(code);
        }
        for opcode in [0x308, 0x309, 0x30a, 0x30b, 0x30c, 0x30d] {
            let mut code = instruction(opcode, &immediate(0xffff_fffe));
            code.extend(immediate(1));
            code.extend(if opcode & 1 == 0 {
                vec![12, 0, 0]
            } else {
                immediate(77)
            });
            cases.push(code);
        }
        for code in cases {
            let mut vm = BinaryVm::new("invalid.scn", code.clone()).unwrap();
            let error = vm.step().unwrap_err().to_string();
            assert_eq!(vm.pc, 0);
            assert_eq!(vm.data, code);
            assert_eq!(vm.banks[&12][0], 0);
            assert_eq!(vm.step().unwrap_err().to_string(), error);
        }
    }

    #[test]
    fn scheduler_holds_task_until_reply_and_honors_host_shutdown() {
        let mut code = instruction(0x3bd, &[12, 0, 0]);
        code.extend(instruction(0, &immediate(0)));
        let mut vm = BinaryVm::new("caller.scn", code).unwrap();
        let mut worker = instruction(0x38e, &immediate(99));
        worker.extend([12, 0, 0]);
        worker.extend(instruction(0, &immediate(0)));
        vm.scenarios.insert(
            0x4000_0000,
            Scenario {
                name: "worker.scn".into(),
                data: worker,
            },
        );
        vm.define_task(1, 0x4000_0000, 0, true);
        vm.rescan_tasks();
        assert_eq!(vm.scheduled_step().unwrap(), Event::SchedulerPoll);
        vm.respond(1).unwrap();
        let request = vm.scheduled_step().unwrap();
        assert!(matches!(
            request,
            Event::Platform {
                request: PlatformRequest::ClockMilliseconds,
                ..
            }
        ));
        assert_eq!(vm.scheduled_step().unwrap(), request);
        assert_eq!(vm.current_task(), 0);
        vm.respond(123).unwrap();
        vm.scheduled_step().unwrap();
        assert_eq!(vm.current_task(), 1);
        assert_eq!(vm.banks[&12][0], 99);
        assert_eq!(vm.tasks[&0].locals[0], 123);
        assert_eq!(vm.scheduled_step().unwrap(), Event::SchedulerPoll);
        vm.respond(0).unwrap();
        assert_eq!(vm.scheduled_step().unwrap(), Event::End);
        assert_eq!(vm.steps, 2);
    }

    #[test]
    fn cross_scenario_calls_match_original_loader_and_dispatcher() {
        let probe: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/scenario-call-probe.json"
        ))
        .unwrap();
        let decode = |key: &str| {
            let text = probe[key].as_str().unwrap();
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        let mut vm = BinaryVm::new("caller.scn", decode("caller")).unwrap();
        for expected in probe["states"].as_array().unwrap() {
            if let Event::Platform { request, .. } = vm.step().unwrap() {
                assert_eq!(
                    request,
                    PlatformRequest::LoadScenario {
                        id: 2,
                        name: "library.scn".into(),
                        activate: false
                    }
                );
                assert!(vm.respond(1).is_err());
                assert!(vm.respond_scenario(Vec::new()).is_err());
                assert!(vm.scenarios.is_empty());
                vm.respond_scenario(decode("library")).unwrap();
            }
            assert_eq!(
                serde_json::json!({ "scenario": vm.name, "pc": vm.pc,
                "sp": vm.sp, "mouse": vm.mouse_mapping.value }),
                *expected
            );
        }
        assert_eq!(vm.name, "caller.scn");
        assert_eq!(vm.sp, CELLS);
        assert_eq!(vm.mouse_mapping.value, b'B' as u32);
        assert_eq!(vm.data[128], b'Z');
        assert!(vm.respond_scenario(decode("library")).is_err());
    }

    #[test]
    fn invalid_pop_and_return_cleanup_keep_stack_and_location() {
        let mut code = instruction(0x2f8, &immediate(7));
        code.extend(instruction(0x2f9, &[0xff]));
        let mut vm = BinaryVm::new("bad-pop.scn", code).unwrap();
        vm.step().unwrap();
        let pc = vm.pc;
        assert!(vm.step().is_err());
        assert_eq!((vm.sp, vm.pc), (999, pc));
        assert_eq!(vm.banks[&8][999], 7);

        let mut code = instruction(0x262, &[0x84, 32, 0, 0, 0]);
        code.resize(32, 0);
        code.extend(instruction(0x26d, &immediate(1)));
        let mut vm = BinaryVm::new("bad-return.scn", code).unwrap();
        vm.step().unwrap();
        let stack = vm.banks[&8].clone();
        assert!(vm.step().is_err());
        assert_eq!((vm.sp, vm.pc), (998, 32));
        assert_eq!(vm.banks[&8], stack);
    }

    #[test]
    fn matches_original_dispatcher_core_probes() {
        let probe: serde_json::Value =
            serde_json::from_str(include_str!("../../../docs/validation/scn-core-probe.json"))
                .unwrap();
        for case in probe["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new(case["name"].as_str().unwrap(), code).unwrap();
            let mut clocks = case["clocks"].as_array().into_iter().flatten();
            let mut sleeps = 0;
            for (step, expected) in case["states"].as_array().unwrap().iter().enumerate() {
                if let Some(task) = case["tasks"].get(step) {
                    vm.current_task = task.as_u64().unwrap() as u32;
                }
                match vm.step().unwrap() {
                    Event::Platform {
                        request: PlatformRequest::WaitTaskTimer { epoch, duration },
                        ..
                    } => loop {
                        let now = clocks.next().unwrap().as_u64().unwrap() as u32;
                        let ready = now.wrapping_sub(epoch) >= duration;
                        assert!(vm.respond(2).is_err());
                        vm.respond(u32::from(ready)).unwrap();
                        if ready {
                            break;
                        }
                        sleeps += 1;
                        let steps = vm.steps;
                        assert!(matches!(
                            vm.scheduled_step().unwrap(),
                            Event::Platform {
                                request: PlatformRequest::WaitTaskTimer { .. },
                                ..
                            }
                        ));
                        assert_eq!(vm.steps, steps);
                    },
                    Event::Platform {
                        request: PlatformRequest::ClockMilliseconds,
                        ..
                    } => {
                        vm.respond(clocks.next().unwrap().as_u64().unwrap() as u32)
                            .unwrap();
                    }
                    Event::Platform {
                        request: PlatformRequest::LoadAsset { .. },
                        ..
                    } => {
                        let hex = case["asset"].as_str().unwrap();
                        let data = (0..hex.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                            .collect();
                        vm.respond_asset(data).unwrap();
                    }
                    _ => {}
                }
                let actual = serde_json::json!({
                    "pc": vm.pc, "mouse": vm.mouse_mapping.value, "sp": vm.sp,
                    "thread_limit": vm.thread_limit, "override": vm.thread_limit_override,
                    "read_marker": vm.file_read_marker, "write_marker": vm.file_write_marker,
                    "media_flags": vm.media_flags, "save_encoding": vm.save_encoding,
                    "context_flags": vm.context_flags, "message_mode": vm.message_mode, "parameter_depth": vm.parameters.len(),
                });
                assert_eq!(&actual, expected, "{} step {}", vm.name, vm.steps);
            }
            assert!(clocks.next().is_none());
            assert_eq!(sleeps, case["sleeps"].as_array().map_or(0, Vec::len));
        }
    }

    #[test]
    fn clock_and_shutdown_replies_control_execution() {
        let mut code = instruction(0x3bd, &[12, 0, 0]);
        code.extend(instruction(0x34, &[]));
        let mut vm = BinaryVm::new("clock.scn", code).unwrap();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::ClockMilliseconds,
                ..
            }
        ));
        assert_eq!(vm.steps, 1);
        assert!(vm.respond_bytes(b"wrong type").is_err());
        vm.respond(u32::MAX).unwrap();
        assert_eq!(vm.banks[&12][0], u32::MAX);
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::PumpMessages,
                ..
            }
        ));
        vm.respond(0).unwrap();
        assert!(matches!(vm.step().unwrap(), Event::End));
        assert!(matches!(vm.step().unwrap(), Event::End));
        assert_eq!(vm.steps, 2);
    }

    #[test]
    fn device_failure_is_written_but_resource_failure_stops() {
        for opcode in [0x6a4, 0x6c2] {
            let mut vm = BinaryVm::new("device.scn", instruction(opcode, &[12, 0, 0])).unwrap();
            vm.step().unwrap();
            assert!(vm.respond(2).is_err());
            vm.respond(0).unwrap();
            assert_eq!(vm.banks[&12][0], 0);
        }
        let mut vm = BinaryVm::new("surface.scn", instruction(0x546, &immediate(2))).unwrap();
        let event = vm.step().unwrap();
        assert!(
            vm.respond(0)
                .unwrap_err()
                .to_string()
                .contains("surface.scn:0x0")
        );
        assert_eq!(vm.step().unwrap(), event);
        vm.respond(1).unwrap();
    }

    #[test]
    fn archive_registration_lowercases_in_place_and_deduplicates() {
        let first = instruction(0xdd, b"\x10DATA.WAR\0");
        let mut code = first.clone();
        code.extend(instruction(0xdd, b"\x10data.war\0"));
        let mut vm = BinaryVm::new("archive.scn", code).unwrap();
        vm.step().unwrap();
        assert_eq!(&vm.data[3..first.len()], b"data.war\0");
        vm.step().unwrap();
        assert_eq!(vm.archive_paths, ["data.war"]);
        let mut invalid =
            BinaryVm::new("archive.scn", instruction(0xdd, b"\x10\x82\xa0\0")).unwrap();
        assert!(invalid.step().is_err());
        assert!(invalid.archive_paths.is_empty());
    }

    #[test]
    fn platform_string_replies_are_bounded_and_can_retry() {
        let mut vm = BinaryVm::new("directory.scn", instruction(0xa28, &[0x4c, 0, 0])).unwrap();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::ProjectDirectory,
                ..
            }
        ));
        assert!(vm.respond(1).is_err());
        for bytes in [vec![b'x'; 256], b"bad\0path".to_vec()] {
            assert!(vm.respond_bytes(&bytes).is_err());
            assert!(vm.banks[&12].iter().all(|&v| v == 0));
        }
        vm.respond_bytes(b"Saves\\").unwrap();
        assert_eq!(vm.string_bytes(bank_base(12)).unwrap(), b"Saves\\");
        assert!(vm.respond_bytes(b"again").is_err());
    }

    #[test]
    fn ini_read_uses_old_destination_as_default() {
        let mut code = instruction(0x104, b"\x10settings.ini\0");
        let mut args = immediate(77);
        args.extend([12, 0, 0]);
        code.extend(instruction(0x38e, &args));
        code.extend(instruction(0x107, b"\x10Engine\0\x10Volume\0\x0c\0\0"));
        let mut vm = BinaryVm::new("ini.scn", code).unwrap();
        vm.step().unwrap();
        vm.step().unwrap();
        assert!(matches!(vm.step().unwrap(), Event::Platform {
            request: PlatformRequest::ReadIniInteger { file, section, key, default: 77 }, ..
        } if file == "settings.ini" && section == "Engine" && key == "Volume"));
        vm.respond(23).unwrap();
        assert_eq!(vm.banks[&12][0], 23);
    }

    #[test]
    fn formatting_consumes_terminator_and_bounds_destination() {
        let mut args = vec![0x4c, 0, 0];
        args.extend(b"\x10save%03d.dat\0");
        args.extend(immediate(54));
        args.push(0xff);
        let mut code = instruction(0x2db, &args);
        let next = code.len();
        code.extend(instruction(0x49d, &immediate(7)));
        let mut vm = BinaryVm::new("format.scn", code).unwrap();
        vm.step().unwrap();
        assert_eq!(vm.pc, next);
        assert_eq!(vm.string_bytes(bank_base(12)).unwrap(), b"save054.dat");
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping.value, 7);
        for end in 0..args.len() {
            let mut vm = BinaryVm::new("short.scn", instruction(0x2db, &args[..end])).unwrap();
            assert!(vm.step().is_err());
            assert!(vm.banks[&12].iter().all(|&v| v == 0));
        }
        args[1..3].copy_from_slice(&999u16.to_le_bytes());
        let mut vm = BinaryVm::new("bounds.scn", instruction(0x2db, &args)).unwrap();
        assert!(vm.step().is_err());
        assert!(vm.banks[&12].iter().all(|&v| v == 0));
    }

    #[test]
    fn repeated_local_scopes_reuse_addresses_without_overwriting_live_memory() {
        let mut args = immediate(64);
        args.extend([12, 0, 0]);
        let mut code = instruction(0x2bc, &args);
        let loop_start = code.len();
        code.extend(instruction(0x3cf, b"\x01\0\x12local\0"));
        code.extend(instruction(0x3cf, b"\0\0"));
        code.extend(instruction(
            0x258,
            &immediate(SCRIPT_BASE + loop_start as u32),
        ));
        let mut vm = BinaryVm::new("scope-loop.scn", code).unwrap();
        vm.step().unwrap();
        let persistent = vm.banks[&12][0];
        vm.allocations.get_mut(&persistent).unwrap().fill(0xa5);
        let mut previous = None;
        // The old monotonic allocator exhausted its 256 MiB address arena
        // after fewer than 32,768 push/pop pairs, despite only one live scope.
        for _ in 0..40000 {
            vm.step().unwrap();
            let scope = vm.named_scopes.last().unwrap().allocation;
            assert_eq!(*previous.get_or_insert(scope), scope);
            assert_ne!(scope, persistent);
            assert_eq!(vm.allocations[&scope][32..36], [0; 4]);
            vm.step().unwrap();
            assert!(vm.memory_read(scope, 4).is_err());
            vm.step().unwrap();
        }
        assert_eq!(vm.allocations.len(), 1);
        assert_eq!(vm.allocations[&persistent], vec![0xa5; 64]);
        assert!(vm.memory_read(persistent + 64, 1).is_err());
    }

    #[test]
    fn invalid_named_declarations_do_not_allocate_or_push_scopes() {
        for args in [
            b"\x02\0\x12valid\0\x12truncated".as_slice(),
            b"\x01\0\x12array[4]\0",
            b"\x01\0\x12\0",
        ] {
            let mut vm = BinaryVm::new("scope.scn", instruction(0x3cf, args)).unwrap();
            assert!(vm.step().is_err());
            assert!(vm.allocations.is_empty());
            assert!(vm.named_scopes.is_empty());
            assert_eq!(vm.pc, 0);
        }
        let mut code = instruction(0x3cf, b"\x01\0\x12local\0");
        code.extend(instruction(0x3cf, b"\0\0"));
        code.extend(instruction(0x49d, b"\x12local\0"));
        let mut vm = BinaryVm::new("scope.scn", code).unwrap();
        vm.step().unwrap();
        vm.step().unwrap();
        assert!(vm.allocations.is_empty());
        assert!(
            vm.step()
                .unwrap_err()
                .to_string()
                .contains("undefined named variable")
        );
    }

    #[test]
    fn matches_original_mouse_masks() {
        for (value, masks) in [
            (0, [0, 1, 2, 3, 4, 5, 6, 7]),
            (1, [0, 2, 1, 3, 4, 6, 5, 7]),
            (2, [0, 2, 1, 3, 4, 6, 5, 7]),
            (u32::MAX, [0, 2, 1, 3, 4, 6, 5, 7]),
        ] {
            let mut vm = BinaryVm::new("mouse.scn", instruction(0x49d, &immediate(value))).unwrap();
            assert!(
                matches!(vm.step().unwrap(), Event::MouseButtonMapping { value: v, .. } if v == value)
            );
            assert_eq!(vm.pc, 7);
            for (physical, expected) in masks.into_iter().enumerate() {
                assert_eq!(vm.mouse_mapping().map_buttons(physical as u8), expected);
            }
        }
    }

    #[test]
    fn platform_request_waits_for_reply_and_writes_the_result() {
        let mut operands = immediate(0);
        operands.extend(immediate((-26i32) as u32));
        operands.extend([12, 3, 0]);
        let mut code = instruction(0x7ee, &operands);
        code.extend(instruction(0x49d, &[12, 3, 0]));
        let mut vm = BinaryVm::new("host.scn", code).unwrap();
        for _ in 0..3 {
            assert!(matches!(
                vm.step().unwrap(),
                Event::Platform {
                    request: PlatformRequest::GetClassLong {
                        window: 0,
                        index: -26
                    },
                    ..
                }
            ));
            assert_eq!(vm.pc, 15);
            assert_eq!(vm.steps, 1);
        }
        vm.respond(0x12345678).unwrap();
        assert!(vm.respond(0).is_err());
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping.value, 0x12345678);
    }

    #[test]
    fn malformed_operands_and_stack_errors_preserve_state() {
        for code in [
            instruction(0x49d, &immediate(1)),
            instruction(0x7ee, &[4, 0, 0, 0, 0, 4, 230, 255, 255, 255, 12, 0, 0]),
        ] {
            for end in 0..code.len() {
                let mut vm = BinaryVm::new("short.scn", code[..end].to_vec()).unwrap();
                let error = vm.step().unwrap_err().to_string();
                assert_eq!(vm.pc, 0);
                assert_eq!(vm.steps, 0);
                assert_eq!(vm.mouse_mapping.value, 0);
                assert_eq!(error, vm.step().unwrap_err().to_string());
            }
        }
        for code in [
            instruction(0x316, &immediate(1001)),
            instruction(0x317, &immediate(1)),
            instruction(0x49d, &[12, 232, 3]),
            instruction(0x49d, &[8, 0, 0]),
            instruction(0x49d, &[0x43, 0, 0, 0, 0]),
        ] {
            let mut vm = BinaryVm::new("invalid.scn", code).unwrap();
            assert!(vm.step().is_err());
            assert_eq!(vm.sp, 1000);
            assert_eq!(vm.pc, 0);
            assert_eq!(vm.mouse_mapping.value, 0);
        }
        let mut vm = BinaryVm::new("unknown.scn", vec![0xf5, 1]).unwrap();
        let error = vm.step().unwrap_err().to_string();
        assert!(error.contains("unknown.scn:0x0"));
        assert!(error.contains("opcode 0x01f5"));
    }

    #[test]
    fn malformed_second_destination_does_not_write_first() {
        let mut vm =
            BinaryVm::new("atomic.scn", instruction(0x9f6, &[12, 0, 0, 12, 232, 3])).unwrap();
        assert!(vm.step().is_err());
        assert_eq!(vm.banks[&12][0], 0);
        assert_eq!(vm.pc, 0);
        for (divisor, remainder) in [(0, vec![12, 1, 0]), (2, vec![12, 232, 3]), (2, vec![0xff])] {
            let mut args = immediate(divisor);
            args.extend([12, 0, 0]);
            args.extend(remainder);
            let mut vm = BinaryVm::new("bad-divide.scn", instruction(0x3a0, &args)).unwrap();
            vm.banks.get_mut(&12).unwrap()[0] = 127;
            assert!(vm.step().is_err());
            assert_eq!(vm.pc, 0);
            assert_eq!(vm.banks[&12][0], 127);
        }
    }

    #[test]
    fn text_color_controls_match_original_without_skipping_unknown_text() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/text-style-probe.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("text.scn", code).unwrap();
            let event = vm.step().unwrap();
            let clocks = case["clocks"].as_array().unwrap();
            if clocks.is_empty() {
                assert!(matches!(
                    event,
                    Event::BinaryInstruction { opcode: 0x84, .. }
                ));
            } else {
                assert_eq!(clocks.len(), 1);
                assert!(matches!(
                    event,
                    Event::Platform {
                        request: PlatformRequest::ClockMilliseconds,
                        ..
                    }
                ));
                assert_eq!(vm.scheduled_step().unwrap(), event);
                assert_eq!(vm.text_style, crate::text::TextStyle::default());
                vm.respond(clocks[0].as_u64().unwrap() as u32).unwrap();
            }
            assert_eq!(
                serde_json::json!(vm.text_style),
                case["style"],
                "{}",
                case["text"]
            );
            assert_eq!(vm.pc as u64, case["next_offset"].as_u64().unwrap());
        }
        let mut args = immediate(u32::MAX);
        args.extend(b"\x10_c1,2,3_text\0");
        let mut vm = BinaryVm::new("unknown-text.scn", instruction(0x84, &args)).unwrap();
        assert!(vm.step().is_err());
        assert_eq!(vm.pc, 0);
        assert_eq!(vm.text_style, crate::text::TextStyle::default());
    }

    #[test]
    fn text_cursor_matches_original_and_rejects_partial_operands() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/text-cursor-probe.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let hex = case["code"].as_str().unwrap();
            let code = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let mut vm = BinaryVm::new("cursor.scn", code).unwrap();
            for expected in case["snapshots"].as_array().unwrap() {
                vm.step().unwrap();
                assert_eq!(
                    serde_json::json!({"cursor":vm.text_cursor,
                    "mouse":vm.mouse_mapping.value,"next_offset":vm.pc}),
                    *expected
                );
            }
        }
        for opcode in [0x78, 0x79, 0x7a] {
            let args = if opcode == 0x79 {
                vec![12, 0, 0]
            } else {
                immediate(40)
            };
            let mut vm = BinaryVm::new("bad-cursor.scn", instruction(opcode, &args)).unwrap();
            vm.text_cursor = [10, 20, 30];
            assert!(vm.step().is_err());
            assert_eq!(vm.text_cursor, [10, 20, 30]);
            assert_eq!(vm.banks[&12][0], 0);
            assert_eq!(vm.pc, 0);
        }
    }

    #[test]
    fn array_format_matches_original_dispatcher_and_wine_and_validates_memory() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/validation/format-array-probe.json"
        ))
        .unwrap();
        let decode = |value: &serde_json::Value| {
            let hex = value.as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>()
        };
        for case in fixture["cases"].as_array().unwrap() {
            let mut vm = BinaryVm::new("format.scn", decode(&case["code"])).unwrap();
            vm.step().unwrap();
            assert_eq!(vm.pc as u64, case["next_offset"].as_u64().unwrap());
            let buffer = decode(&case["buffer"]);
            assert_eq!(&vm.data[252..252 + buffer.len()], buffer);
        }
        for (destination, arguments, format) in [
            (500, 128, "%99i"),
            (256, 510, "%i"),
            (128, 128, "%i"),
            (256, 128, "%s"),
        ] {
            let mut args = vec![0x84];
            args.extend((destination as u32).to_le_bytes());
            args.push(0x10);
            args.extend(format.as_bytes());
            args.push(0);
            args.push(0x84);
            args.extend((arguments as u32).to_le_bytes());
            let mut code = instruction(0x2da, &args);
            code.resize(512, 0xa5);
            let mut vm = BinaryVm::new("bad-format.scn", code.clone()).unwrap();
            assert!(vm.step().is_err());
            assert_eq!(vm.data, code);
            assert_eq!(vm.pc, 0);
        }
    }

    #[test]
    fn invalid_branch_target_is_diagnostic_not_a_panic() {
        let mut args = immediate(0);
        args.push(1);
        args.extend(immediate(0));
        args.extend(u32::MAX.to_le_bytes());
        let mut vm = BinaryVm::new("branch.scn", instruction(0x1f4, &args)).unwrap();
        assert!(vm.step().unwrap_err().to_string().contains("branch target"));
        assert_eq!(vm.pc, 0);
    }

    #[test]
    fn heap_memory_is_zeroed_bounded_and_distinct_from_script_addresses() {
        let mut args = immediate(8);
        args.extend([12, 0, 0]);
        let mut code = instruction(0x2bc, &args);
        code.extend(instruction(0x49d, &[13, 0, 0]));
        let mut args = vec![12, 0, 0];
        args.extend(immediate(8));
        args.extend(immediate(0x12));
        code.extend(instruction(0x2c7, &args));
        code.extend(instruction(0x49d, &[13, 0, 0]));
        let mut vm = BinaryVm::new("heap.scn", code).unwrap();
        vm.step().unwrap();
        let address = vm.banks[&12][0];
        assert!(address >= 0x2000_0000);
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping.value, 0);
        vm.step().unwrap();
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping.value, 0x12121212);
        assert!(vm.memory_read(address + 5, 4).is_err());
        assert!(vm.memory_read(address - 1, 4).is_err());
        assert!(vm.memory_read(u32::MAX, 4).is_err());
        assert!(vm.memory_read(address, usize::MAX).is_err());
    }

    #[test]
    fn invalid_fill_and_allocation_leave_memory_unchanged() {
        let mut args = vec![0x4c, 0, 0];
        args.extend(immediate(4001));
        args.extend(immediate(0xff));
        let mut vm = BinaryVm::new("fill.scn", instruction(0x2c7, &args)).unwrap();
        assert!(vm.step().is_err());
        assert!(vm.banks[&12].iter().all(|&value| value == 0));
        assert_eq!(vm.pc, 0);
        for size in [0, u32::MAX, 16 * 1024 * 1024 + 1] {
            let mut args = immediate(size);
            args.extend([12, 0, 0]);
            let mut vm = BinaryVm::new("allocate.scn", instruction(0x2bc, &args)).unwrap();
            assert!(vm.step().is_err());
            assert!(vm.allocations.is_empty());
            assert_eq!(vm.pc, 0);
        }
        let mut vm = BinaryVm::new("return.scn", instruction(0x26c, &[])).unwrap();
        assert!(vm.step().unwrap_err().to_string().contains("underflow"));
        assert_eq!(vm.sp, CELLS);
    }
}
