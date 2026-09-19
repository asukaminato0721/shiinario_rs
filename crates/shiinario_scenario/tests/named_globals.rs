use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

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
fn named(tag: u8, name: &str) -> Vec<u8> {
    let mut bytes = vec![tag];
    bytes.extend(name.as_bytes());
    bytes.push(0);
    bytes
}
fn assign(code: &mut Vec<u8>, value: u32, name: &str) {
    code.extend(instruction(
        0x38e,
        &[immediate(value), named(0x92, name)].concat(),
    ));
}
fn observe(code: &mut Vec<u8>, name: &str) {
    code.extend(instruction(0x49d, &named(0x12, name)));
}
fn execute(code: Vec<u8>) -> Vec<u32> {
    let length = code.len();
    let mut vm = BinaryVm::with_version("names.scn", code, EngineVersion::V2_36).unwrap();
    let mut values = Vec::new();
    for _ in 0..5000 {
        if vm.location().offset == length {
            return values;
        }
        if let Event::MouseButtonMapping { value, .. } = vm.step().unwrap() {
            values.push(value);
        }
    }
    panic!("script did not finish");
}

#[test]
fn global_fallback_matches_original_setter_and_getter() {
    // Captured by executing 428d40/428b00 from wana.EXE in Unicorn, supplying
    // only the variable tables and the three Win32 string imports.
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/named-globals-probe.json")).unwrap();
    let mut code = Vec::new();
    assign(&mut code, 38, "msg_no");
    observe(&mut code, "msg_no");
    assign(&mut code, 39, "msg_no");
    observe(&mut code, "msg_no");
    code.extend(instruction(0x3cf, b"\x01\x00\x12msg_no\0"));
    assign(&mut code, 7, "msg_no");
    observe(&mut code, "msg_no");
    assign(&mut code, 40, "msg_gno");
    observe(&mut code, "msg_gno");
    code.extend(instruction(0x3cf, &[0, 0]));
    observe(&mut code, "msg_no");
    assign(&mut code, 99, "MSG_NO");
    observe(&mut code, "msg_no");
    observe(&mut code, "MSG_NO");
    let expected: Vec<u32> =
        serde_json::from_value(fixture["v236"]["observations"].clone()).unwrap();
    assert_eq!(execute(code), expected);
}

#[test]
fn wana_mai_assignment_at_1847_without_local_declaration() {
    let mut target = immediate(0x1847);
    target[0] = 0x84;
    let mut code = instruction(0x258, &target);
    code.resize(0x1847, 0);
    // Exact failing instruction and its following assignment from WANA_MAI.SCN.
    code.extend(b"\x8e\x03\x04\x26\x00\x00\x00\x92msg_no\0");
    code.extend(b"\x8e\x03\x04\x26\x00\x00\x00\x92msg_gno\0");
    observe(&mut code, "msg_no");
    observe(&mut code, "msg_gno");
    assert_eq!(execute(code), [38, 38]);
}

#[test]
fn global_address_survives_updates_allocations_and_local_scope_release() {
    let mut code = Vec::new();
    assign(&mut code, 38, "msg_no");
    // Store &msg_no in a second auto-created global, then dereference it.
    code.extend(instruction(
        0x38f,
        &[named(0x52, "msg_no"), named(0x12, "pointer")].concat(),
    ));
    code.extend(instruction(0x3cf, b"\x01\x00\x12msg_no\0"));
    assign(&mut code, 8, "msg_no");
    code.extend(instruction(0x3cf, &[0, 0]));
    code.extend(instruction(
        0x38e,
        &[immediate(42), named(0x13, "pointer")].concat(),
    ));
    observe(&mut code, "msg_no");
    assign(&mut code, 43, "msg_no");
    code.extend(instruction(0x49d, &named(0x13, "pointer")));
    assert_eq!(execute(code), [42, 43]);
}

#[test]
fn platform_reply_creates_distinct_globals_and_coalesces_duplicate_names() {
    for names in [["x", "y"], ["x", "x"]] {
        let mut code = instruction(
            0x456,
            &[named(0x12, names[0]), named(0x92, names[1])].concat(),
        );
        observe(&mut code, names[0]);
        observe(&mut code, names[1]);
        let mut vm = BinaryVm::with_version("cursor.scn", code, EngineVersion::V2_36).unwrap();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::CursorPosition,
                ..
            }
        ));
        vm.respond_point([17, 29]).unwrap();
        for expected in if names[0] == names[1] {
            [29, 29]
        } else {
            [17, 29]
        } {
            assert!(
                matches!(vm.step().unwrap(), Event::MouseButtonMapping { value, .. } if value == expected)
            );
        }
    }
}

#[test]
fn globals_are_shared_across_tasks_and_scenarios() {
    let mut code = Vec::new();
    assign(&mut code, 38, "msg_no");
    code.extend(instruction(
        1,
        &[immediate(1), b"\x10child.scn\0".to_vec()].concat(),
    ));
    code.extend(instruction(0, &immediate(0)));
    let mut child = Vec::new();
    observe(&mut child, "msg_no");
    assign(&mut child, 39, "msg_no");
    // Switch the running child to another scenario without resetting globals.
    child.extend(instruction(
        1,
        &[immediate(1), b"\x10next.scn\0".to_vec()].concat(),
    ));
    let mut next = Vec::new();
    observe(&mut next, "msg_no");
    next.extend(instruction(0, &immediate(0)));
    let mut vm = BinaryVm::with_version("parent.scn", code, EngineVersion::V2_36).unwrap();
    let mut values = Vec::new();
    let mut ended = false;
    for _ in 0..100 {
        match vm.scheduled_step().unwrap() {
            Event::Platform {
                request: PlatformRequest::LoadScenario { name, .. },
                ..
            } => {
                vm.respond_scenario(if name == "child.scn" {
                    child.clone()
                } else {
                    next.clone()
                })
                .unwrap();
            }
            Event::SchedulerPoll => vm.respond(1).unwrap(),
            Event::MouseButtonMapping { value, .. } => values.push((vm.current_task(), value)),
            Event::End => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    assert!(ended);
    assert_eq!(values, [(1, 38), (1, 39)]);
}

#[test]
fn undefined_reads_indirection_and_v247_writes_remain_errors() {
    for (version, code) in [
        (
            EngineVersion::V2_36,
            instruction(0x49d, &named(0x12, "missing")),
        ),
        (
            EngineVersion::V2_36,
            instruction(0x38e, &[immediate(1), named(0x13, "missing")].concat()),
        ),
        (
            EngineVersion::V2_47,
            instruction(0x38e, &[immediate(1), named(0x92, "missing")].concat()),
        ),
    ] {
        let mut vm = BinaryVm::with_version("missing.scn", code, version).unwrap();
        assert!(
            vm.step()
                .unwrap_err()
                .to_string()
                .contains("undefined named variable")
        );
        assert_eq!(vm.location().offset, 0);
    }
}

#[test]
fn full_global_table_reports_an_error_at_the_failing_instruction() {
    let mut code = Vec::new();
    for i in 0..1000 {
        assign(&mut code, i, &format!("variable_{i}"));
    }
    let failure_offset = code.len();
    assign(&mut code, 1000, "one_too_many");
    let mut vm = BinaryVm::with_version("full.scn", code, EngineVersion::V2_36).unwrap();
    for _ in 0..1000 {
        vm.step().unwrap();
    }
    assert!(
        vm.step()
            .unwrap_err()
            .to_string()
            .contains("global variable table is full")
    );
    assert_eq!(vm.location().offset, failure_offset);
}
