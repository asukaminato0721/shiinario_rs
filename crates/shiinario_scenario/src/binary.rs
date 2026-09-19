//! Verified subset of the supplied v2.47 engine. See docs/SCN_RESEARCH.md.
use crate::{Event, Location};
use anyhow::{Context, Result, bail, ensure};

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

/// Requests are explicit so a headless trace cannot invent operating-system results.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum PlatformRequest {
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
    Script(std::ops::Range<usize>),
    Bank(u8, std::ops::Range<usize>),
    Heap(u32, std::ops::Range<usize>),
}

fn bank_base(tag: u8) -> u32 {
    0x1100_0000 + u32::from(tag) * 0x10000
}

struct NamedScope {
    allocation: u32,
    names: Vec<Vec<u8>>,
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
}

pub struct BinaryVm {
    name: String,
    data: Vec<u8>,
    pc: usize,
    steps: usize,
    mouse_mapping: MouseButtonMapping,
    failure: Option<String>,
    // Direct banks indexed by operand tag. Threads beyond thread zero are not
    // executable yet. Keeping their scheduling limit does not imply support.
    banks: std::collections::BTreeMap<u8, Vec<u32>>,
    sp: usize,
    thread_limit: u32,
    thread_limit_override: u32,
    file_read_marker: u32,
    file_write_marker: u32,
    input_latches: [u32; 2],
    save_encoding: u32,
    media_flags: u32,
    viewport: [u32; 2],
    pending: Option<(Event, Option<Destination>)>,
    allocations: std::collections::BTreeMap<u32, Vec<u8>>,
    next_allocation: u32,
    // Defined but inactive task entry points. Activation/scheduling remains a
    // separate operation; unsupported activation instructions still stop.
    task_entries: std::collections::BTreeMap<u32, usize>,
    callback_tasks: std::collections::BTreeMap<u16, u32>,
    ended: bool,
    named_scopes: Vec<NamedScope>,
    registry_root: u32,
    registry_path: String,
    pending_bytes: Option<u32>,
}
impl BinaryVm {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Result<Self> {
        ensure!(data.len() <= 16 * 1024 * 1024, "scenario exceeds size cap");
        Ok(Self {
            name: name.into(),
            data,
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
            file_read_marker: 0,
            file_write_marker: 0,
            input_latches: [0; 2],
            save_encoding: 1,
            media_flags: 0,
            viewport: [800, 600],
            pending: None,
            allocations: Default::default(),
            next_allocation: 0x2000_0000,
            task_entries: Default::default(),
            callback_tasks: Default::default(),
            ended: false,
            named_scopes: Vec::new(),
            registry_root: 0,
            registry_path: String::new(),
            pending_bytes: None,
        })
    }
    pub fn location(&self) -> Location {
        Location {
            scenario: self.name.clone(),
            offset: self.pc,
            line: None,
        }
    }
    pub fn mouse_mapping(&self) -> MouseButtonMapping {
        self.mouse_mapping
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
            self.pending_bytes.is_none(),
            "platform request requires a byte-string reply"
        );
        let (event, destination) = self.pending.take().context("no pending platform request")?;
        if matches!(
            event,
            Event::Platform {
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
        Ok(())
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
                        address = address.wrapping_add(SCRIPT_BASE);
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
            return match tag & 0x3f {
                4 => Ok(SCRIPT_BASE.wrapping_add(cursor.dword()?)),
                bank @ (2 | 6 | 8 | 10 | 12 | 14) => {
                    let index = self.bank_index(bank, cursor.word()?)?;
                    Ok(bank_base(bank) + (index * if bank == 6 { 1 } else { 4 }) as u32)
                }
                0x12 => self.named_address(&cursor.variable_name()?),
                _ => bail!("unsupported address operand tag {tag:#04x} at {start:#x}"),
            };
        }
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
            0x10 if tag == 0x10 => {
                let address = SCRIPT_BASE + cursor.pc as u32;
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
                    let address = value.wrapping_add(if tag & 0x80 != 0 { SCRIPT_BASE } else { 0 });
                    return Ok(u32::from_le_bytes(
                        self.memory_read(address, 4)?.try_into().unwrap(),
                    ));
                }
                value
            }
            _ => bail!("unsupported operand tag {tag:#04x} at {start:#x}"),
        };
        Ok(if tag & 0x80 != 0 {
            value.wrapping_add(SCRIPT_BASE)
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
            address.wrapping_add(if tag & 0x80 != 0 { SCRIPT_BASE } else { 0 }),
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
        if let Some(r) = range(SCRIPT_BASE, self.data.len()) {
            return Ok(MemoryRange::Script(r));
        }
        for &bank in self.banks.keys() {
            if let Some(r) = range(bank_base(bank), CELLS * if bank == 6 { 1 } else { 4 }) {
                return Ok(MemoryRange::Bank(bank, r));
            }
        }
        if let Some((&base, data)) = self.allocations.range(..=address).next_back()
            && let Some(r) = range(base, data.len())
        {
            return Ok(MemoryRange::Heap(base, r));
        }
        bail!("invalid memory range {address:#x} + {len:#x}")
    }
    fn memory_read(&self, address: u32, len: usize) -> Result<Vec<u8>> {
        Ok(match self.memory_range(address, len)? {
            MemoryRange::Script(r) => self.data[r].to_vec(),
            MemoryRange::Heap(base, r) => self.allocations[&base][r].to_vec(),
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
            MemoryRange::Script(r) => self.data[r].copy_from_slice(bytes),
            MemoryRange::Heap(base, r) => {
                self.allocations.get_mut(&base).unwrap()[r].copy_from_slice(bytes)
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
        match self.execute() {
            Ok(event) => Ok(event),
            Err(error) => {
                let opcode = self
                    .data
                    .get(self.pc..self.pc + 2)
                    .map(|b| format!("0x{:04x}", u16::from_le_bytes([b[0], b[1]])))
                    .unwrap_or_else(|| "truncated".into());
                let message = format!(
                    "{}:{:#x}: {error:#}; opcode={opcode}, executed_steps={}, stack_pointer={}, thread_limit={}, mouse_button_mapping={:#x}, next_bytes={:02x?}",
                    self.name,
                    self.pc,
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
        let mut event = Event::BinaryInstruction {
            location: location.clone(),
            opcode,
        };
        match opcode {
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
            0x0304 => {
                let address = self.read(&mut cursor)?;
                let value = u32::from(self.memory_read(address, 1)?[0]);
                writes.push((self.destination(&mut cursor)?, value));
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
            0x0258 => {
                let address = self.read(&mut cursor)?;
                let target = address
                    .checked_sub(SCRIPT_BASE)
                    .context("jump address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "jump target {target:#x} outside scenario"
                );
                cursor.pc = target;
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
                        names.push(cursor.variable_name()?);
                    }
                    let address = self.next_allocation;
                    let size = count * 40;
                    let next = address
                        .checked_add(((size as u32 + 4095) & !4095) + 4096)
                        .context("allocation overflow")?;
                    ensure!(next < 0x3000_0000, "VM allocation address space exhausted");
                    let mut bytes = vec![0; size];
                    for (i, name) in names.iter().enumerate() {
                        bytes[i * 40..i * 40 + name.len()].copy_from_slice(name);
                        bytes[i * 40 + 36] = 1;
                    }
                    self.allocations.insert(address, bytes);
                    self.next_allocation = next;
                    self.named_scopes.push(NamedScope {
                        allocation: address,
                        names,
                    });
                }
            }
            0x00fa => {
                let root = self.read(&mut cursor)?;
                let path = self.string(self.read(&mut cursor)?)?;
                ensure!(path.len() < 256, "registry path exceeds engine buffer");
                self.registry_root = root;
                self.registry_path = path;
            }
            0x0034 => request = Some(PlatformRequest::PumpMessages),
            0x0a8d => request = Some(PlatformRequest::DisableIme),
            0x0262 => {
                let address = self.read(&mut cursor)?;
                let target = address
                    .checked_sub(SCRIPT_BASE)
                    .context("call address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "call target {target:#x} outside scenario"
                );
                ensure!(self.sp >= 2, "call stack overflow");
                writes.push((Destination::Bank(8, self.sp - 1), SCRIPT_BASE));
                writes.push((
                    Destination::Bank(8, self.sp - 2),
                    SCRIPT_BASE + cursor.pc as u32,
                ));
                self.sp -= 2;
                cursor.pc = target;
            }
            0x026c => {
                ensure!(self.sp <= CELLS - 2, "return stack underflow");
                let address = self.banks[&8][self.sp];
                let base = self.banks[&8][self.sp + 1];
                ensure!(
                    base == SCRIPT_BASE,
                    "return to unsupported script base {base:#x}"
                );
                let target = address
                    .checked_sub(SCRIPT_BASE)
                    .context("return address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "return target {target:#x} outside scenario"
                );
                cursor.pc = target;
                self.sp += 2;
            }
            0x000c => {
                let task = self.read(&mut cursor)?;
                ensure!(
                    task > 0 && task < CELLS as u32,
                    "unsupported task definition {task:#x}"
                );
                let address = self.read(&mut cursor)?;
                let target = address
                    .checked_sub(SCRIPT_BASE)
                    .context("task address outside scenario")?
                    as usize;
                ensure!(
                    target < self.data.len(),
                    "task target {target:#x} outside scenario"
                );
                self.task_entries.insert(task, target);
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
            0x02bc => {
                let size = self.read(&mut cursor)? as usize;
                ensure!(
                    size > 0 && size <= 16 * 1024 * 1024,
                    "allocation size outside 1..16 MiB"
                );
                let destination = self.destination(&mut cursor)?;
                let address = self.next_allocation;
                let next = address
                    .checked_add(((size as u32 + 4095) & !4095) + 4096)
                    .context("allocation address overflow")?;
                ensure!(next < 0x3000_0000, "VM allocation address space exhausted");
                self.allocations.insert(address, vec![0; size]);
                self.next_allocation = next;
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
                    self.thread_limit = 1;
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
            0x038e => {
                let value = self.read(&mut cursor)?;
                writes.push((self.destination(&mut cursor)?, value));
            }
            0x0396..=0x0398 => {
                let first = self.read(&mut cursor)?;
                let saved = cursor.pc;
                let second = self.read(&mut cursor)?;
                cursor.pc = saved;
                let destination = self.destination(&mut cursor)?;
                let value = match opcode {
                    0x396 => first & second,
                    0x397 => first | second,
                    _ => first ^ second,
                };
                writes.push((destination, value));
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
        if let Some((range, bytes)) = memory_write {
            self.memory_write(range, &bytes);
        }
        for (destination, value) in writes {
            self.write(destination, value);
        }
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
        if let Event::Platform { location, request } = vm.step()? {
            bail!(
                "{}:{:#x}: platform host required: {request:?}",
                location.scenario,
                location.offset
            );
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
            for expected in case["states"].as_array().unwrap() {
                vm.step().unwrap();
                let actual = serde_json::json!({
                    "pc": vm.pc, "mouse": vm.mouse_mapping.value, "sp": vm.sp,
                    "thread_limit": vm.thread_limit, "override": vm.thread_limit_override,
                    "read_marker": vm.file_read_marker, "write_marker": vm.file_write_marker,
                    "media_flags": vm.media_flags, "save_encoding": vm.save_encoding,
                });
                assert_eq!(&actual, expected, "{} step {}", vm.name, vm.steps);
            }
        }
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
