use anyhow::{Result, bail};
use shiinario_core::EngineVersion;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

fn immediate(value: u32) -> Vec<u8> {
    let mut bytes = vec![4];
    bytes.extend(value.to_le_bytes());
    bytes
}

fn text_program(style: &[u8], text: &[u8], surface: u32) -> Vec<u8> {
    let mut code = 0x78u16.to_le_bytes().to_vec();
    code.extend(immediate(23));
    code.extend(immediate(41));
    for (opcode, surface, bytes) in [(0x84u16, u32::MAX, style), (0x83, surface, text)] {
        code.extend(opcode.to_le_bytes());
        code.extend(immediate(surface));
        code.push(0x10);
        code.extend(bytes);
        code.push(0);
    }
    code.extend(0u16.to_le_bytes());
    code.extend(immediate(0));
    code
}

// Retain requests, style snapshots, text ticks and scheduler polls, excluding
// only instruction locations (the inline strings differ in byte length).
fn execute(version: EngineVersion, style: &[u8], text: &[u8], surface: u32) -> Result<Vec<Event>> {
    let mut vm = BinaryVm::with_version("text.scn", text_program(style, text, surface), version)?;
    let mut events = Vec::new();
    let mut clock = 0;
    for _ in 0..500 {
        match vm.scheduled_step()? {
            Event::End => return Ok(events),
            Event::BinaryInstruction { .. } => {}
            Event::Platform {
                mut location,
                request,
            } => {
                // Reading an unacknowledged request must not advance text.
                assert!(matches!(vm.scheduled_step()?, Event::Platform { .. }));
                let reply = match &request {
                    PlatformRequest::ClockMilliseconds => {
                        clock += 10;
                        clock
                    }
                    PlatformRequest::DrawGlyph { .. } => 1,
                    other => bail!("unexpected request {other:?}"),
                };
                location.offset = 0;
                events.push(Event::Platform { location, request });
                vm.respond(reply)?;
            }
            Event::SchedulerPoll => {
                events.push(Event::SchedulerPoll);
                vm.respond(1)?;
            }
            event @ Event::TextTick { .. } => events.push(event),
            other => bail!("unexpected event {other:?}"),
        }
    }
    bail!("text did not finish")
}

#[test]
fn v236_n_preserves_glyphs_style_cursor_and_timing() {
    // Original v2.36 helper 425fc0 -> 42697b consumes 'n' without changing
    // the text context; unlike _r, this command does not move the cursor.
    for delay in [0, 10] {
        let style = format!("_A_c12,34,56_w{delay}");
        let with_noop = format!("_n{style}_n");
        for (text, with_n) in [
            (b"AB".as_slice(), b"_nA_nB_n".as_slice()),
            (b"A1!B", b"A_n1_n!B"),
            (b"A_rB", b"A_n_r_nB"),
            (b"_A_c12,34,56AB", b"_A_c12,34,56_nAB"),
            (b"\x82\xa0\x82\xa2", b"\x82\xa0_n\x82\xa2"),
            (b"__nB", b"_n__n_nB"),
            (b"", b"_n_n"),
        ] {
            for surface in [0, u32::MAX] {
                assert_eq!(
                    execute(EngineVersion::V2_36, with_noop.as_bytes(), with_n, surface).unwrap(),
                    execute(EngineVersion::V2_36, style.as_bytes(), text, surface).unwrap(),
                    "text={with_n:?}, delay={delay}, surface={surface}"
                );
            }
        }
    }
    let events = execute(EngineVersion::V2_36, b"_A", b"A_nB", 0).unwrap();
    let glyphs: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Platform {
                request:
                    PlatformRequest::DrawGlyph {
                        character,
                        position,
                        ..
                    },
                ..
            } => Some((*character, *position)),
            _ => None,
        })
        .collect();
    assert_eq!(glyphs, [('A', [23, 41]), ('B', [31, 41])]);
}

#[test]
fn v249_n_selects_default_context_without_changing_text_state() {
    // NUKI100.exe: 4385d4 -> 437890 -> 437670. An omitted index is zero;
    // selecting the active default context leaves layout and timing intact.
    for delay in [0, 10] {
        let style = format!("_A_c12,34,56_w{delay}");
        for surface in [0, u32::MAX] {
            for (plain, controls) in [
                (b"AB".as_slice(), b"_nA_nB_n".as_slice()),
                (b"AB", b"A_n0B"),
                (b"AB", b"A_n 0,B"),
                (b"AB", b"A_n+0.B"),
                (b"AB", b"A_n-0 B"),
                (b"AB", b"A_n0x0,B"),
                (b"A_rB", b"A_n_r_nB"),
                (b"A!B", b"A_n!B"),
                (b"__nB", b"_n__n_nB"),
                (b"", b"_n_n "),
                // Reproduce the reported text_byte=0xe with CP932 glyphs.
                (
                    b"\x82\xa0\x82\xa2\x82\xa4\x82\xa6\x82\xa8\x82\xa9\x82\xab\x82\xad",
                    b"\x82\xa0\x82\xa2\x82\xa4\x82\xa6\x82\xa8\x82\xa9\x82\xab_n\x82\xad",
                ),
            ] {
                assert_eq!(
                    execute(EngineVersion::V2_49, style.as_bytes(), controls, surface).unwrap(),
                    execute(EngineVersion::V2_49, style.as_bytes(), plain, surface).unwrap(),
                    "text={controls:?}, delay={delay}, surface={surface}"
                );
            }
            assert_eq!(
                execute(
                    EngineVersion::V2_49,
                    format!("_n0{style}_n").as_bytes(),
                    b"AB",
                    surface,
                )
                .unwrap(),
                execute(EngineVersion::V2_49, style.as_bytes(), b"AB", surface).unwrap(),
            );
        }
    }
}

#[test]
fn v249_n_rejects_unimplemented_contexts_and_expressions() {
    for control in [b"_n1".as_slice(), b"_n-1", b"_n0x10"] {
        for (style, text) in [(b"_A".as_slice(), control), (control, b"A".as_slice())] {
            let error = execute(EngineVersion::V2_49, style, text, 0).unwrap_err();
            assert!(format!("{error:#}").contains("nonzero inline text context"));
        }
    }
    let error = execute(EngineVersion::V2_49, b"_A", b"_n{0}A", 0).unwrap_err();
    assert!(format!("{error:#}").contains("number expressions are unresolved"));
    let error = execute(EngineVersion::V2_49, b"_A", b"_?", 0).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported text control _?"));
}

#[test]
fn n_is_version_specific_and_unknown_controls_still_fail() {
    // v2.47's 432466 parses a context number and switches contexts. Do not
    // silently apply v2.36's no-op semantics to the newer engine.
    for (style, text) in [(b"_A".as_slice(), b"_n".as_slice()), (b"_A_n", b"A")] {
        let error = execute(EngineVersion::V2_47, style, text, 0).unwrap_err();
        assert!(format!("{error:#}").contains("unsupported text control _n"));
    }
    let error = execute(EngineVersion::V2_36, b"_A", b"_?", 0).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported text control _?"));
}
