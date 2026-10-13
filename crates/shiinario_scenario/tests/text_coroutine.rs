//! Event digests recorded before the text coroutine replaced TextPhase.
use sha2::{Digest, Sha256};
use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

fn immediate(code: &mut Vec<u8>, value: u32) {
    code.push(4);
    code.extend(value.to_le_bytes());
}

struct Case {
    name: &'static str,
    style: &'static [u8],
    text: &'static [u8],
    surface: u32,
    input: u32,
    image: bool,
    digest: &'static str,
}

fn events(case: &Case, version: EngineVersion, start: u32) -> Vec<Event> {
    let mut code = Vec::new();
    code.extend(0x78u16.to_le_bytes());
    immediate(&mut code, 23);
    immediate(&mut code, 41);
    if case.image {
        code.extend(0xb4u16.to_le_bytes());
        immediate(&mut code, 17);
        immediate(&mut code, 2);
    }
    for (op, surface, bytes) in [
        (0x84u16, u32::MAX, case.style),
        (0x83, case.surface, case.text),
    ] {
        code.extend(op.to_le_bytes());
        immediate(&mut code, surface);
        code.push(0x10);
        code.extend(bytes);
        code.push(0);
    }
    code.extend(0u16.to_le_bytes());
    immediate(&mut code, 0);
    let mut vm = BinaryVm::with_version("coroutine.scn", code, version).unwrap();
    let mut result = Vec::new();
    let mut clock = start;
    let mut inputs = 0;
    for _ in 0..1000 {
        let event = vm.scheduled_step().unwrap();
        result.push(event.clone());
        match event {
            Event::End => return result,
            Event::SchedulerPoll => {
                clock = clock.wrapping_add(3);
                vm.respond(1).unwrap();
            }
            Event::Platform { request, .. } => {
                // A pending request must repeat without advancing the coroutine.
                assert_eq!(vm.scheduled_step().unwrap(), result.last().unwrap().clone());
                let value = match request {
                    PlatformRequest::ClockMilliseconds => {
                        clock = clock.wrapping_add(7);
                        clock
                    }
                    PlatformRequest::TextInput { clear: false } => {
                        inputs += 1;
                        if inputs == 2 { case.input } else { 0 }
                    }
                    PlatformRequest::TextInput { clear: true } => 0,
                    PlatformRequest::DrawGlyph { .. } | PlatformRequest::DrawImageGlyph { .. } => 1,
                    other => panic!("unexpected request: {other:?}"),
                };
                vm.respond(value).unwrap();
            }
            Event::BinaryInstruction { .. } | Event::TextTick { .. } => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
    panic!("text did not finish: {}", case.name);
}

#[test]
fn text_events_match_the_previous_scheduler_for_all_supported_versions() {
    let cases = [
        Case {
            name: "plain",
            style: b"_A",
            text: b"AB__$$",
            surface: 0,
            input: 0,
            image: false,
            digest: "e30fa1cdc2d6b91c03e696b93e0dc75e1872bef8f8293df689068dabf2e9c2ac",
        },
        Case {
            name: "timed",
            style: b"_A_w25",
            text: b"AB",
            surface: 0,
            input: 0,
            image: false,
            digest: "df86376de336793d33ea75db6c7d01986770de5362b4edacbb3e922ad44c574a",
        },
        Case {
            name: "controls",
            style: b"_A_l39",
            text: b"AB_rC_r!D_w25E_W0_c12,34,56F",
            surface: 0,
            input: 0,
            image: false,
            digest: "54572a87944791226227c8cabba3a4a3aa09b8cce9b39300999a8619c4e19958",
        },
        Case {
            name: "cp932",
            style: b"_l47",
            text: b"\x82\xa0\x82\xa2_r\x82\xa4",
            surface: 0,
            input: 0,
            image: false,
            digest: "d02205c39381a2ef082a85ba35ada8c037ea742b2ce51b9439888f5477d024fc",
        },
        Case {
            name: "image",
            style: b"_A_w25",
            text: b"AB",
            surface: u32::MAX,
            input: 0,
            image: true,
            digest: "94cba8338b5c6192f61a41fd9a1d69b06b2deccdfb09ca61570dddd78bb2de50",
        },
        Case {
            name: "invisible",
            style: b"_A_w25",
            text: b"AB",
            surface: u32::MAX,
            input: 0,
            image: false,
            digest: "5a0b8c82ce475db5b8b07b0babb9bee2711c6c026ce89cc08073a34619dac38c",
        },
        Case {
            name: "shadow",
            style: b"_A_e1_ca128_Sa64",
            text: b"AB",
            surface: 0,
            input: 0,
            image: false,
            digest: "574d64a6c53568d9dd492b4a8de5f28d7037d67c8750a50649c796ed0fe1cfc5",
        },
        Case {
            name: "skip1",
            style: b"_A_w25_s1",
            text: b"ABC",
            surface: 0,
            input: 0x20,
            image: false,
            digest: "372d53ae55adb4160db50de7e5518eb487759028f51a486e853797fb0f8ed0ee",
        },
        Case {
            name: "skip2",
            style: b"_A_w25_s2",
            text: b"ABC",
            surface: 0,
            input: 0x10,
            image: false,
            digest: "fd9650e74341a57dbf46ad8bfeb7571125eed4cc482c6d53f54c71b6fdd389f9",
        },
        Case {
            name: "skip4",
            style: b"_A_w25_s4",
            text: b"ABC",
            surface: 0,
            input: 0x100,
            image: false,
            digest: "d80ca0ce2585d03d593f721ca48c623e8bbf4815d21fc457029f9d8820f432af",
        },
        Case {
            name: "skip8",
            style: b"_A_w25_s8",
            text: b"ABC",
            surface: 0,
            input: 0x10000,
            image: false,
            digest: "c3f6177400bd1e39397e3bbefb8ee5cdff51f7bc5c4a2816e47a65280aa3ea7d",
        },
        Case {
            name: "empty",
            style: b"_A_s8",
            text: b"",
            surface: 0,
            input: 0,
            image: false,
            digest: "ff83d88c1227c6e8d9487bf4fb7dfa0d99fbf0f94d3ff089ac33c08a398ca763",
        },
    ];
    for case in cases {
        let mut hash = Sha256::new();
        for version in [
            EngineVersion::V2_36,
            EngineVersion::V2_47,
            EngineVersion::V2_48,
            EngineVersion::V2_49,
        ] {
            for start in [0, u32::MAX - 25] {
                let trace = events(&case, version, start);
                assert!(trace.iter().any(|event| matches!(
                    event,
                    Event::TextTick {
                        completed: true,
                        ..
                    }
                )));
                hash.update(serde_json::to_vec(&trace).unwrap());
            }
        }
        let digest = format!("{:x}", hash.finalize());
        assert_eq!(digest, case.digest, "{}", case.name);
    }
}
