use super::{Cpu, mask, signed};
use anyhow::Result;
use iced_x86::{Instruction, Mnemonic};

fn lanes(a: u64, b: u64, width: usize, operation: impl Fn(u64, u64) -> u64) -> u64 {
    let mut output = 0;
    for index in 0..8 / width {
        let shift = index * width * 8;
        output |= (operation((a >> shift) & mask(width), (b >> shift) & mask(width)) & mask(width))
            << shift;
    }
    output
}
impl Cpu<'_> {
    pub(super) fn simd(&mut self, i: &Instruction) -> Result<bool> {
        use Mnemonic::*;
        let m = i.mnemonic();
        if !matches!(
            m,
            Pxor | Por
                | Pand
                | Pandn
                | Paddb
                | Paddw
                | Paddd
                | Psubb
                | Psubw
                | Psubd
                | Pmullw
                | Pmulhw
                | Pmulhuw
                | Punpcklbw
                | Punpcklwd
                | Punpckldq
                | Punpckhbw
                | Punpckhwd
                | Punpckhdq
                | Packuswb
                | Packsswb
                | Packssdw
                | Psrlw
                | Psrld
                | Psrlq
                | Psllw
                | Pslld
                | Psllq
                | Psraw
                | Psrad
        ) {
            return Ok(false);
        }
        let a = self.read(i, 0)?;
        let b = self.read(i, 1)?;
        let result = match m {
            Pxor => a ^ b,
            Por => a | b,
            Pand => a & b,
            Pandn => (!a) & b,
            Paddb | Paddw | Paddd => lanes(
                a,
                b,
                match m {
                    Paddb => 1,
                    Paddw => 2,
                    _ => 4,
                },
                u64::wrapping_add,
            ),
            Psubb | Psubw | Psubd => lanes(
                a,
                b,
                match m {
                    Psubb => 1,
                    Psubw => 2,
                    _ => 4,
                },
                u64::wrapping_sub,
            ),
            Pmullw => lanes(a, b, 2, u64::wrapping_mul),
            Pmulhw => lanes(a, b, 2, |a, b| ((signed(a, 2) * signed(b, 2)) >> 16) as u64),
            Pmulhuw => lanes(a, b, 2, |a, b| (a * b) >> 16),
            Punpcklbw | Punpcklwd | Punpckldq | Punpckhbw | Punpckhwd | Punpckhdq => {
                let width = match m {
                    Punpcklbw | Punpckhbw => 1,
                    Punpcklwd | Punpckhwd => 2,
                    _ => 4,
                };
                let high = matches!(m, Punpckhbw | Punpckhwd | Punpckhdq);
                let mut output = 0;
                for index in 0..4 / width {
                    let from = (index + if high { 4 / width } else { 0 }) * width * 8;
                    let to = index * 2 * width * 8;
                    output |= ((a >> from) & mask(width)) << to;
                    output |= ((b >> from) & mask(width)) << (to + width * 8);
                }
                output
            }
            Packuswb | Packsswb | Packssdw => {
                let width = if m == Packssdw { 2 } else { 1 };
                let mut output = 0;
                for index in 0..8 / width {
                    let input = if index < 4 / width { a } else { b };
                    let from = (index % (4 / width)) * width * 16;
                    let value = signed((input >> from) & mask(width * 2), width * 2);
                    let value = if m == Packuswb {
                        value.clamp(0, 255)
                    } else {
                        let sign = 1i64 << (width * 8 - 1);
                        value.clamp(-sign, sign - 1)
                    };
                    output |= ((value as u64) & mask(width)) << (index * width * 8);
                }
                output
            }
            _ => {
                let width = match m {
                    Psrlw | Psllw | Psraw => 2,
                    Psrld | Pslld | Psrad => 4,
                    _ => 8,
                };
                lanes(a, 0, width, |value, _| {
                    if matches!(m, Psraw | Psrad) {
                        (signed(value, width) >> (b.min((width * 8 - 1) as u64) as u32)) as u64
                    } else if b >= width as u64 * 8 {
                        0
                    } else if matches!(m, Psllw | Pslld | Psllq) {
                        value << (b as u32)
                    } else {
                        value >> (b as u32)
                    }
                })
            }
        };
        self.write(i, 0, result)?;
        Ok(true)
    }
}
