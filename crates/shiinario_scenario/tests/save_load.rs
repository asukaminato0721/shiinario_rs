use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

fn immediate(n: u32) -> Vec<u8> {
    let mut b = vec![4];
    b.extend(n.to_le_bytes());
    b
}
fn instruction(op: u16, args: &[u8]) -> Vec<u8> {
    let mut b = op.to_le_bytes().to_vec();
    b.extend(args);
    b
}
fn relative(n: u32) -> Vec<u8> {
    let mut b = immediate(n);
    b[0] = 0x84;
    b
}

#[test]
fn mixed_returns_keep_pending_result_backing_until_return() {
    // Original menu code calls a 026c routine with 0281. Its result frame
    // survives until a later 0285, after the enclosing scope has been popped.
    let mut args = vec![12, 0, 0];
    args.extend(relative(64));
    args.extend([0, 0]);
    let mut code = instruction(0x281, &args);
    code.extend(instruction(0x49d, &immediate(19)));
    code.resize(64, 0);
    code.extend(instruction(0x3cf, b"\x01\x00\x12result\0"));
    let mut args = b"\x12result\0".to_vec();
    args.extend(relative(128));
    args.extend([0, 0]);
    code.extend(instruction(0x281, &args));
    code.extend(instruction(0x3cf, &[0, 0]));
    code.extend(instruction(0x285, &immediate(0x12345678)));
    code.resize(128, 0);
    code.extend(instruction(0x26c, &[]));
    let mut vm = BinaryVm::new("mixed.scn", code).unwrap();
    for _ in 0..7 {
        vm.step().unwrap();
    }
    assert_eq!(vm.mouse_mapping().value, 19);
}

#[test]
fn raw_write_waits_and_rejects_bad_ranges() {
    let mut args = relative(128);
    args.extend(immediate(4));
    args.extend(b"\x10save.dat\0");
    let mut code = instruction(0xd2, &args);
    code.extend(instruction(0x49d, &immediate(11)));
    code.resize(132, 7);
    let mut vm = BinaryVm::new("write.scn", code.clone()).unwrap();
    let request = vm.step().unwrap();
    assert!(matches!(
        request,
        Event::Platform {
            request: PlatformRequest::WriteFile { length: 4, .. },
            ..
        }
    ));
    assert_eq!(vm.file_write_bytes().unwrap(), [7; 4]);
    assert_eq!(vm.step().unwrap(), request);
    assert!(vm.respond(0).is_err());
    assert_eq!(vm.step().unwrap(), request);
    vm.respond(1).unwrap();
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 11);
    code.pop();
    let mut vm = BinaryVm::new("bad.scn", code).unwrap();
    assert!(vm.step().is_err());
    assert_eq!(vm.location().offset, 0);
}

#[test]
fn flag_files_preserve_byte_and_integer_banks() {
    for system in [false, true] {
        for encoded in [false, true] {
            let size = if system { 4000 } else { 5000 };
            let mut original: Vec<u8> = (0..size).map(|i| (i * 37 + 13) as u8).collect();
            let plain = original.clone();
            original.resize(size + 8, 0);
            if encoded {
                for b in &mut original[..size] {
                    *b ^= 0x9c ^ 0xf4;
                }
                original[size] = 0x9c ^ 0xb1;
                let sum: u32 = plain.iter().map(|b| u32::from(b ^ 0xd3)).sum();
                original[size + 4..].copy_from_slice(&(sum ^ 0x4bc57d21).to_le_bytes());
            }
            let mut code = instruction(0x136, &immediate(encoded as u32));
            code.extend(instruction(
                if system { 0x13a } else { 0x138 },
                b"\x10flags.dat\0",
            ));
            code.extend(instruction(
                if system { 0x13b } else { 0x139 },
                b"\x10flags.dat\0",
            ));
            let mut vm = BinaryVm::new("flags.scn", code).unwrap();
            vm.step().unwrap();
            vm.step().unwrap();
            assert!(vm.respond_flags(&original[..size + 7]).is_err());
            assert!(vm.respond(1).is_err());
            vm.respond_flags(&original).unwrap();
            vm.step().unwrap();
            let saved = vm.flag_write_bytes().unwrap();
            assert_eq!(saved.len(), size + 8);
            let decoded: Vec<u8> = saved[..size]
                .iter()
                .map(|b| {
                    if encoded {
                        b ^ saved[size] ^ 0xb1 ^ 0xf4
                    } else {
                        *b
                    }
                })
                .collect();
            assert_eq!(decoded, plain);
            if encoded {
                let sum: u32 = plain.iter().map(|b| u32::from(b ^ 0xd3)).sum();
                assert_eq!(&saved[size + 4..], &(sum ^ 0x4bc57d21).to_le_bytes());
            } else {
                assert_eq!(&saved[size..], &[0; 8]);
            }
        }
    }
}
