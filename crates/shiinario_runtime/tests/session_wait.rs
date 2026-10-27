use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_runtime::{
    resources::{AudioStream, Sound},
    session::{Host, Session, SessionState, Wake},
};
use shiinario_scenario::{Event, PlatformRequest, SoundCommand};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Game(PathBuf);
impl Game {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "shiinario-wait-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("RANDL_.exe"), []).unwrap();
        std::fs::write(
            root.join("fixture.war"),
            include_bytes!("../../shiinario_assets/tests/fixtures/minimal.war"),
        )
        .unwrap();
        let (ini, _, _) = encoding_rs::SHIFT_JIS.encode(
            "[椎名里緒 v2.47]\r\nWindowWidth=800\r\nWindowHeight=600\r\nArc=fixture.war\r\nScn=wait.scn\r\n",
        );
        std::fs::write(root.join("GAME.INI"), ini).unwrap();
        // Relative delay, change mouse mapping, then terminate the task.
        let mut code = Vec::new();
        for (opcode, value) in [(0x0028u16, 100u32), (0x049d, 42), (0, 0)] {
            code.extend(opcode.to_le_bytes());
            code.push(4);
            code.extend(value.to_le_bytes());
        }
        std::fs::write(root.join("wait.scn"), code).unwrap();
        Self(root)
    }
}
impl Drop for Game {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct TestHost {
    now: u32,
    invalidates: usize,
    updates: usize,
    requests: usize,
    polls: usize,
    messages: Vec<[u32; 3]>,
    scan_keys: Vec<u32>,
}
impl Host for TestHost {
    fn take_window_messages(&mut self) -> Vec<[u32; 3]> {
        std::mem::take(&mut self.messages)
    }
    fn simulated(&self) -> bool {
        true
    }
    fn poll(&mut self) -> Result<()> {
        self.polls += 1;
        Ok(())
    }
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        self.requests += 1;
        match request {
            PlatformRequest::SetWindowTitle { window: 0, .. } => Ok(1),
            PlatformRequest::FindWindow { .. } => Ok(0),
            PlatformRequest::ClockMilliseconds => Ok(self.now),
            PlatformRequest::ReadScanKey { scan } => Ok(if self.scan_keys.contains(scan) {
                0x80
            } else {
                0
            }),
            PlatformRequest::WaitTaskTimer { epoch, duration } => {
                Ok(u32::from(self.now.wrapping_sub(*epoch) >= *duration))
            }
            PlatformRequest::InvalidateRect { .. } => {
                self.invalidates += 1;
                Ok(1)
            }
            PlatformRequest::UpdateWindow => {
                self.updates += 1;
                Ok(1)
            }
            _ => bail!("unexpected request: {request:?}"),
        }
    }
    fn point(&mut self, _: &PlatformRequest) -> Result<[i32; 2]> {
        bail!("unexpected point request")
    }
    fn install_sound(&mut self, _: u32, _: Arc<Sound>) -> Result<()> {
        bail!("unexpected sound installation")
    }
    fn sound_command(&mut self, _: u32, _: &SoundCommand) -> Result<u32> {
        bail!("unexpected sound command")
    }
    fn stop_stream(&mut self, _: u32) -> Result<()> {
        Ok(())
    }
    fn fade_stream(&mut self, _: u32, _: u32, _: i32, _: u32) -> Result<()> {
        bail!("unexpected stream fade")
    }
    fn play_stream(&mut self, _: u32, _: Arc<AudioStream>, _: u32) -> Result<()> {
        Ok(())
    }
}

#[test]
fn window_titles_use_story_encoding_before_the_first_story_load() {
    for (codec, story, title) in [
        (
            encoding_rs::BIG5,
            "善意的虛偽，勝於引起爭端的事實。",
            "花與乙女的祝福",
        ),
        (encoding_rs::SHIFT_JIS, "日本語の文章です。", "日本語の窓"),
        (encoding_rs::SHIFT_JIS, "ASCII only", "Example Game"),
    ] {
        let game = Game::new();
        let (sample, _, bad) = codec.encode(story);
        assert!(!bad);
        // The loose story overrides the synthetic archive's fixture.txt.
        std::fs::write(game.0.join("fixture.txt"), sample).unwrap();
        let (encoded_title, _, bad) = codec.encode(title);
        assert!(!bad);
        if codec == encoding_rs::BIG5 {
            // The title alone is valid CP932 as well. A per-title fallback
            // cannot detect this game's encoding without a story sample.
            assert!(
                encoding_rs::SHIFT_JIS
                    .decode_without_bom_handling_and_without_replacement(&encoded_title)
                    .is_some()
            );
        }
        let mut code = 0x07a8u16.to_le_bytes().to_vec();
        code.extend([4, 0, 0, 0, 0, 0x10]);
        code.extend_from_slice(&encoded_title);
        code.push(0);
        code.extend(0x07b2u16.to_le_bytes());
        code.extend(b"\x10ShiinaRio\0\x10");
        code.extend_from_slice(&encoded_title);
        code.extend([0, 12, 0, 0]);
        code.extend([0, 0, 4, 0, 0, 0, 0]);
        std::fs::write(game.0.join("wait.scn"), code).unwrap();
        let project = Project::open(&game.0).unwrap();
        let mut session = Session::new(&project, "wait.scn").unwrap();
        let mut host = TestHost::default();
        let mut titles = Vec::new();
        let ended = (0..100).any(|_| {
            !session
                .step(&project, &mut host, |event| {
                    if let Event::Platform { request, .. } = event {
                        match request {
                            PlatformRequest::SetWindowTitle { title, .. }
                            | PlatformRequest::FindWindow { title, .. } => {
                                titles.push(title.clone())
                            }
                            PlatformRequest::LoadAsset { .. }
                            | PlatformRequest::BeginAssetLoad { .. }
                            | PlatformRequest::ReadAssetInto { .. } => {
                                panic!("title must precede story loading")
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })
                .unwrap()
        });
        assert!(ended);
        assert_eq!(titles, [title, title]);
    }
}

#[test]
fn physical_key_repeats_keep_separate_state_for_left_and_right_keys() {
    let game = Game::new();
    let mut code = Vec::new();
    for scan in [29u32, 157, 29, 157] {
        code.extend(0x03ecu16.to_le_bytes());
        code.push(4);
        code.extend(scan.to_le_bytes());
        code.extend([12, 0, 0]);
        code.extend(0x049du16.to_le_bytes());
        code.extend([12, 0, 0]);
    }
    code.extend(0u16.to_le_bytes());
    code.extend([4, 0, 0, 0, 0]);
    std::fs::write(game.0.join("wait.scn"), code).unwrap();
    let project = Project::open(&game.0).unwrap();
    let mut session = Session::new(&project, "wait.scn").unwrap();
    let mut host = TestHost {
        scan_keys: vec![29, 157],
        ..Default::default()
    };
    let mut values = Vec::new();
    let mut ended = false;
    for _ in 0..100 {
        if session
            .resume(&project, &mut host, |event| {
                if let Event::MouseButtonMapping { value, .. } = event {
                    values.push(*value);
                }
                Ok(())
            })
            .unwrap()
            == SessionState::Ended
        {
            ended = true;
            break;
        }
    }
    assert!(ended);
    assert_eq!(values, [0x80, 0x80, 0, 0]);
}

#[test]
fn window_update_retains_the_committed_damage_and_has_no_operands() {
    let game = Game::new();
    let mut code = Vec::new();
    for (opcode, values) in [
        (0x04d8u16, vec![0u32, 0, 0, 1, 1, 10, 20, 30]),
        (0x07d1, vec![]),
        (0x07d0, vec![0, 0, 1, 1]),
        (0x07d1, vec![]),
        (0, vec![0]),
    ] {
        code.extend(opcode.to_le_bytes());
        for value in values {
            code.push(4);
            code.extend(value.to_le_bytes());
        }
    }
    std::fs::write(game.0.join("wait.scn"), code).unwrap();
    let project = Project::open(&game.0).unwrap();
    let mut session = Session::new(&project, "wait.scn").unwrap();
    let mut host = TestHost::default();
    let mut ended = false;
    for _ in 0..100 {
        let state = session.resume(&project, &mut host, |_| Ok(())).unwrap();
        if host.updates == 1 && host.invalidates == 0 {
            assert_eq!(&session.surface(0).unwrap().rgba[..4], &[10, 20, 30, 255]);
            assert_eq!(&session.display().unwrap().rgba[..4], &[0, 0, 0, 255]);
        }
        if state == SessionState::Ended {
            ended = true;
            break;
        }
    }
    assert!(ended);
    assert_eq!((host.updates, host.invalidates), (2, 1));
    assert_eq!(&session.display().unwrap().rgba[..4], &[10, 20, 30, 255]);
}

#[test]
fn bitblt_copies_pixels_and_presents_only_the_screen_destination() {
    for raster_operation in [0x00cc_0020u32, 0x1234_5678] {
        let game = Game::new();
        let mut code = Vec::new();
        for (opcode, values) in [
            (0x04d8u16, vec![1, 0, 0, 1, 1, 10, 20, 30]),
            (0x0514, vec![0, 5, 6, 1, 1, 1, 0, 0, raster_operation]),
            (0x0514, vec![1, 2, 3, 1, 1, 0, 5, 6, 0x00cc_0020]),
            (0, vec![0]),
        ] {
            code.extend(opcode.to_le_bytes());
            for value in values {
                code.push(4);
                code.extend(value.to_le_bytes());
            }
        }
        std::fs::write(game.0.join("wait.scn"), code).unwrap();
        let project = Project::open(&game.0).unwrap();
        let mut session = Session::new(&project, "wait.scn").unwrap();
        let mut host = TestHost::default();
        let result = (|| -> Result<()> {
            for _ in 0..100 {
                if session.resume(&project, &mut host, |_| Ok(()))? == SessionState::Ended {
                    return Ok(());
                }
            }
            bail!("script did not end")
        })();
        let screen_pixel = (6 * 800 + 5) * 4;
        if raster_operation == 0x00cc_0020 {
            result.unwrap();
            assert_eq!(host.invalidates, 1);
            assert_eq!(
                &session.display().unwrap().rgba[screen_pixel..screen_pixel + 4],
                &[10, 20, 30, 255]
            );
            let offscreen = session.surface(1).unwrap();
            let index = (3 * 800 + 2) * 4;
            assert_eq!(&offscreen.rgba[index..index + 4], &[10, 20, 30, 255]);
        } else {
            assert!(
                format!("{:#}", result.unwrap_err())
                    .contains("unsupported BitBlt raster operation")
            );
            assert_eq!(host.invalidates, 0);
            assert_eq!(
                &session.surface(0).unwrap().rgba[screen_pixel..screen_pixel + 4],
                &[0, 0, 0, 255]
            );
        }
    }
}

#[test]
fn clear_surface_presents_only_surface_zero_and_sleep_waits_for_elapsed_time() {
    let game = Game::new();
    let imm = |value: u32| [vec![4], value.to_le_bytes().to_vec()].concat();
    let mut code = Vec::new();
    for id in [0u32, 1] {
        code.extend(0x04d8u16.to_le_bytes());
        for value in [id, 0, 0, 800, 600, 10, 20, 30] {
            code.extend(imm(value));
        }
    }
    for id in [1u32, 0] {
        code.extend(0x04ceu16.to_le_bytes());
        code.extend(imm(id));
    }
    code.extend(0x002au16.to_le_bytes());
    code.extend(imm(20));
    code.extend(0u16.to_le_bytes());
    code.extend(imm(0));
    std::fs::write(game.0.join("wait.scn"), code).unwrap();
    let project = Project::open(&game.0).unwrap();
    let mut session = Session::new(&project, "wait.scn").unwrap();
    let mut host = TestHost::default();
    let mut cleared = 0;
    for _ in 0..100 {
        let state = session
            .resume(&project, &mut host, |event| {
                if matches!(
                    event,
                    Event::Platform {
                        request: PlatformRequest::ClearSurface { .. },
                        ..
                    }
                ) {
                    cleared += 1;
                }
                Ok(())
            })
            .unwrap();
        if cleared == 1 {
            assert_eq!(host.invalidates, 0);
            assert_eq!(&session.surface(0).unwrap().rgba[..4], &[10, 20, 30, 255]);
            assert_eq!(&session.surface(1).unwrap().rgba[..4], &[0, 0, 0, 255]);
        }
        if matches!(state, SessionState::Waiting { .. }) {
            break;
        }
    }
    assert_eq!(cleared, 2);
    assert_eq!(host.invalidates, 1);
    assert_eq!(&session.display().unwrap().rgba[..4], &[0, 0, 0, 255]);
    host.now = 19;
    assert!(matches!(
        session.resume(&project, &mut host, |_| Ok(())).unwrap(),
        SessionState::Waiting { .. }
    ));
    host.now = 20;
    let mut ended = false;
    for _ in 0..10 {
        if session.resume(&project, &mut host, |_| Ok(())).unwrap() == SessionState::Ended {
            ended = true;
            break;
        }
    }
    assert!(ended);
}

#[test]
fn window_timers_repeat_coalesce_and_stop_after_cancellation() {
    for cancel in [false, true] {
        for epoch in [0u32, u32::MAX - 50] {
            let game = Game::new();
            let imm = |value: u32| [vec![4], value.to_le_bytes().to_vec()].concat();
            let op = |opcode: u16, args: Vec<u8>| [opcode.to_le_bytes().to_vec(), args].concat();
            let mut code = op(0xc, imm(10));
            code.push(0x84);
            let patch = code.len();
            code.extend([0; 4]);
            code.extend(op(0xaf0, imm(10)));
            code.extend(op(0xafa, [imm(123), imm(100), vec![12, 0, 0]].concat()));
            let target = code.len() as u32;
            code.extend(op(
                0x258,
                [vec![0x84], target.to_le_bytes().to_vec()].concat(),
            ));
            let callback = code.len() as u32;
            code[patch..patch + 4].copy_from_slice(&callback.to_le_bytes());
            if cancel {
                code.extend(op(0xafb, imm(123)));
            }
            code.extend(op(0x49d, imm(17)));
            code.extend(op(0, imm(0)));
            std::fs::write(game.0.join("wait.scn"), code).unwrap();
            let project = Project::open(&game.0).unwrap();
            let mut session = Session::new(&project, "wait.scn").unwrap();
            let mut host = TestHost {
                now: epoch,
                ..Default::default()
            };
            let mut callbacks = 0;
            for (elapsed, expected) in [
                (0, 0),
                (99, 0),
                (100, 1),
                (199, 1),
                (350, if cancel { 1 } else { 2 }),
            ] {
                host.now = epoch.wrapping_add(elapsed);
                for _ in 0..200 {
                    session
                        .resume(&project, &mut host, |event| {
                            if matches!(event, Event::MouseButtonMapping { value: 17, .. }) {
                                callbacks += 1;
                            }
                            Ok(())
                        })
                        .unwrap();
                }
                assert_eq!(
                    callbacks, expected,
                    "cancel={cancel}, epoch={epoch}, elapsed={elapsed}"
                );
            }
        }
    }
}

#[test]
fn registry_directories_are_nonempty_and_resolve_inside_the_project() {
    for savedata in [false, true] {
        let game = Game::new();
        if savedata {
            std::fs::create_dir(game.0.join("savedata")).unwrap();
        }
        // Allocate a 256-byte string buffer, then read installation paths.
        let mut code = b"\xbc\x02\x04\0\x01\0\0\x0c\0\0".to_vec();
        for name in ["DataPath", "InstPath"] {
            code.extend(b"\xfe\0\x10");
            code.extend(name.as_bytes());
            code.extend(b"\0\x0c\0\0");
            // Read the first byte that the reply stored in the VM buffer.
            code.extend(b"\x04\x03\x0c\0\0\x0c\x01\0");
            code.extend(b"\x9d\x04\x0c\x01\0");
        }
        code.extend(b"\0\0\x04\0\0\0\0");
        std::fs::write(game.0.join("wait.scn"), code).unwrap();
        let project = Project::open(&game.0).unwrap();
        let mut session = Session::new(&project, "wait.scn").unwrap();
        let mut host = TestHost::default();
        let mut paths = Vec::new();
        let mut exists = Vec::new();
        let mut ended = false;
        for _ in 0..100 {
            let state = session
                .resume(&project, &mut host, |event| {
                    match event {
                        Event::PlatformBytesReply { bytes, .. } => paths.push(bytes.clone()),
                        Event::MouseButtonMapping { value, .. } => exists.push(*value),
                        _ => {}
                    }
                    Ok(())
                })
                .unwrap();
            if state == SessionState::Ended {
                ended = true;
                break;
            }
        }
        assert!(ended);
        assert_eq!(
            paths,
            [
                if savedata {
                    b"savedata\\".as_slice()
                } else {
                    b".\\"
                },
                b".\\"
            ]
        );
        assert_eq!(
            exists,
            [
                if savedata { b's' as u32 } else { b'.' as u32 },
                b'.' as u32
            ]
        );
        for path in paths {
            let path = String::from_utf8(path).unwrap().replace('\\', "/");
            assert!(game.0.join(path).is_dir());
        }
    }
}

#[test]
fn resume_preserves_timer_blocking_and_execution_across_clock_wrap() {
    let game = Game::new();
    let project = Project::open(&game.0).unwrap();
    for epoch in [50, u32::MAX - 49] {
        let mut session = Session::new(&project, "wait.scn").unwrap();
        let mut host = TestHost {
            now: epoch,
            ..Default::default()
        };
        let mut mapped = false;
        assert_eq!(session.next_wake_delay_ms(epoch), Some(0));
        let mut waiting = false;
        for _ in 0..10 {
            let state = session
                .resume(&project, &mut host, |event| {
                    mapped |= matches!(event, Event::MouseButtonMapping { .. });
                    Ok(())
                })
                .unwrap();
            if state
                == (SessionState::Waiting {
                    epoch,
                    duration: 100,
                })
            {
                waiting = true;
                break;
            }
        }
        assert!(waiting && !mapped);
        assert_eq!(session.next_wake_delay_ms(epoch), Some(100));
        host.now = epoch.wrapping_add(99);
        assert_eq!(session.next_wake_delay_ms(host.now), Some(1));
        assert_eq!(
            session.resume(&project, &mut host, |_| Ok(())).unwrap(),
            SessionState::Waiting {
                epoch,
                duration: 100
            }
        );
        host.now = epoch.wrapping_add(100);
        assert_eq!(session.next_wake_delay_ms(host.now), Some(0));
        assert_eq!(
            session.resume(&project, &mut host, |_| Ok(())).unwrap(),
            SessionState::Runnable
        );
        let mut ended = false;
        for _ in 0..10 {
            let state = session
                .resume(&project, &mut host, |event| {
                    mapped |= matches!(event, Event::MouseButtonMapping { value: 42, .. });
                    Ok(())
                })
                .unwrap();
            if state == SessionState::Ended {
                ended = true;
                break;
            }
        }
        assert!(mapped && ended);
        assert_eq!(session.next_wake_delay_ms(host.now), None);
    }
}

#[test]
fn movies_continue_while_script_waits_without_releasing_the_task() {
    let game = Game::new();
    std::fs::write(
        game.0.join("movie.mpg"),
        include_bytes!("fixtures/movie.mpg"),
    )
    .unwrap();
    let mut code = 0x05b5u16.to_le_bytes().to_vec();
    code.extend([4, 0, 0, 0, 0]);
    code.extend(b"\x10movie.mpg\0");
    for value in [0u32, u32::MAX] {
        code.push(4);
        code.extend(value.to_le_bytes());
    }
    for (opcode, value) in [(0x0028u16, 1000u32), (0x049d, 42), (0, 0)] {
        code.extend(opcode.to_le_bytes());
        code.push(4);
        code.extend(value.to_le_bytes());
    }
    std::fs::write(game.0.join("wait.scn"), code).unwrap();
    let project = Project::open(&game.0).unwrap();
    let mut session = Session::new(&project, "wait.scn").unwrap();
    let mut host = TestHost {
        now: 0,
        ..Default::default()
    };
    let mut mapped = false;
    let mut ended = false;
    let mut wakes = 0;
    for _ in 0..100 {
        let now = host.now;
        let state = session
            .resume_main(
                &project,
                &mut host,
                Wake::Deadline(now),
                |event| {
                    if matches!(event, Event::MouseButtonMapping { value: 42, .. }) {
                        assert_eq!(now, 1000);
                        mapped = true;
                    }
                    Ok(())
                },
                || false,
            )
            .unwrap();
        if state == SessionState::Ended {
            ended = true;
            break;
        }
        if matches!(state, SessionState::Waiting { .. }) {
            let delay = session.next_wake_delay_ms(host.now).unwrap();
            assert!(delay > 0);
            if wakes == 0 {
                assert!(delay < 1000, "the movie must wake before the script timer");
            }
            host.now += delay;
            wakes += 1;
        }
    }
    assert!(mapped && ended);
    assert!(wakes > 1 && wakes < 30);
    assert!(host.invalidates > 1);
}

#[test]
fn main_coroutine_preserves_single_step_events_with_different_slice_budgets() {
    let game = Game::new();
    let project = Project::open(&game.0).unwrap();
    for epoch in [50, u32::MAX - 49] {
        let trace = |slice_budget: Option<usize>| {
            let mut session = Session::new(&project, "wait.scn").unwrap();
            let mut host = TestHost {
                now: epoch,
                ..Default::default()
            };
            let mut events = Vec::new();
            for _ in 0..100 {
                let state = if let Some(budget) = slice_budget {
                    let now = host.now;
                    let mut processed = 0;
                    session
                        .resume_main(
                            &project,
                            &mut host,
                            Wake::Deadline(now),
                            |event| {
                                events.push(event.clone());
                                Ok(())
                            },
                            || {
                                processed += 1;
                                processed >= budget
                            },
                        )
                        .unwrap()
                } else {
                    session
                        .resume(&project, &mut host, |event| {
                            events.push(event.clone());
                            Ok(())
                        })
                        .unwrap()
                };
                match state {
                    SessionState::Ended => return events,
                    SessionState::Waiting { .. } => {
                        host.now = host
                            .now
                            .wrapping_add(session.next_wake_delay_ms(host.now).unwrap());
                    }
                    SessionState::Runnable => {}
                }
            }
            panic!("session did not end");
        };
        let reference = trace(None);
        for budget in [1, 3, 10_000] {
            assert_eq!(trace(Some(budget)), reference, "slice budget {budget}");
        }
    }
}

#[test]
fn main_coroutine_skips_early_deadlines_but_delivers_input_messages() {
    let game = Game::new();
    let project = Project::open(&game.0).unwrap();
    let mut session = Session::new(&project, "wait.scn").unwrap();
    let mut host = TestHost {
        now: 50,
        ..Default::default()
    };
    let waiting = SessionState::Waiting {
        epoch: 50,
        duration: 100,
    };
    assert_eq!(
        session
            .resume_main(
                &project,
                &mut host,
                Wake::Deadline(50),
                |_| Ok(()),
                || false,
            )
            .unwrap(),
        waiting
    );

    let calls = (host.polls, host.requests);
    for now in [51, 99, 149] {
        host.now = now;
        let state = session
            .resume_main(
                &project,
                &mut host,
                Wake::Deadline(now),
                |_| panic!("early timer executed the VM"),
                || panic!("early timer checked the execution budget"),
            )
            .unwrap();
        assert_eq!(state, waiting);
        assert_eq!((host.polls, host.requests), calls);
    }
    // The message must reach the VM even though the task timer is not due.
    host.messages.push([0x10, 0, 0]);
    session
        .resume_main(&project, &mut host, Wake::Input, |_| Ok(()), || false)
        .unwrap();
    assert!(host.messages.is_empty());
    assert!(host.requests > calls.1);

    host.now = 150;
    let mut mapped = false;
    assert_eq!(
        session
            .resume_main(
                &project,
                &mut host,
                Wake::Deadline(150),
                |event| {
                    mapped |= matches!(event, Event::MouseButtonMapping { .. });
                    Ok(())
                },
                || false,
            )
            .unwrap(),
        SessionState::Ended
    );
    assert!(
        !mapped,
        "the queued close must end the session before the next instruction"
    );
    let calls = (host.polls, host.requests);
    assert_eq!(
        session
            .resume_main(
                &project,
                &mut host,
                Wake::Input,
                |_| panic!("ended session emitted another event"),
                || false,
            )
            .unwrap(),
        SessionState::Ended
    );
    assert_eq!((host.polls, host.requests), calls);
}
