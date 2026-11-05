//! Bounded 32-bit x86 interpretation for SCN 0276. No native host code executes.
mod memory;
mod simd;
use super::{BinaryVm, MemoryRange, task_bank_base};
use anyhow::{Context, Result, bail, ensure};
use iced_x86::{Code, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};
use memory::{FLAGS_BASE, Memory, STACK_BASE, STACK_SIZE};
use std::collections::BTreeMap;
use std::ops::Range;

const RETURN: u32 = 0xffff_fff0;
const STEP_LIMIT: u64 = 100_000_000;
const CF: u32 = 1;
const PF: u32 = 4;
const AF: u32 = 16;
const ZF: u32 = 64;
const SF: u32 = 128;
const DF: u32 = 1024;
const OF: u32 = 2048;
const EAX: usize = 0;
const ECX: usize = 1;
const EDX: usize = 2;
const ESP: usize = 4;
const EBP: usize = 5;
const ESI: usize = 6;
const EDI: usize = 7;

#[derive(Clone, Copy)]
struct RegisterView {
    slot: usize,
    width: usize,
    shift: u32,
}

pub(super) enum Write {
    Memory(MemoryRange, Vec<u8>),
    Flags(Range<usize>, Vec<u8>),
}
impl Write {
    pub(super) fn apply(self, vm: &mut BinaryVm) {
        match self {
            Self::Memory(range, bytes) => vm.memory_write(range, &bytes),
            Self::Flags(range, bytes) => {
                let flags = vm.banks.get_mut(&6).unwrap();
                for (index, value) in range.zip(bytes.as_chunks::<4>().0) {
                    flags[index] = u32::from_le_bytes(*value);
                }
            }
        }
    }
}
fn register_view(register: Register) -> Result<RegisterView> {
    use Register::*;
    let (slot, width, shift) = match register {
        EAX => (0, 4, 0),
        ECX => (1, 4, 0),
        EDX => (2, 4, 0),
        EBX => (3, 4, 0),
        ESP => (4, 4, 0),
        EBP => (5, 4, 0),
        ESI => (6, 4, 0),
        EDI => (7, 4, 0),
        AX => (0, 2, 0),
        CX => (1, 2, 0),
        DX => (2, 2, 0),
        BX => (3, 2, 0),
        SP => (4, 2, 0),
        BP => (5, 2, 0),
        SI => (6, 2, 0),
        DI => (7, 2, 0),
        AL => (0, 1, 0),
        CL => (1, 1, 0),
        DL => (2, 1, 0),
        BL => (3, 1, 0),
        AH => (0, 1, 8),
        CH => (1, 1, 8),
        DH => (2, 1, 8),
        BH => (3, 1, 8),
        _ => bail!("unsupported native x86 register {register:?}"),
    };
    Ok(RegisterView { slot, width, shift })
}
fn mask(width: usize) -> u64 {
    if width == 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    }
}
fn signed(value: u64, width: usize) -> i64 {
    ((value << (64 - width * 8)) as i64) >> (64 - width * 8)
}

struct Cpu<'a> {
    memory: Memory<'a>,
    registers: [u32; 8],
    mmx: [u64; 8],
    flags: u32,
    ip: u32,
    steps: u64,
    cache: BTreeMap<u32, Instruction>,
    code_revision: u64,
}
impl BinaryVm {
    pub(super) fn emulate_native(&self, address: u32) -> Result<Vec<Write>> {
        let mut cpu = Cpu {
            memory: Memory::new(self),
            registers: [0; 8],
            mmx: [0; 8],
            flags: 0x202,
            ip: address,
            steps: 0,
            cache: BTreeMap::new(),
            code_revision: 0,
        };
        let context = STACK_BASE;
        for (index, bank) in [14, 10, 6, 12, 8].into_iter().enumerate() {
            // The native ABI exposes flags as DWORDs. SCN address operands
            // expose the same flags as bytes; Memory preserves both views.
            let base = if bank == 6 {
                FLAGS_BASE
            } else {
                task_bank_base(self.current_task, bank)
            };
            let pointer = base + if bank == 8 { self.sp as u32 * 4 } else { 0 };
            cpu.memory
                .write(context + index as u32 * 4, 4, u64::from(pointer))?;
        }
        cpu.registers[ESP] = STACK_BASE + STACK_SIZE as u32 - 256;
        cpu.memory.write(cpu.registers[ESP], 4, u64::from(RETURN))?;
        cpu.memory
            .write(cpu.registers[ESP] + 4, 4, u64::from(context))?;
        while cpu.ip != RETURN {
            cpu.charge()?;
            let at = cpu.ip;
            let instruction = cpu.fetch()?;
            cpu.ip = instruction.next_ip32();
            cpu.execute(&instruction)
                .with_context(|| format!("native x86 {at:#x}: {:?}", instruction.mnemonic()))?;
        }
        cpu.memory.finish()
    }
}
impl Cpu<'_> {
    fn charge(&mut self) -> Result<()> {
        ensure!(
            self.steps < STEP_LIMIT,
            "native x86 execution exceeds {STEP_LIMIT} steps"
        );
        self.steps += 1;
        Ok(())
    }
    fn fetch(&mut self) -> Result<Instruction> {
        if self.code_revision != self.memory.code_revision {
            self.cache.clear();
            self.code_revision = self.memory.code_revision;
        }
        if let Some(instruction) = self.cache.get(&self.ip) {
            return Ok(*instruction);
        }
        ensure!(
            self.cache.len() < 16384,
            "native x86 decoded instruction limit exceeded"
        );
        let bytes = self.memory.instruction(self.ip)?;
        let instruction =
            Decoder::with_ip(32, bytes, u64::from(self.ip), DecoderOptions::NONE).decode();
        ensure!(
            !instruction.is_invalid(),
            "invalid native x86 instruction at {:#x}",
            self.ip
        );
        ensure!(
            !instruction.has_lock_prefix(),
            "native x86 LOCK is unsupported"
        );
        self.cache.insert(self.ip, instruction);
        Ok(instruction)
    }
    fn flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
    fn set_flag(&mut self, flag: u32, value: bool) {
        self.flags = (self.flags & !flag) | if value { flag } else { 0 };
    }
    fn register(&self, register: Register) -> Result<u64> {
        if (Register::MM0..=Register::MM7).contains(&register) {
            return Ok(self.mmx[register as usize - Register::MM0 as usize]);
        }
        if register == Register::None {
            return Ok(0);
        }
        let view = register_view(register)?;
        Ok(u64::from(self.registers[view.slot] >> view.shift) & mask(view.width))
    }
    fn set_register(&mut self, register: Register, value: u64) -> Result<()> {
        if (Register::MM0..=Register::MM7).contains(&register) {
            self.mmx[register as usize - Register::MM0 as usize] = value;
            return Ok(());
        }
        let view = register_view(register)?;
        let bits = (mask(view.width) as u32) << view.shift;
        self.registers[view.slot] =
            (self.registers[view.slot] & !bits) | (((value as u32) << view.shift) & bits);
        Ok(())
    }
    fn address(&self, i: &Instruction) -> Result<u32> {
        ensure!(
            matches!(
                i.memory_segment(),
                Register::DS | Register::ES | Register::SS | Register::CS
            ),
            "native x86 segment is unsupported"
        );
        for register in [i.memory_base(), i.memory_index()] {
            ensure!(
                register == Register::None || register_view(register)?.width == 4,
                "native x86 memory address requires 32-bit registers"
            );
        }
        Ok((self.register(i.memory_base())? as u32)
            .wrapping_add(
                (self.register(i.memory_index())? as u32).wrapping_mul(i.memory_index_scale()),
            )
            .wrapping_add(i.memory_displacement32()))
    }
    fn width(&self, i: &Instruction, op: u32) -> Result<usize> {
        Ok(match i.op_kind(op) {
            OpKind::Register => {
                let register = i.op_register(op);
                if (Register::MM0..=Register::MM7).contains(&register) {
                    8
                } else {
                    register_view(register)?.width
                }
            }
            OpKind::Memory => {
                let size = i.memory_size().size();
                ensure!(
                    matches!(size, 1 | 2 | 4 | 8),
                    "unsupported native x86 operand width {size}"
                );
                size
            }
            OpKind::Immediate8 => 1,
            OpKind::Immediate16 | OpKind::Immediate8to16 => 2,
            OpKind::Immediate32 | OpKind::Immediate8to32 => 4,
            OpKind::Immediate64 | OpKind::Immediate8to64 | OpKind::Immediate32to64 => 8,
            _ => bail!("unsupported native x86 operand kind {:?}", i.op_kind(op)),
        })
    }
    fn read(&mut self, i: &Instruction, op: u32) -> Result<u64> {
        match i.op_kind(op) {
            OpKind::Register => self.register(i.op_register(op)),
            OpKind::Memory => self.memory.read(self.address(i)?, self.width(i, op)?),
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => Ok(i.immediate(op)),
            OpKind::NearBranch16 | OpKind::NearBranch32 => Ok(i.near_branch_target()),
            _ => bail!("unsupported native x86 read operand {:?}", i.op_kind(op)),
        }
    }
    fn write(&mut self, i: &Instruction, op: u32, value: u64) -> Result<()> {
        match i.op_kind(op) {
            OpKind::Register => self.set_register(i.op_register(op), value),
            OpKind::Memory => self
                .memory
                .write(self.address(i)?, self.width(i, op)?, value),
            _ => bail!("unsupported native x86 destination {:?}", i.op_kind(op)),
        }
    }
    fn push(&mut self, value: u64, width: usize) -> Result<()> {
        self.registers[ESP] = self.registers[ESP].wrapping_sub(width as u32);
        self.memory.write(self.registers[ESP], width, value)
    }
    fn pop(&mut self, width: usize) -> Result<u64> {
        let value = self.memory.read(self.registers[ESP], width)?;
        self.registers[ESP] = self.registers[ESP].wrapping_add(width as u32);
        Ok(value)
    }
    fn zsp(&mut self, value: u64, width: usize) {
        let value = value & mask(width);
        self.set_flag(ZF, value == 0);
        self.set_flag(SF, value & (1 << (width * 8 - 1)) != 0);
        self.set_flag(PF, (value as u8).count_ones().is_multiple_of(2));
    }
    fn arithmetic(&mut self, a: u64, b: u64, carry: bool, width: usize, subtract: bool) -> u64 {
        let a = a & mask(width);
        let b = b & mask(width);
        let amount = b + u64::from(carry);
        let result = if subtract {
            a.wrapping_sub(amount)
        } else {
            a + amount
        };
        let sign = 1 << (width * 8 - 1);
        self.set_flag(
            CF,
            if subtract {
                a < amount
            } else {
                result > mask(width)
            },
        );
        self.set_flag(
            OF,
            if subtract {
                (a ^ b) & (a ^ result) & sign != 0
            } else {
                (!(a ^ b)) & (a ^ result) & sign != 0
            },
        );
        self.set_flag(AF, (a ^ b ^ result) & 16 != 0);
        self.zsp(result, width);
        result & mask(width)
    }
    fn condition(&self, m: Mnemonic) -> Option<bool> {
        use Mnemonic::*;
        Some(match m {
            Je => self.flag(ZF),
            Jne => !self.flag(ZF),
            Ja => !self.flag(CF) && !self.flag(ZF),
            Jae => !self.flag(CF),
            Jb => self.flag(CF),
            Jbe => self.flag(CF) || self.flag(ZF),
            Jg => !self.flag(ZF) && self.flag(SF) == self.flag(OF),
            Jge => self.flag(SF) == self.flag(OF),
            Jl => self.flag(SF) != self.flag(OF),
            Jle => self.flag(ZF) || self.flag(SF) != self.flag(OF),
            Jo => self.flag(OF),
            Jno => !self.flag(OF),
            Js => self.flag(SF),
            Jns => !self.flag(SF),
            Jp => self.flag(PF),
            Jnp => !self.flag(PF),
            Jecxz => self.registers[ECX] == 0,
            Jcxz => self.registers[ECX] as u16 == 0,
            _ => return None,
        })
    }
    fn execute(&mut self, i: &Instruction) -> Result<()> {
        use Mnemonic::*;
        let m = i.mnemonic();
        if let Some(taken) = self.condition(m) {
            if taken {
                self.ip = self.read(i, 0)? as u32;
            }
            return Ok(());
        }
        match m {
            Nop | Emms => {}
            Mov | Movzx | Movd | Movq => {
                let value = self.read(i, 1)?;
                let value = if m == Movd {
                    value & u64::from(u32::MAX)
                } else {
                    value
                };
                self.write(i, 0, value)?;
            }
            Movsx => {
                let value = self.read(i, 1)?;
                let width = self.width(i, 1)?;
                self.write(i, 0, signed(value, width) as u64)?;
            }
            Lea => {
                let value = self.address(i)?;
                self.write(i, 0, u64::from(value))?;
            }
            Xchg => {
                let a = self.read(i, 0)?;
                let b = self.read(i, 1)?;
                self.write(i, 0, b)?;
                self.write(i, 1, a)?;
            }
            Push => {
                let value = self.read(i, 0)?;
                let width = if self.width(i, 0)? == 2 { 2 } else { 4 };
                self.push(value, width)?;
            }
            Pop => {
                let width = self.width(i, 0)?;
                let value = self.pop(width)?;
                self.write(i, 0, value)?;
            }
            Pushfd => self.push(u64::from(self.flags), 4)?,
            Popfd => {
                self.flags = (self.pop(4)? as u32) | 2;
            }
            Pushad => {
                let values = self.registers;
                for value in values {
                    self.push(u64::from(value), 4)?;
                }
            }
            Popad => {
                for slot in (0..8).rev() {
                    let value = self.pop(4)? as u32;
                    if slot != ESP {
                        self.registers[slot] = value;
                    }
                }
            }
            Clc => self.set_flag(CF, false),
            Stc => self.set_flag(CF, true),
            Cmc => self.set_flag(CF, !self.flag(CF)),
            Cld => self.set_flag(DF, false),
            Std => self.set_flag(DF, true),
            Add | Adc | Sub | Sbb | Cmp => {
                let a = self.read(i, 0)?;
                let b = self.read(i, 1)?;
                let width = self.width(i, 0)?;
                ensure!(
                    width <= 4,
                    "native x86 integer arithmetic width exceeds 32 bits"
                );
                let carry = matches!(m, Adc | Sbb) && self.flag(CF);
                let value = self.arithmetic(a, b, carry, width, matches!(m, Sub | Sbb | Cmp));
                if m != Cmp {
                    self.write(i, 0, value)?;
                }
            }
            Inc | Dec => {
                let a = self.read(i, 0)?;
                let width = self.width(i, 0)?;
                let cf = self.flag(CF);
                let value = self.arithmetic(a, 1, false, width, m == Dec);
                self.set_flag(CF, cf);
                self.write(i, 0, value)?;
            }
            Neg => {
                let a = self.read(i, 0)?;
                let width = self.width(i, 0)?;
                let value = self.arithmetic(0, a, false, width, true);
                self.write(i, 0, value)?;
            }
            Not => {
                let a = self.read(i, 0)?;
                self.write(i, 0, !a)?;
            }
            And | Or | Xor | Test => {
                let a = self.read(i, 0)?;
                let b = self.read(i, 1)?;
                let value = match m {
                    Or => a | b,
                    Xor => a ^ b,
                    _ => a & b,
                };
                let width = self.width(i, 0)?;
                self.zsp(value, width);
                self.set_flag(CF, false);
                self.set_flag(OF, false);
                self.set_flag(AF, false);
                if m != Test {
                    self.write(i, 0, value)?;
                }
            }
            Shl | Shr | Sar | Rol | Ror => self.shift(i)?,
            Bswap => {
                let value = self.read(i, 0)? as u32;
                self.write(i, 0, u64::from(value.swap_bytes()))?;
            }
            Cdq => {
                self.registers[EDX] = if self.registers[EAX] & 0x8000_0000 != 0 {
                    u32::MAX
                } else {
                    0
                }
            }
            Cwde => self.registers[EAX] = (self.registers[EAX] as i16 as i32) as u32,
            Cwd => {
                let value = if self.registers[EAX] & 0x8000 != 0 {
                    u16::MAX
                } else {
                    0
                };
                self.set_register(Register::DX, u64::from(value))?;
            }
            Cbw => {
                let value = self.registers[EAX] as i8 as i16;
                self.set_register(Register::AX, value as u64)?;
            }
            Mul | Imul => self.multiply(i)?,
            Div | Idiv => self.divide(i)?,
            Jmp => self.ip = self.read(i, 0)? as u32,
            Call => {
                ensure!(
                    matches!(i.code(), Code::Call_rel32_32 | Code::Call_rm32),
                    "native x86 CALL requires a 32-bit target"
                );
                let target = self.read(i, 0)? as u32;
                self.push(u64::from(self.ip), 4)?;
                self.ip = target;
            }
            Ret => {
                ensure!(
                    matches!(i.code(), Code::Retnd | Code::Retnd_imm16),
                    "native x86 RET requires a 32-bit target"
                );
                self.ip = self.pop(4)? as u32;
                if i.op_count() != 0 {
                    self.registers[ESP] = self.registers[ESP].wrapping_add(i.immediate16() as u32);
                }
            }
            Leave => {
                ensure!(
                    i.code() == Code::Leaved,
                    "native x86 LEAVE requires 32 bits"
                );
                self.registers[ESP] = self.registers[EBP];
                self.registers[EBP] = self.pop(4)? as u32;
            }
            Loop | Loope | Loopne => {
                ensure!(
                    matches!(
                        i.code(),
                        Code::Loop_rel8_32_ECX | Code::Loope_rel8_32_ECX | Code::Loopne_rel8_32_ECX
                    ),
                    "native x86 LOOP requires a 32-bit counter"
                );
                self.registers[ECX] = self.registers[ECX].wrapping_sub(1);
                let taken = self.registers[ECX] != 0
                    && match m {
                        Loope => self.flag(ZF),
                        Loopne => !self.flag(ZF),
                        _ => true,
                    };
                if taken {
                    self.ip = self.read(i, 0)? as u32;
                }
            }
            Movsb | Movsw | Movsd | Stosb | Stosw | Stosd | Lodsb | Lodsw | Lodsd => {
                self.string(i)?
            }
            _ => {
                ensure!(self.simd(i)?, "unsupported native x86 instruction {m:?}");
            }
        }
        Ok(())
    }
    fn shift(&mut self, i: &Instruction) -> Result<()> {
        use Mnemonic::*;
        let m = i.mnemonic();
        let value = self.read(i, 0)?;
        let width = self.width(i, 0)?;
        let bits = width * 8;
        ensure!(width <= 4, "native x86 shift width exceeds 32 bits");
        let amount = (self.read(i, 1)? & 31) as usize;
        if amount == 0 {
            return Ok(());
        }
        let value = value & mask(width);
        let result = match m {
            Rol => {
                let n = amount % bits;
                ((value << n) | (value >> (bits - n))) & mask(width)
            }
            Ror => {
                let n = amount % bits;
                ((value >> n) | (value << (bits - n))) & mask(width)
            }
            Shl => (value << amount) & mask(width),
            Shr => value >> amount,
            Sar => (signed(value, width) >> amount) as u64 & mask(width),
            _ => unreachable!(),
        };
        self.set_flag(
            CF,
            match m {
                Rol => result & 1 != 0,
                Ror => result & (1 << (bits - 1)) != 0,
                Shl => amount <= bits && value & (1 << (bits - amount)) != 0,
                Sar => (signed(value, width) >> (amount - 1)) & 1 != 0,
                _ => amount <= bits && value & (1 << (amount - 1)) != 0,
            },
        );
        if amount == 1 {
            self.set_flag(
                OF,
                match m {
                    Shl | Rol => (result & (1 << (bits - 1)) != 0) ^ self.flag(CF),
                    Shr => value & (1 << (bits - 1)) != 0,
                    Ror => (result >> (bits - 2)) & 1 != (result >> (bits - 1)) & 1,
                    _ => false,
                },
            );
        }
        if !matches!(m, Rol | Ror) {
            self.zsp(result, width);
        }
        self.write(i, 0, result)
    }
    fn multiply(&mut self, i: &Instruction) -> Result<()> {
        let signed_mul = i.mnemonic() == Mnemonic::Imul;
        let (a, b, width) = if i.op_count() == 1 {
            let width = self.width(i, 0)?;
            (
                u64::from(self.registers[EAX]) & mask(width),
                self.read(i, 0)?,
                width,
            )
        } else if i.op_count() == 2 {
            (self.read(i, 0)?, self.read(i, 1)?, self.width(i, 0)?)
        } else {
            (self.read(i, 1)?, self.read(i, 2)?, self.width(i, 0)?)
        };
        ensure!(width <= 4, "native x86 multiply width exceeds 32 bits");
        let product = if signed_mul {
            signed(a, width).wrapping_mul(signed(b, width)) as u64
        } else {
            (a & mask(width)) * (b & mask(width))
        };
        let overflow = if signed_mul {
            signed(product, width) as u64 != product
        } else {
            product > mask(width)
        };
        self.set_flag(CF, overflow);
        self.set_flag(OF, overflow);
        if i.op_count() != 1 {
            self.write(i, 0, product)?;
        } else if width == 1 {
            self.set_register(Register::AX, product)?;
        } else {
            self.set_register(
                if width == 2 {
                    Register::AX
                } else {
                    Register::EAX
                },
                product,
            )?;
            self.set_register(
                if width == 2 {
                    Register::DX
                } else {
                    Register::EDX
                },
                product >> (width * 8),
            )?;
        }
        Ok(())
    }
    fn divide(&mut self, i: &Instruction) -> Result<()> {
        let width = self.width(i, 0)?;
        ensure!(width <= 4, "native x86 divide width exceeds 32 bits");
        let divisor = self.read(i, 0)? & mask(width);
        ensure!(divisor != 0, "native x86 division by zero");
        let dividend = if width == 1 {
            u64::from(self.registers[EAX] & 0xffff)
        } else {
            ((u64::from(self.registers[EDX]) & mask(width)) << (width * 8))
                | (u64::from(self.registers[EAX]) & mask(width))
        };
        let (quotient, remainder) = if i.mnemonic() == Mnemonic::Idiv {
            let dividend = signed(dividend, width * 2);
            let divisor = signed(divisor, width);
            let quotient = dividend
                .checked_div(divisor)
                .context("native x86 signed division overflow")?;
            ensure!(
                quotient == signed(quotient as u64, width),
                "native x86 quotient exceeds register width"
            );
            (quotient as u64, (dividend % divisor) as u64)
        } else {
            let quotient = dividend / divisor;
            ensure!(
                quotient <= mask(width),
                "native x86 quotient exceeds register width"
            );
            (quotient, dividend % divisor)
        };
        self.set_register(
            match width {
                1 => Register::AL,
                2 => Register::AX,
                _ => Register::EAX,
            },
            quotient,
        )?;
        self.set_register(
            match width {
                1 => Register::AH,
                2 => Register::DX,
                _ => Register::EDX,
            },
            remainder,
        )
    }
    fn string(&mut self, i: &Instruction) -> Result<()> {
        use Mnemonic::*;
        let m = i.mnemonic();
        let width = match m {
            Movsb | Stosb | Lodsb => 1,
            Movsw | Stosw | Lodsw => 2,
            _ => 4,
        };
        ensure!(
            i.op_kinds()
                .any(|kind| matches!(kind, OpKind::MemorySegESI | OpKind::MemoryESEDI))
                && i.op_kinds().all(|kind| matches!(
                    kind,
                    OpKind::MemorySegESI | OpKind::MemoryESEDI | OpKind::Register
                )),
            "native x86 string address size is unsupported"
        );
        ensure!(
            !matches!(i.segment_prefix(), Register::FS | Register::GS),
            "native x86 string segment is unsupported"
        );
        let repeated = i.has_rep_prefix() || i.has_repne_prefix();
        let count = if repeated { self.registers[ECX] } else { 1 };
        for _ in 0..count {
            self.charge()?;
            let value = if matches!(m, Stosb | Stosw | Stosd) {
                u64::from(self.registers[EAX])
            } else {
                self.memory.read(self.registers[ESI], width)?
            };
            if matches!(m, Lodsb | Lodsw | Lodsd) {
                self.set_register(
                    match width {
                        1 => Register::AL,
                        2 => Register::AX,
                        _ => Register::EAX,
                    },
                    value,
                )?;
            } else {
                self.memory.write(self.registers[EDI], width, value)?;
            }
            let step = if self.flag(DF) {
                0u32.wrapping_sub(width as u32)
            } else {
                width as u32
            };
            if !matches!(m, Stosb | Stosw | Stosd) {
                self.registers[ESI] = self.registers[ESI].wrapping_add(step);
            }
            if !matches!(m, Lodsb | Lodsw | Lodsd) {
                self.registers[EDI] = self.registers[EDI].wrapping_add(step);
            }
            if repeated {
                self.registers[ECX] -= 1;
            }
        }
        Ok(())
    }
}
