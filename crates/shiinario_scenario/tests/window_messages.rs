use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

fn instruction(op: u16, values: &[u32]) -> Vec<u8> {
    let mut code = op.to_le_bytes().to_vec();
    for value in values {
        code.push(4);
        code.extend(value.to_le_bytes());
    }
    code
}

#[test]
fn post_message_reads_exact_error_bytes_and_preserves_next_instruction() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        let mut code = instruction(0x7bc, &[0, 0x10, 0, 0]);
        assert_eq!(code.len(), 22);
        code.extend(instruction(0x49d, &[42]));
        let mut vm = BinaryVm::with_version("close.scn", code, version).unwrap();
        assert!(matches!(
            vm.step().unwrap(),
            Event::Platform {
                request: PlatformRequest::PostWindowMessage {
                    window: 0,
                    message: 0x10,
                    wparam: 0,
                    lparam: 0
                },
                ..
            }
        ));
        // Posting itself is asynchronous: the request must be delivered.
        vm.respond(1).unwrap();
        assert_eq!(vm.location().offset, 22);
        assert!(matches!(
            vm.step().unwrap(),
            Event::MouseButtonMapping { value: 42, .. }
        ));
        vm.window_message(0x10, 0, 0);
        assert_eq!(vm.scheduled_step().unwrap(), Event::End);
        assert_eq!(vm.scheduled_step().unwrap(), Event::End);
    }
}

#[test]
fn close_callback_can_consume_message_or_allow_normal_exit() {
    for version in [EngineVersion::V2_36, EngineVersion::V2_47] {
        for result in [0, 1] {
            let mut code = instruction(0x000c, &[10]);
            code.push(0x84); // scenario-relative callback address
            let patch = code.len();
            code.extend([0; 4]);
            code.extend(instruction(0x07e4, &[10]));
            code.extend(instruction(0x49d, &[42]));
            code.extend(instruction(0x0000, &[0]));
            let callback = code.len() as u32;
            code[patch..patch + 4].copy_from_slice(&callback.to_le_bytes());
            // Read message argument _Z001 without disturbing the suspended task.
            code.extend([0x9d, 0x04, 8, 1, 0]);
            code.extend(instruction(0x0000, &[result]));
            let mut vm = BinaryVm::with_version("callback.scn", code, version).unwrap();
            vm.step().unwrap(); // define
            vm.step().unwrap(); // register
            vm.window_message(0x10, 11, 22);
            assert!(matches!(
                vm.scheduled_step().unwrap(),
                Event::MouseButtonMapping { value: 0x10, .. }
            ));
            vm.scheduled_step().unwrap(); // callback return
            let mut resumed = false;
            for _ in 0..20 {
                match vm.scheduled_step().unwrap() {
                    Event::SchedulerPoll => vm.respond(1).unwrap(),
                    Event::End => break,
                    Event::MouseButtonMapping { value: 42, .. } => {
                        resumed = true;
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(resumed, result == 1, "{version:?}");
        }
    }
}

#[test]
fn truncated_post_preserves_pc_and_reports_error() {
    let mut bytes = instruction(0x7bc, &[0, 0x10, 0, 0]);
    bytes.pop();
    let mut vm = BinaryVm::new("bad.scn", bytes).unwrap();
    assert!(vm.step().is_err());
    assert_eq!(vm.location().offset, 0);
}
