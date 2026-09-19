use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event};

fn immediate(value: u32) -> Vec<u8> {
    let mut bytes = vec![4];
    bytes.extend(value.to_le_bytes());
    bytes
}
fn instruction(opcode: u16, operands: &[u8]) -> Vec<u8> {
    let mut bytes = opcode.to_le_bytes().to_vec();
    bytes.extend(operands);
    bytes
}
fn execute(version: EngineVersion, code: Vec<u8>) -> Vec<u32> {
    let length = code.len();
    let mut vm = BinaryVm::with_version("absolute.scn", code, version).unwrap();
    let mut observed = Vec::new();
    while vm.location().offset < length {
        if let Event::MouseButtonMapping { value, .. } = vm.step().unwrap() {
            observed.push(value);
        }
    }
    assert_eq!(vm.location().offset, length);
    observed
}

#[test]
fn signed_absolute_matches_both_original_helpers() {
    // Unicorn executes the original arithmetic and operand-pointer rewind.
    // Only operand reads/writes are supplied by the probe.
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/absolute-probe.json")).unwrap();
    for (index, version) in [EngineVersion::V2_36, EngineVersion::V2_47]
        .into_iter()
        .enumerate()
    {
        // Include task-local registers, shared registers, and the active stack.
        for destination in [&[12, 0, 0][..], &[10, 0, 0], &[8, 0, 0]] {
            let mut code = instruction(0x2f8, &immediate(0));
            let mut expected = Vec::new();
            for case in fixture["versions"][index]["cases"].as_array().unwrap() {
                let value = case["input"].as_u64().unwrap() as u32;
                code.extend(instruction(
                    0x38e,
                    &[immediate(value), destination.to_vec()].concat(),
                ));
                code.extend(instruction(0x3a2, destination));
                code.extend(instruction(0x49d, destination));
                expected.push(case["output"].as_u64().unwrap() as u32);
            }
            assert_eq!(
                execute(version, code),
                expected,
                "{version:?} {destination:?}"
            );
        }
    }
}

#[test]
fn failing_named_operand_and_following_multiply_stay_aligned() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        let mut code = instruction(0x3cf, b"\x01\x00\x12param\0");
        code.extend(instruction(
            0x38e,
            &[immediate((-38i32) as u32), b"\x12param\0".to_vec()].concat(),
        ));
        let mut target = immediate(0x21d19);
        target[0] = 0x84;
        code.extend(instruction(0x258, &target));
        code.resize(0x21d19, 0);
        // start.SCN:21d19 and the immediately following 039e instruction.
        code.extend(b"\xa2\x03\x12param\0\x9e\x03\x04\x04\x00\x00\x00\x12param\0");
        code.extend(instruction(0x49d, b"\x12param\0"));
        assert_eq!(execute(version, code), [152]);
    }
}

#[test]
fn indirect_operand_updates_pointee_and_immediate_is_discarded() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        let mut code = instruction(0x3cf, b"\x01\x00\x12pointer\0");
        code.extend(instruction(
            0x38e,
            &[immediate(u32::MAX), vec![12, 1, 0]].concat(),
        ));
        code.extend(instruction(0x38f, b"\x4c\x01\x00\x12pointer\0"));
        code.extend(instruction(0x3a2, b"\x13pointer\0"));
        code.extend(instruction(0x49d, &[12, 1, 0]));
        code.extend(instruction(0x3a2, &immediate(u32::MAX)));
        code.extend(instruction(0x49d, &immediate(42)));
        assert_eq!(execute(version, code), [1, 42]);
    }
}

#[test]
fn malformed_absolute_operands_report_the_original_instruction() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        for operand in [
            &[12, 0][..],
            b"\x12param",
            b"\x12undefined\0",
            &[12, 0xe8, 3],
        ] {
            let mut vm =
                BinaryVm::with_version("bad.scn", instruction(0x3a2, operand), version).unwrap();
            let error = vm.step().unwrap_err().to_string();
            assert!(error.contains("bad.scn:0x0"), "{error}");
            assert!(error.contains("opcode=0x03a2"), "{error}");
            assert_eq!(vm.location().offset, 0);
            assert_eq!(vm.step().unwrap_err().to_string(), error);
        }
    }
}
