use anyhow::{Result, bail};
use shiinario_runtime::{
    audio::Mixer,
    movie::Movies,
    resources::{AudioStream, Resources, Sound},
    session::Host,
};
use shiinario_scenario::{BinaryVm, Event, MovieCommand as Cmd, PlatformRequest, SoundCommand};
use std::sync::Arc;

const FIXTURE: &[u8] = include_bytes!("fixtures/movie.mpg");
#[derive(Default)]
struct TestHost {
    clock: u32,
    mixer: Mixer,
    starts: usize,
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
            PlatformRequest::ClockMilliseconds => Ok(self.clock),
            PlatformRequest::InvalidateRect { .. } => {
                self.invalidates += 1;
                Ok(1)
            }
            _ => bail!("unexpected request: {request:?}"),
        }
    }
    fn point(&mut self, _: &PlatformRequest) -> Result<[i32; 2]> {
        bail!("unexpected point query")
    }
    fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()> {
        self.mixer.install_sound(id, sound)
    }
    fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32> {
        self.mixer.sound_command(id, command)
    }
    fn play_stream(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()> {
        self.starts += 1;
        self.mixer.play(handle, stream, flags)
    }
    fn fade_stream(&mut self, handle: u32, interval: u32, step: i32, target: u32) -> Result<()> {
        self.mixer.fade(handle, interval, step, target)
    }
    fn stop_stream(&mut self, handle: u32) -> Result<()> {
        self.mixer.stop(handle);
        Ok(())
    }
}
fn setup(looping: bool) -> (Movies, Resources, TestHost) {
    let mut resources = Resources::default();
    resources.create_surface(0, 64, 48, 0).unwrap();
    resources.create_surface(13, 32, 24, 0).unwrap();
    let mut movies = Movies::default();
    let mut host = TestHost::default();
    movies
        .open(
            0,
            &Cmd::Open {
                name: "fixture.mpg".into(),
                flags: u32::from(looping),
                surface: 13,
                play: true,
            },
            FIXTURE.to_vec(),
            &mut resources,
            &mut host,
        )
        .unwrap();
    (movies, resources, host)
}
fn command(m: &mut Movies, r: &mut Resources, h: &mut TestHost, c: Cmd) -> u32 {
    m.command(0, &c, r, h).unwrap()
}

#[test]
fn decode_audio_present_pause_seek_restart_and_stop() {
    let (mut m, mut r, mut h) = setup(false);
    assert_eq!(m.dimensions(0).unwrap(), [32, 24]);
    let memory = r.surface_memory(13).unwrap();
    h.clock = 150;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20e);
    command(&mut m, &mut r, &mut h, Cmd::Update { present: true });
    assert!(h.invalidates > 0);
    assert!(
        r.surface(0)
            .unwrap()
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] > 10)
    );
    let original = memory.read(0, memory.len()).unwrap();
    let mut pcm = vec![0.0; 4096];
    h.mixer.render(&mut pcm, 44100, 1).unwrap();
    assert!(
        pcm.iter().any(|s| s.abs() > 0.01),
        "movie audio must reach the mixer"
    );
    command(&mut m, &mut r, &mut h, Cmd::Pause);
    h.clock = 1150;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x211);
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Position), 150);
    assert_eq!(memory.read(0, memory.len()).unwrap(), original);
    h.mixer.render(&mut pcm, 44100, 1).unwrap();
    assert!(pcm.iter().all(|s| *s == 0.0));
    command(&mut m, &mut r, &mut h, Cmd::Seek { milliseconds: 300 });
    command(&mut m, &mut r, &mut h, Cmd::SetVolume { percent: 25 });
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Volume), 25);
    command(&mut m, &mut r, &mut h, Cmd::Play);
    h.clock += 500;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20d);
    assert_ne!(memory.read(0, memory.len()).unwrap(), original);
    h.mixer.render(&mut pcm, 44100, 1).unwrap();
    assert!(pcm.iter().all(|s| *s == 0.0));
    command(&mut m, &mut r, &mut h, Cmd::Play);
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Position), 0);
    command(&mut m, &mut r, &mut h, Cmd::Stop);
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::LoopCount), u32::MAX);
    m.stop_all(&mut h).unwrap();
    assert!(m.dimensions(0).is_err());
}

#[test]
fn b_frame_eof_timing_loop_and_replacement() {
    let (mut m, mut r, mut h) = setup(false);
    h.clock = 400;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20e);
    let before = r.surface(13).unwrap();
    h.clock = 560;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20d);
    assert_ne!(
        r.surface(13).unwrap(),
        before,
        "delayed final B/P pictures must be presented"
    );
    let open = Cmd::Open {
        name: "fixture.mpg".into(),
        flags: 1,
        surface: 13,
        play: true,
    };
    m.open(0, &open, FIXTURE.to_vec(), &mut r, &mut h).unwrap();
    h.clock += 750;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20e);
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::LoopCount), 1);
    assert_eq!(h.starts, 3);
    assert!(
        m.open(0, &open, b"not a movie".to_vec(), &mut r, &mut h)
            .is_err()
    );
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20e);
    m.stop_all(&mut h).unwrap();
    let mut pcm = [0.0; 100];
    h.mixer.render(&mut pcm, 44100, 1).unwrap();
    assert_eq!(pcm, [0.0; 100]);
}

#[test]
fn preload_muting_clipping_and_clock_wrap() {
    let mut m = Movies::default();
    let mut r = Resources::default();
    r.create_surface(0, 32, 24, 0).unwrap();
    let mut h = TestHost {
        clock: u32::MAX - 100,
        ..Default::default()
    };
    let open = Cmd::Open {
        name: "fixture.mpg".into(),
        flags: 0,
        surface: u32::MAX,
        play: false,
    };
    m.open(0, &open, FIXTURE.to_vec(), &mut r, &mut h).unwrap();
    assert_eq!(h.starts, 0);
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Status), 0x20d);
    command(
        &mut m,
        &mut r,
        &mut h,
        Cmd::Rect {
            rect: [-8, 4, 32, 16],
        },
    );
    command(&mut m, &mut r, &mut h, Cmd::SetVolume { percent: 0 });
    command(&mut m, &mut r, &mut h, Cmd::Play);
    h.clock = 50;
    m.update(&mut h, &mut r).unwrap();
    assert_eq!(command(&mut m, &mut r, &mut h, Cmd::Position), 151);
    let frame = r.surface(0).unwrap();
    assert!(
        frame.rgba[..32 * 4 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[..3] == [0, 0, 0])
    );
    assert!(frame.rgba.as_chunks::<4>().0.iter().any(|p| p[0] > 10));
    let mut samples = [0.0; 4096];
    h.mixer.render(&mut samples, 44100, 1).unwrap();
    assert!(samples.iter().all(|s| *s == 0.0));
    command(&mut m, &mut r, &mut h, Cmd::SetVolume { percent: 100 });
    h.mixer.render(&mut samples, 44100, 1).unwrap();
    assert!(samples.iter().any(|s| s.abs() > 0.01));
}

#[test]
fn strict_vm_requests_wait_for_real_dimensions_and_status() {
    fn imm(n: u32) -> Vec<u8> {
        let mut bytes = vec![4];
        bytes.extend(n.to_le_bytes());
        bytes
    }
    fn instruction(code: &mut Vec<u8>, op: u16, args: &[u8]) {
        code.extend(op.to_le_bytes());
        code.extend(args);
    }
    let mut code = Vec::new();
    let mut args = imm(0);
    args.extend(b"\x10fixture.mpg\0");
    args.extend(imm(0));
    args.extend(imm(13));
    instruction(&mut code, 0x5b5, &args);
    let mut args = imm(0);
    args.extend([12, 0, 0, 12, 1, 0]);
    instruction(&mut code, 0x5bf, &args);
    let mut args = imm(0);
    args.extend([12, 2, 0]);
    instruction(&mut code, 0x5b9, &args);
    // The subsequent instruction reads back the query output.
    instruction(&mut code, 0x49d, &[12, 2, 0]);
    let mut vm = BinaryVm::new("movie.scn", code).unwrap();
    let (mut movies, mut resources, mut host) = setup(false);
    for _ in 0..3 {
        let event = vm.step().unwrap();
        assert_eq!(vm.step().unwrap(), event);
        let Event::Platform {
            request: PlatformRequest::Movie { id, command },
            ..
        } = event
        else {
            panic!("{event:?}");
        };
        match command {
            Cmd::Open { .. } => {
                vm.respond(
                    movies
                        .open(id, &command, FIXTURE.to_vec(), &mut resources, &mut host)
                        .unwrap(),
                )
                .unwrap();
            }
            Cmd::Dimensions => vm.respond_point(movies.dimensions(id).unwrap()).unwrap(),
            _ => vm
                .respond(
                    movies
                        .command(id, &command, &mut resources, &mut host)
                        .unwrap(),
                )
                .unwrap(),
        }
    }
    assert!(matches!(
        vm.step().unwrap(),
        Event::MouseButtonMapping { value: 0x20e, .. }
    ));
}
