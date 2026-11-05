//! Bounded replacements for the additional v2.48 interface operations.
use super::*;

impl BinaryVm {
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
