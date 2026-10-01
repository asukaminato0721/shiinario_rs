use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

fn immediate(n: u32) -> Vec<u8> {
    let mut bytes = vec![4];
    bytes.extend(n.to_le_bytes());
    bytes
}
fn instruction(op: u16, args: &[u8]) -> Vec<u8> {
    let mut bytes = op.to_le_bytes().to_vec();
    bytes.extend(args);
    bytes
}

#[test]
fn program_info_dispatches_by_version() {
    for (version, expected) in [
        (EngineVersion::V2_36, [236, 20050900]),
        (EngineVersion::V2_47, [247, 20090401]),
        (EngineVersion::V2_48, [248, 20101101]),
        (EngineVersion::V2_49, [249, 20110301]),
    ] {
        let mut code = instruction(0x3c0, &[12, 0, 0, 12, 1, 0]);
        for index in 0..2 {
            code.extend(instruction(0x49d, &[12, index, 0]));
        }
        let mut vm = BinaryVm::with_version("version.scn", code, version).unwrap();
        vm.step().unwrap();
        for value in expected {
            vm.step().unwrap();
            assert_eq!(vm.mouse_mapping().value, value);
        }
    }
}

#[test]
fn image_draw_operands_preserve_the_next_instruction() {
    for (version, flags, tail, extra) in [
        (EngineVersion::V2_36, 0, vec![], 0),
        (EngineVersion::V2_36, 0x40000000, vec![0], 0),
        (
            EngineVersion::V2_36,
            0x20000000,
            vec![0, 0x112, 0x234, 0x356],
            0x563412,
        ),
        (EngineVersion::V2_47, 0, vec![123], 123),
        (EngineVersion::V2_48, 0, vec![123], 123),
        (EngineVersion::V2_49, 0, vec![123], 123),
    ] {
        let args: Vec<u8> = [1, 2, flags, 3, 4, 5, 6]
            .into_iter()
            .chain(tail)
            .flat_map(immediate)
            .collect();
        let mut code = instruction(0x4bd, &args);
        let next = code.len();
        code.extend(instruction(0x4c4, &immediate(0)));
        let mut vm = BinaryVm::with_version("draw.scn", code, version).unwrap();
        vm.step().unwrap();
        assert_eq!(vm.location().offset, next);
        let Event::Platform {
            request: PlatformRequest::DrawImages { items, .. },
            ..
        } = vm.step().unwrap()
        else {
            panic!("expected draw request")
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].extra, [6, extra]);
        assert_eq!([items[0].x, items[0].y], [4, 5]);
    }
}

#[test]
fn clipped_draw_list_consumes_rectangle_before_next_instruction() {
    let args: Vec<_> = [0, u32::MAX, 2, 600, 400]
        .into_iter()
        .flat_map(immediate)
        .collect();
    let mut code = instruction(0x4c5, &args);
    code.extend(instruction(0x49d, &immediate(7)));
    let mut vm = BinaryVm::with_version("clip.scn", code, EngineVersion::V2_49).unwrap();
    assert!(matches!(
        vm.step().unwrap(),
        Event::Platform {
            request: PlatformRequest::DrawImages {
                clip: Some([-1, 2, 600, 400]),
                ..
            },
            ..
        }
    ));
    vm.respond(1).unwrap();
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 7);
}

#[test]
fn local_builtin_tracks_scope_depth_and_allows_shadowing() {
    let read = instruction(0x49d, b"\x12__LOCAL__\0");
    let mut code = read.clone();
    code.extend(instruction(0x3cf, b"\x01\x00\x12a\0"));
    code.extend(&read);
    code.extend(instruction(0x3cf, b"\x01\x00\x12__LOCAL__\0"));
    code.extend(&read);
    code.extend(instruction(0x3cf, &[0, 0]));
    code.extend(&read);
    code.extend(instruction(0x3cf, &[0, 0]));
    code.extend(&read);
    let mut vm = BinaryVm::new("local.scn", code).unwrap();
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 0);
    for expected in [1, 0, 1, 0] {
        vm.step().unwrap();
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping().value, expected);
    }
}

#[test]
fn button_query_waits_for_a_pair_reply() {
    let mut code = instruction(0x458, &[12, 0, 0, 12, 1, 0]);
    code.extend(instruction(0x49d, &[12, 0, 0]));
    code.extend(instruction(0x49d, &[12, 1, 0]));
    let mut vm = BinaryVm::new("buttons.scn", code).unwrap();
    let event = vm.step().unwrap();
    assert!(matches!(
        event,
        Event::Platform {
            request: PlatformRequest::MouseButtons,
            ..
        }
    ));
    assert_eq!(vm.step().unwrap(), event);
    assert!(vm.respond(1).is_err());
    vm.respond_point([1, 0]).unwrap();
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 1);
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 0);
}

#[test]
fn message_wait_blocks_until_its_timeout_without_advancing() {
    let mut code = instruction(0x7bf, &immediate(10));
    code.extend(instruction(0x49d, &immediate(7)));
    let mut vm = BinaryVm::with_version("wait.scn", code, EngineVersion::V2_36).unwrap();
    assert!(matches!(
        vm.step().unwrap(),
        Event::Platform {
            request: PlatformRequest::ClockMilliseconds,
            ..
        }
    ));
    vm.respond(100).unwrap();
    let wait = vm.step().unwrap();
    assert!(matches!(
        wait,
        Event::Platform {
            request: PlatformRequest::WaitTaskTimer {
                epoch: 100,
                duration: 10
            },
            ..
        }
    ));
    vm.respond(0).unwrap();
    assert_eq!(vm.step().unwrap(), wait);
    assert_eq!(vm.mouse_mapping().value, 0);
    vm.respond(1).unwrap();
    vm.step().unwrap();
    assert_eq!(vm.mouse_mapping().value, 7);
}

#[test]
fn system_metric_query_waits_for_reply_and_preserves_the_following_operand() {
    for reply in [0, 1] {
        let mut args = immediate(23);
        args.extend([12, 0, 0]);
        let query = instruction(0x9dd, &args);
        let mut code = query.clone();
        code.extend(instruction(0x49d, &[12, 0, 0]));
        let mut vm = BinaryVm::with_version("config.scn", code, EngineVersion::V2_49).unwrap();
        let event = vm.step().unwrap();
        assert!(matches!(
            event,
            Event::Platform {
                request: PlatformRequest::SystemMetric { index: 23 },
                ..
            }
        ));
        assert_eq!(vm.step().unwrap(), event);
        vm.respond(reply).unwrap();
        assert_eq!(vm.location().offset, query.len());
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping().value, reply);
        for length in 2..query.len() {
            let mut truncated = BinaryVm::with_version(
                "config.scn",
                query[..length].to_vec(),
                EngineVersion::V2_49,
            )
            .unwrap();
            assert!(truncated.step().is_err());
            assert_eq!(truncated.location().offset, 0);
        }
    }
}

#[test]
fn directory_dll_call_is_translated_and_other_calls_are_rejected() {
    for symbol in ["CreateDirectoryA", "DeleteFileA"] {
        let mut args = immediate(1);
        args.extend(b"\x10kernel32.dll\0\x10");
        args.extend(symbol.as_bytes());
        args.extend(b"\0\x10save\0");
        args.extend(immediate(0));
        args.extend([0xff, 12, 0, 0]);
        let mut code = instruction(0x1a4, &args);
        code.extend(instruction(0x49d, &[12, 0, 0]));
        let mut vm = BinaryVm::new("directory.scn", code).unwrap();
        if symbol == "DeleteFileA" {
            assert!(
                vm.step()
                    .unwrap_err()
                    .to_string()
                    .contains("unsupported DLL call")
            );
            continue;
        }
        assert!(
            matches!(vm.step().unwrap(), Event::Platform { request: PlatformRequest::CreateDirectory { path }, .. } if path == "save")
        );
        vm.respond(1).unwrap();
        vm.step().unwrap();
        assert_eq!(vm.mouse_mapping().value, 1);
    }
}
