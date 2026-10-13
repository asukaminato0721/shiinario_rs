use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_runtime::{
    resources::{AudioStream, Sound},
    session::{Host, Session, SessionState},
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

struct TestHost {
    now: u32,
    invalidates: usize,
}
impl Host for TestHost {
    fn simulated(&self) -> bool {
        true
    }
    fn poll(&mut self) -> Result<()> {
        Ok(())
    }
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        match request {
            PlatformRequest::ClockMilliseconds => Ok(self.now),
            PlatformRequest::WaitTaskTimer { epoch, duration } => {
                Ok(u32::from(self.now.wrapping_sub(*epoch) >= *duration))
            }
            PlatformRequest::InvalidateRect { .. } => {
                self.invalidates += 1;
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
fn resume_preserves_timer_blocking_and_execution_across_clock_wrap() {
    let game = Game::new();
    let project = Project::open(&game.0).unwrap();
    for epoch in [50, u32::MAX - 49] {
        let mut session = Session::new(&project, "wait.scn").unwrap();
        let mut host = TestHost {
            now: epoch,
            invalidates: 0,
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
        invalidates: 0,
    };
    let mut mapped = false;
    let mut ended = false;
    let mut wakes = 0;
    for _ in 0..100 {
        let now = host.now;
        let state = session
            .resume(&project, &mut host, |event| {
                if matches!(event, Event::MouseButtonMapping { value: 42, .. }) {
                    assert_eq!(now, 1000);
                    mapped = true;
                }
                Ok(())
            })
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
