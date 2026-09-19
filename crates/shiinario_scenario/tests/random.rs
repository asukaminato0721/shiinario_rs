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
fn random(code: &mut Vec<u8>, bound: u32, destination: &[u8]) {
    let mut operands = immediate(bound);
    operands.extend(destination);
    code.extend(instruction(0x3ac, &operands));
}
fn observe(code: &mut Vec<u8>, source: &[u8]) {
    code.extend(instruction(0x49d, source));
}
fn execute(version: EngineVersion, code: Vec<u8>) -> Vec<u32> {
    let length = code.len();
    let mut vm = BinaryVm::with_version("random.scn", code, version).unwrap();
    let mut values = Vec::new();
    while vm.location().offset < length {
        if let Event::MouseButtonMapping { value, .. } = vm.step().unwrap() {
            values.push(value);
        }
    }
    assert_eq!(vm.location().offset, length);
    values
}

#[test]
fn bounded_random_matches_original_helpers_and_crt_sequence() {
    // Unicorn executed both original opcode helpers. Only operand access and
    // CRT TLS were supplied; the original v2.47 rand arithmetic generated the
    // shared CRT sequence (v2.36 imports MSVCRT rand).
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/random-probe.json")).unwrap();
    for (index, version) in [EngineVersion::V2_36, EngineVersion::V2_47]
        .into_iter()
        .enumerate()
    {
        for case in fixture["versions"][index]["cases"].as_array().unwrap() {
            let seed = case["seed"].as_u64().unwrap() as u32;
            let mut code = instruction(0x3ae, &immediate(seed));
            for bound in case["bounds"].as_array().unwrap() {
                random(&mut code, bound.as_u64().unwrap() as u32, &[12, 0, 0]);
                observe(&mut code, &[12, 0, 0]);
            }
            // Query the explicit seed after generating: it must not return
            // the evolving internal state.
            code.extend(instruction(0x3ad, &[12, 0, 0]));
            observe(&mut code, &[12, 0, 0]);
            let mut expected: Vec<u32> = serde_json::from_value(case["outputs"].clone()).unwrap();
            expected.push(seed);
            assert_eq!(execute(version, code), expected, "{version:?}, seed={seed}");
        }
    }
}

#[test]
fn default_sequence_reseeding_and_load_effect_stack_operands() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        let mut code = instruction(0x3ad, &[12, 0, 0]);
        observe(&mut code, &[12, 0, 0]);
        random(&mut code, u32::MAX, &[12, 0, 0]);
        observe(&mut code, &[12, 0, 0]);
        // A discarded destination still consumes one random number.
        random(&mut code, 1, &immediate(0));
        code.extend(instruction(0x3cf, b"\x01\x00\x12result\0"));
        random(&mut code, u32::MAX, b"\x12result\0");
        observe(&mut code, b"\x12result\0");
        code.extend(instruction(0x3ae, &immediate(1)));
        // Match efclib.scn:4470 exactly: bound Z005, result Z003, SP=976.
        for _ in 0..24 {
            code.extend(instruction(0x2f8, &immediate(0)));
        }
        let mut assign = immediate(7);
        assign.extend([8, 5, 0]);
        code.extend(instruction(0x38e, &assign));
        code.extend([0xac, 0x03, 0x08, 0x05, 0x00, 0x08, 0x03, 0x00]);
        observe(&mut code, &[8, 3, 0]);
        assert_eq!(execute(version, code), [0, 41, 6334, 6]);
    }
}

#[test]
fn truncated_random_destination_stops_at_the_instruction() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        let mut code = instruction(0x3ac, &immediate(7));
        code.extend([12, 0]);
        let mut vm = BinaryVm::with_version("bad.scn", code, version).unwrap();
        assert!(vm.step().is_err());
        assert_eq!(vm.location().offset, 0);
        assert!(vm.step().is_err());
    }
}
