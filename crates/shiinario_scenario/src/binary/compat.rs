//! Bounded replacements for the additional v2.48 interface operations.
use super::*;
use sha2::{Digest, Sha256};

impl BinaryVm {
    pub(super) fn effect_native_helper(
        &self,
        address: u32,
    ) -> Result<Option<(MemoryRange, Vec<u8>)>> {
        let matches = |length, digest: &str| {
            self.memory_read(address, length)
                .is_ok_and(|code| format!("{:x}", Sha256::digest(code)) == digest)
        };
        let copy = matches(
            69,
            "b88bd818e6b19cdbe8c5cbb3752218f325a494e6f27f0021c3e4b2a2a267ab41",
        );
        let invert = matches(
            49,
            "8ac6574da39cbf8f6ca928f3b4c06693a50ee8a7236fd2ba75f97568d4db5786",
        );
        if !copy && !invert {
            return Ok(None);
        }
        let argument = |index| -> Result<u32> { Ok(self.banks[&8][self.bank_index(8, index)?]) };
        let target = argument(0)?;
        let words = argument(if copy { 3 } else { 1 })? as usize;
        ensure!(
            words > 0 && words <= 4 * 1024 * 1024,
            "native XOR word count out of bounds"
        );
        let length = words * 4;
        let range = self.memory_range(target, length)?;
        let source = if copy { argument(4)? } else { target };
        let input = self.memory_read(source, length)?;
        let mask = if copy {
            let value = argument(5)?;
            let value = value | value.wrapping_shl(8);
            (value | value.wrapping_shl(16)).to_le_bytes()
        } else {
            [255; 4]
        };
        let mut output = vec![0; length];
        // The original copies forward. Earlier writes remain visible to later
        // reads when the two validated regions overlap.
        for word in 0..words {
            let value: [u8; 4] = std::array::from_fn(|byte| {
                let offset = word * 4 + byte;
                let at = u64::from(source) + offset as u64;
                let previous = at.checked_sub(u64::from(target));
                let value = match previous {
                    Some(index) if index < (word * 4) as u64 => output[index as usize],
                    _ => input[offset],
                };
                value ^ mask[byte]
            });
            output[word * 4..word * 4 + 4].copy_from_slice(&value);
        }
        Ok(Some((range, output)))
    }

    /// Store CP932 family names in the original 32-byte record format.
    pub fn respond_font_names(&mut self, names: &[String]) -> Result<u32> {
        ensure!(
            matches!(
                &self.pending,
                Some((
                    Event::Platform {
                        request: PlatformRequest::EnumerateFonts { .. },
                        ..
                    },
                    _
                ))
            ),
            "no pending font enumeration"
        );
        let mut bytes = Vec::new();
        let mut unique = std::collections::BTreeSet::new();
        for name in names {
            let (encoded, _, bad) = encoding_rs::SHIFT_JIS.encode(name);
            if !bad && !encoded.is_empty() && encoded.len() < 32 && unique.insert(encoded.to_vec())
            {
                ensure!(bytes.len() < 1024 * 1024, "font enumeration exceeds 1 MiB");
                let start = bytes.len();
                bytes.resize(start + 32, 0);
                bytes[start..start + encoded.len()].copy_from_slice(&encoded);
            }
        }
        bytes.resize(bytes.len() + 32, 0);
        let address = self.allocation_address(bytes.len())?;
        self.respond(address)?;
        self.allocations.insert(address, bytes);
        if let Some(old) = self.font_allocation.replace(address) {
            self.allocations.remove(&old);
        }
        Ok(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operand(value: u32) -> Vec<u8> {
        [vec![4], value.to_le_bytes().to_vec()].concat()
    }
    fn instruction(opcode: u16, args: &[u8]) -> Vec<u8> {
        [opcode.to_le_bytes().to_vec(), args.to_vec()].concat()
    }

    #[test]
    fn known_native_helpers_check_fingerprints_ranges_and_forward_overlap() -> Result<()> {
        let copy = include_bytes!("../../tests/fixtures/hanaoto-xor-copy.bin");
        let invert = include_bytes!("../../tests/fixtures/hanaoto-invert.bin");
        for (code, copying) in [(copy.as_slice(), true), (invert.as_slice(), false)] {
            for displacement in [0, 1, 4, 7] {
                let address = 0x2000_0000;
                let helper = 0x2100_0000;
                let mut vm = BinaryVm::new("helper.scn", instruction(0x0276, &operand(helper)))?;
                vm.allocations.insert(helper, code.to_vec());
                vm.allocations.insert(address, (0..32u8).collect());
                vm.sp = CELLS - 6;
                let mask = 0x1234_56a5;
                let args = if copying {
                    [address + displacement, 0, 0, 4, address, mask]
                } else {
                    [address + displacement, 4, 0, 0, 0, 0]
                };
                vm.banks.get_mut(&8).unwrap()[vm.sp..].copy_from_slice(&args);
                let before = vm.allocations[&address].clone();
                let mut expected = before.clone();
                let expanded =
                    mask | mask.wrapping_shl(8) | mask.wrapping_shl(16) | mask.wrapping_shl(24);
                for word in 0..4 {
                    let start = if copying {
                        word * 4
                    } else {
                        displacement as usize + word * 4
                    };
                    let input = u32::from_le_bytes(expected[start..start + 4].try_into()?);
                    let value = input ^ if copying { expanded } else { u32::MAX };
                    let at = displacement as usize + word * 4;
                    expected[at..at + 4].copy_from_slice(&value.to_le_bytes());
                }
                let banks = vm.banks.clone();
                vm.step()?;
                assert_eq!(vm.allocations[&address], expected);
                assert_eq!(vm.banks, banks);
                assert_eq!(vm.sp, CELLS - 6);
                for invalid in [0, 1, 2] {
                    let mut vm =
                        BinaryVm::new("invalid-helper.scn", instruction(0x0276, &operand(helper)))?;
                    let mut bytes = code.to_vec();
                    if invalid == 0 {
                        bytes[20] ^= 1;
                    }
                    vm.allocations.insert(helper, bytes);
                    vm.allocations.insert(address, before.clone());
                    vm.sp = CELLS - 6;
                    let mut args = args;
                    if invalid == 1 {
                        args[if copying { 3 } else { 1 }] = 100;
                    }
                    if invalid == 2 {
                        args[if copying { 3 } else { 1 }] = 0;
                    }
                    vm.banks.get_mut(&8).unwrap()[vm.sp..].copy_from_slice(&args);
                    assert!(vm.step().is_err());
                    assert_eq!(vm.pc, 0);
                    assert_eq!(vm.allocations[&address], before);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn font_records_are_cp932_terminated_and_replaced_after_each_reply() -> Result<()> {
        let mut args = operand(128);
        args.extend([12, 0, 0]);
        let mut code = instruction(0x006f, &args);
        code.extend(instruction(0x006f, &args));
        let mut vm = BinaryVm::new("fonts.scn", code)?;
        assert!(matches!(
            vm.step()?,
            Event::Platform {
                request: PlatformRequest::EnumerateFonts { charset: 128 },
                ..
            }
        ));
        let address = vm.respond_font_names(&[
            "ＭＳ ゴシック".into(),
            "Arial".into(),
            "Arial".into(),
            "😀".into(),
        ])?;
        let records = vm.memory_read(address, 96)?;
        let (encoded, _, bad) = encoding_rs::SHIFT_JIS.encode("ＭＳ ゴシック");
        assert!(!bad);
        assert_eq!(&records[..encoded.len()], encoded.as_ref());
        assert_eq!(&records[32..38], b"Arial\0");
        assert_eq!(&records[64..], &[0; 32]);
        assert_eq!(vm.banks[&12][0], address);
        vm.step()?;
        let replacement = vm.respond_font_names(&[])?;
        assert_eq!(vm.memory_read(replacement, 32)?, vec![0; 32]);
        assert!(vm.memory_read(address, 1).is_err());
        Ok(())
    }

    #[test]
    fn waits_keep_the_request_pending_until_a_valid_completion() -> Result<()> {
        let code = instruction(0x03eb, &operand(1));
        let mut vm = BinaryVm::new("wait.scn", code)?;
        let event = vm.step()?;
        assert!(matches!(
            event,
            Event::Platform {
                request: PlatformRequest::WaitControl { mask: 1 },
                ..
            }
        ));
        assert!(vm.respond(2).is_err());
        assert_eq!(vm.step()?, event);
        vm.respond(0)?;
        assert_eq!(vm.step()?, event);
        vm.respond(1)?;
        assert!(vm.pending.is_none());
        Ok(())
    }

    #[test]
    fn gdi_dll_calls_use_typed_requests_and_reject_unknown_symbols() -> Result<()> {
        let cases = [
            (
                "CreateSolidBrush",
                vec![0x123456],
                GdiCommand::CreateSolidBrush { color: 0x123456 },
            ),
            (
                "CreatePen",
                vec![0, 1, 0x123456],
                GdiCommand::CreatePen {
                    style: 0,
                    width: 1,
                    color: 0x123456,
                },
            ),
            (
                "SelectObject",
                vec![0xe0000003, 123],
                GdiCommand::SelectObject {
                    dc: 0xe0000003,
                    object: 123,
                },
            ),
            (
                "DeleteObject",
                vec![123],
                GdiCommand::DeleteObject { object: 123 },
            ),
            (
                "Ellipse",
                vec![0xe0000003, 1, 2, 3, 4],
                GdiCommand::Ellipse {
                    dc: 0xe0000003,
                    rect: [1, 2, 3, 4],
                },
            ),
        ];
        let encode = |symbol: &str, values: &[u32]| {
            let mut args = operand(1);
            args.extend(b"\x10GDI32.DLL\0\x10");
            args.extend(symbol.as_bytes());
            args.push(0);
            args.extend(values.iter().flat_map(|v| operand(*v)));
            args.extend([255, 12, 0, 0]);
            instruction(0x01a4, &args)
        };
        for (symbol, args, expected) in cases {
            let mut code = encode(symbol, &args);
            let next = code.len();
            code.extend(instruction(0x049d, &operand(42)));
            let mut vm = BinaryVm::new("gdi.scn", code)?;
            assert!(
                matches!(vm.step()?, Event::Platform { request: PlatformRequest::Gdi(command), .. } if command == expected)
            );
            vm.respond(777)?;
            assert_eq!(vm.pc, next);
            assert_eq!(vm.banks[&12][0], 777);
            assert!(matches!(
                vm.step()?,
                Event::MouseButtonMapping { value: 42, .. }
            ));
        }
        for (symbol, args) in [
            ("CreateSolidBrush", vec![]),
            ("CreatePen", vec![1]),
            ("Unknown", vec![]),
        ] {
            let mut vm = BinaryVm::new("invalid-gdi.scn", encode(symbol, &args))?;
            assert!(vm.step().is_err());
            assert_eq!(vm.pc, 0);
        }
        Ok(())
    }

    #[test]
    fn operand_metadata_covers_original_effect_and_movie_boundaries() {
        for (opcode, count) in [
            (0x04fb, 9),
            (0x04fc, 9),
            (0x055f, 10),
            (0x0566, 12),
            (0x056c, 5),
            (0x05c9, 3),
            (0x05ca, 1),
            (0x05cb, 1),
            (0x05cc, 1),
            (0x05cd, 2),
            (0x05d3, 3),
            (0x05d4, 4),
            (0x05d6, 2),
        ] {
            assert_eq!(
                scan::instruction_layout(opcode),
                Some(scan::Layout::Operands(count))
            );
        }
        for opcode in 0x0302..=0x030d {
            assert_eq!(
                scan::instruction_layout(opcode),
                Some(scan::Layout::Operands(if opcode < 0x0308 { 2 } else { 3 }))
            );
        }
    }
}
