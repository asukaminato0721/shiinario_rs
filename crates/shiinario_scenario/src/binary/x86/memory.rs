use super::super::{BinaryVm, CELLS, MemoryRange, bank_base};
use super::Write;
use anyhow::{Context, Result, ensure};
use std::ops::Range;

pub(super) const STACK_BASE: u32 = 0xf000_0000;
pub(super) const STACK_SIZE: usize = 64 * 1024;
pub(super) const FLAGS_BASE: u32 = STACK_BASE + STACK_SIZE as u32;
const MAPPED_LIMIT: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Backing {
    Vm,
    Stack,
    Flags,
}
struct Segment {
    base: u32,
    bytes: Vec<u8>,
    backing: Backing,
    executable: bool,
    dirty: Option<Range<usize>>,
}

/// Private snapshots preserve aliasing. VM writes commit only after RET succeeds.
pub(super) struct Memory<'a> {
    vm: &'a BinaryVm,
    segments: Vec<Segment>,
    mapped: usize,
    last: usize,
    pub code_revision: u64,
}
impl<'a> Memory<'a> {
    pub fn new(vm: &'a BinaryVm) -> Self {
        Self {
            vm,
            segments: vec![
                Segment {
                    base: STACK_BASE,
                    bytes: vec![0; STACK_SIZE],
                    backing: Backing::Stack,
                    executable: false,
                    dirty: None,
                },
                Segment {
                    base: FLAGS_BASE,
                    bytes: vm.banks[&6]
                        .iter()
                        .flat_map(|value| value.to_le_bytes())
                        .collect(),
                    backing: Backing::Flags,
                    executable: false,
                    dirty: None,
                },
            ],
            mapped: STACK_SIZE + CELLS * 4,
            last: 0,
            code_revision: 0,
        }
    }
    fn locate(&mut self, address: u32, length: usize) -> Result<(usize, usize)> {
        let contains = |segment: &Segment| {
            address
                .checked_sub(segment.base)
                .map(|offset| offset as usize)
                .filter(|offset| {
                    *offset <= segment.bytes.len() && length <= segment.bytes.len() - *offset
                })
        };
        if let Some(offset) = contains(&self.segments[self.last]) {
            return Ok((self.last, offset));
        }
        if let Some((index, offset)) = self
            .segments
            .iter()
            .enumerate()
            .find_map(|(index, segment)| contains(segment).map(|offset| (index, offset)))
        {
            self.last = index;
            return Ok((index, offset));
        }
        let (base, size) = match self.vm.memory_range(address, length)? {
            MemoryRange::Script(range) => (address - range.start as u32, self.vm.data.len()),
            MemoryRange::Scenario(base, range) => (
                address - range.start as u32,
                self.vm.scenarios[&base].data.len(),
            ),
            MemoryRange::Heap(base, _) => (base, self.vm.allocations[&base].len()),
            MemoryRange::Shared(base, _) => (base, self.vm.shared_regions[&base].len()),
            MemoryRange::Bank(bank, range) => (
                address - range.start as u32,
                CELLS * if bank == 6 { 1 } else { 4 },
            ),
            MemoryRange::TaskBank(_, _, range) => (address - range.start as u32, CELLS * 4),
        };
        ensure!(
            self.mapped
                .checked_add(size)
                .is_some_and(|total| total <= MAPPED_LIMIT),
            "native x86 mapped memory exceeds 256 MiB"
        );
        ensure!(
            u64::from(base) + size as u64 <= u64::from(STACK_BASE),
            "native x86 region overlaps private stack"
        );
        let bytes = self.vm.memory_read(base, size)?;
        self.mapped += size;
        self.segments.push(Segment {
            base,
            bytes,
            backing: Backing::Vm,
            executable: false,
            dirty: None,
        });
        self.last = self.segments.len() - 1;
        Ok((self.last, (address - base) as usize))
    }
    pub fn read(&mut self, address: u32, width: usize) -> Result<u64> {
        ensure!(
            matches!(width, 1 | 2 | 4 | 8),
            "invalid native x86 read width"
        );
        if let Some(offset) = self.flag_offset(address, width)? {
            let mut value = 0;
            for byte in 0..width {
                value |=
                    (self.read(FLAGS_BASE + ((offset + byte) * 4) as u32, 4)? & 255) << (byte * 8);
            }
            return Ok(value);
        }
        let (index, offset) = self.locate(address, width)?;
        let mut bytes = [0; 8];
        bytes[..width].copy_from_slice(&self.segments[index].bytes[offset..offset + width]);
        Ok(u64::from_le_bytes(bytes))
    }
    pub fn write(&mut self, address: u32, width: usize, value: u64) -> Result<()> {
        ensure!(
            matches!(width, 1 | 2 | 4 | 8),
            "invalid native x86 write width"
        );
        if let Some(offset) = self.flag_offset(address, width)? {
            for byte in 0..width {
                self.write(
                    FLAGS_BASE + ((offset + byte) * 4) as u32,
                    4,
                    (value >> (byte * 8)) & 255,
                )?;
            }
            return Ok(());
        }
        let (index, offset) = self.locate(address, width)?;
        let segment = &mut self.segments[index];
        segment.bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
        if segment.executable {
            self.code_revision = self.code_revision.wrapping_add(1);
        }
        segment.dirty = Some(match segment.dirty.take() {
            Some(range) => range.start.min(offset)..range.end.max(offset + width),
            None => offset..offset + width,
        });
        Ok(())
    }
    pub fn instruction(&mut self, address: u32) -> Result<&[u8]> {
        ensure!(
            self.flag_offset(address, 1)?.is_none(),
            "native x86 cannot execute flag storage"
        );
        let (index, offset) = self.locate(address, 1)?;
        let segment = &mut self.segments[index];
        ensure!(
            matches!(segment.backing, Backing::Vm),
            "native x86 cannot execute stack or flag storage"
        );
        segment.executable = true;
        Ok(&segment.bytes[offset..segment.bytes.len().min(offset + 15)])
    }
    fn flag_offset(&self, address: u32, width: usize) -> Result<Option<usize>> {
        let offset = address.wrapping_sub(bank_base(6)) as usize;
        if offset < CELLS {
            ensure!(
                width <= CELLS - offset,
                "native x86 flag access exceeds bank bounds"
            );
            Ok(Some(offset))
        } else {
            Ok(None)
        }
    }
    pub fn finish(self) -> Result<Vec<Write>> {
        self.segments
            .into_iter()
            .filter(|s| !matches!(s.backing, Backing::Stack))
            .filter_map(|s| s.dirty.clone().map(|range| (s, range)))
            .map(|(segment, range)| {
                let Segment {
                    base,
                    bytes,
                    backing,
                    ..
                } = segment;
                if matches!(backing, Backing::Flags) {
                    let cells = range.start / 4..range.end.div_ceil(4);
                    let bytes = bytes[cells.start * 4..cells.end * 4].to_vec();
                    return Ok(Write::Flags(cells, bytes));
                }
                let address = base
                    .checked_add(range.start as u32)
                    .context("native x86 write address overflow")?;
                Ok(Write::Memory(
                    self.vm.memory_range(address, range.len())?,
                    bytes[range].to_vec(),
                ))
            })
            .collect()
    }
}
