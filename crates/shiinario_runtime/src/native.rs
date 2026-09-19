//! Linux window, input and audio services for the shared scenario session.
use crate::{
    audio::AudioOutput,
    input::{AsyncKeys, ControlState},
    presentation::Presentation,
    resources::{AudioStream, Sound},
    session::{Host, Session},
    viewport::ViewportTransform,
};
use anyhow::{Context, Result, bail, ensure};
use shiinario_assets::project::Project;
use shiinario_scenario::{Event, MouseButtonMapping, PlatformRequest, SoundCommand};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Fullscreen, Window, WindowId},
};

#[derive(Default)]
pub struct Options {
    /// Stop at presentation omissions instead of logging and continuing.
    pub strict: bool,
    /// Optional bounded native smoke run, measured from window creation.
    pub run_for: Option<Duration>,
    /// Optional new directory outside the installation for modified SCN buffers.
    pub scenario_dump: Option<PathBuf>,
}
pub fn run(project: &Project, options: Options) -> Result<()> {
    let dump = if let Some(path) = &options.scenario_dump {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let resolved = parent.canonicalize()?.join(
            path.file_name()
                .context("dump requires a new directory name")?,
        );
        ensure!(
            !resolved.starts_with(project.root.canonicalize()?),
            "scenario dump must be outside the game installation"
        );
        std::fs::create_dir(&resolved).context("scenario dump directory must not already exist")?;
        Some(resolved)
    } else {
        None
    };
    let mut session = Session::new(project, &project.config.startup)?;
    session.set_best_effort(!options.strict);
    let mut app = App {
        project,
        session,
        options,
        host: None,
        presentation: None,
        error: None,
        instructions: 0,
        presented: 0,
    };
    EventLoop::new()?.run_app(&mut app)?;
    eprintln!(
        "Native run: {} instructions, {} presented frames; {}:{:#x}",
        app.instructions,
        app.presented,
        app.session.location().scenario,
        app.session.location().offset
    );
    if let Some(host) = &app.host {
        eprintln!(
            "Native audio: {} streams started, {} device frames",
            host.streams_started,
            host.audio.as_ref().map_or(0, AudioOutput::rendered_frames)
        );
    }
    if let Some(directory) = dump {
        use std::io::Write;
        let mut manifest = std::fs::File::create_new(directory.join("index.txt"))?;
        for (index, (name, data)) in app.session.scenario_buffers().enumerate() {
            let filename = format!("{index:03}.scn");
            std::fs::File::create_new(directory.join(&filename))?.write_all(data)?;
            writeln!(manifest, "{filename}\t{name}")?;
        }
    }
    if let Some(error) = app.error {
        return Err(error);
    }
    Ok(())
}
struct NativeHost {
    window: Arc<Window>,
    started: Instant,
    controls: ControlState,
    async_keys: AsyncKeys,
    mouse: u8,
    mapping: MouseButtonMapping,
    cursor: [i32; 2],
    transform: ViewportTransform,
    adjust_cursor: bool,
    class_style: u32,
    dirty: bool,
    audio: Option<AudioOutput>,
    streams_started: usize,
    text_key: bool,
    window_messages: Vec<[u32; 3]>,
}
impl NativeHost {
    fn key(&mut self, virtual_key: usize, pressed: bool) {
        self.async_keys.set(virtual_key, pressed);
    }
    fn keyboard(&mut self, key: KeyCode, pressed: bool) {
        if let Some((scan, vk)) = keyboard_codes(key) {
            if self.controls.keys[scan] == pressed {
                return;
            }
            self.controls.keys[scan] = pressed;
            let message_key = match vk {
                0xa0 | 0xa1 => 0x10,
                0xa2 | 0xa3 => 0x11,
                0xa4 | 0xa5 => 0x12,
                _ => vk,
            };
            self.window_messages
                .push([if pressed { 0x100 } else { 0x101 }, message_key as u32, 0]);
            self.key(
                vk,
                if vk == 0x0d {
                    self.controls.keys[0x1c] || self.controls.keys[0x9c]
                } else {
                    pressed
                },
            );
            for (vk, left, right) in [(0x10, 0x2a, 0x36), (0x11, 0x1d, 0x9d), (0x12, 0x38, 0xb8)] {
                let down = self.controls.keys[left] || self.controls.keys[right];
                if down != self.async_keys.is_down(vk) {
                    self.key(vk, down);
                }
            }
        }
    }
}
impl Host for NativeHost {
    fn take_window_messages(&mut self) -> Vec<[u32; 3]> {
        std::mem::take(&mut self.window_messages)
    }
    fn simulated(&self) -> bool {
        false
    }
    fn poll(&mut self) -> Result<()> {
        if let Some(audio) = &self.audio {
            audio.check()?;
        }
        Ok(())
    }
    fn mouse_mapping(&mut self, value: u32) {
        self.mapping.value = value;
    }
    fn point(&mut self, request: &PlatformRequest) -> Result<[i32; 2]> {
        match request {
            PlatformRequest::CursorPosition => {
                if self.adjust_cursor {
                    self.transform.to_logical(self.cursor)
                } else {
                    Ok(self.cursor)
                }
            }
            PlatformRequest::MapCursor { point } => self.transform.to_logical(*point),
            _ => bail!("unsupported point request: {request:?}"),
        }
    }
    fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()> {
        self.audio
            .as_ref()
            .context("audio has not been initialized")?
            .install_sound(id, sound)
    }
    fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32> {
        self.audio
            .as_ref()
            .context("audio has not been initialized")?
            .sound_command(id, command)
    }
    fn stop_stream(&mut self, handle: u32) -> Result<()> {
        self.audio
            .as_ref()
            .context("audio has not been initialized")?
            .stop(handle)
    }
    fn play_stream(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()> {
        self.audio
            .as_ref()
            .context("audio has not been initialized")?
            .play(handle, stream, flags)?;
        self.streams_started += 1;
        Ok(())
    }
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        match request {
            PlatformRequest::TextInput { clear } => {
                if *clear {
                    self.text_key = false;
                    return Ok(0);
                }
                self.controls.mouse_buttons = self.mapping.map_buttons(self.mouse);
                Ok(self.controls.mask() | if self.text_key { 0x10000 } else { 0 })
            }
            PlatformRequest::ReadKeyState { key } => {
                Ok(self.async_keys.query(*key, self.controls.focused))
            }
            PlatformRequest::ReadControls => {
                self.controls.mouse_buttons = self.mapping.map_buttons(self.mouse);
                Ok(self.controls.mask())
            }
            PlatformRequest::ClockMilliseconds => Ok(self.started.elapsed().as_millis() as u32),
            PlatformRequest::WaitTaskTimer { epoch, duration } => Ok(u32::from(
                (self.started.elapsed().as_millis() as u32).wrapping_sub(*epoch) >= *duration,
            )),
            PlatformRequest::PumpMessages => {
                self.poll()?;
                Ok(1)
            }
            PlatformRequest::InvalidateRect { .. } => {
                self.dirty = true;
                Ok(1)
            }
            PlatformRequest::SetWindowTitle { window: 0, title } => {
                self.window.set_title(title);
                Ok(1)
            }
            PlatformRequest::SetFullscreen { enabled } => {
                self.window
                    .set_fullscreen(enabled.then_some(Fullscreen::Borderless(None)));
                Ok(1)
            }
            PlatformRequest::InitializeAudio { .. } => {
                if self.audio.is_none() {
                    self.audio = Some(AudioOutput::open()?);
                }
                Ok(1)
            }
            PlatformRequest::InitializeGraphics | PlatformRequest::ReleaseGraphics => Ok(1),
            // Select the verified scalar renderer; SIMD CPU flags describe the original x86 host.
            PlatformRequest::CpuFeatures => Ok(0),
            PlatformRequest::DeviceCaps {
                device: 0,
                index: 12,
            } => Ok(32),
            PlatformRequest::DisableIme => {
                self.window.set_ime_allowed(false);
                Ok(0)
            }
            PlatformRequest::GetClassLong {
                window: 0,
                index: -26,
            } => Ok(self.class_style),
            PlatformRequest::SetClassLong {
                window: 0,
                index: -26,
                value,
            } => {
                let old = self.class_style;
                self.class_style = *value;
                Ok(old)
            }
            PlatformRequest::FindWindow { .. } => Ok(0),
            // The verified startup has an empty INI search prefix; the original
            // command returns its operand default even when RANDL.ini exists.
            PlatformRequest::ReadIniInteger { default, .. } => Ok(*default),
            PlatformRequest::ReadRegistryValue {
                root: 0x80000001,
                path,
                name,
            } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "InstMode" => Ok(0),
            _ => bail!("unsupported native platform request: {request:?}"),
        }
    }
}
struct App<'a> {
    project: &'a Project,
    session: Session,
    options: Options,
    host: Option<NativeHost>,
    presentation: Option<Presentation>,
    error: Option<anyhow::Error>,
    instructions: usize,
    presented: usize,
}
impl App<'_> {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error.context(format!(
            "{}:{:#x}",
            self.session.location().scenario,
            self.session.location().offset
        )));
        event_loop.exit();
    }
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let logical = [self.project.config.width, self.project.config.height];
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Shiina Rio")
                    .with_inner_size(PhysicalSize::new(logical[0], logical[1])),
            )?,
        );
        let presentation = pollster::block_on(Presentation::new(window.clone(), logical))?;
        let flag = |name: &str| -> Result<Option<bool>> {
            self.project
                .config
                .values
                .get(name)
                .map(|value| {
                    value
                        .parse::<i32>()
                        .map(|n| n != 0)
                        .with_context(|| format!("invalid {name} configuration"))
                })
                .transpose()
        };
        let adjust_cursor = flag("adjustmousepos")?
            .or(flag("emulatefullscreen")?)
            .unwrap_or(false);
        self.host = Some(NativeHost {
            controls: ControlState {
                focused: window.has_focus(),
                ..Default::default()
            },
            window,
            started: Instant::now(),
            async_keys: AsyncKeys::default(),
            mouse: 0,
            mapping: Default::default(),
            cursor: [0; 2],
            transform: presentation.transform()?,
            adjust_cursor,
            class_style: 0xb,
            dirty: true,
            audio: None,
            streams_started: 0,
            text_key: false,
            window_messages: Vec::new(),
        });
        self.presentation = Some(presentation);
        Ok(())
    }
}
impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.host.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
        }
    }
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(host) = &mut self.host else {
            return;
        };
        let Some(presentation) = &mut self.presentation else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                presentation.resize(size);
                match presentation.transform() {
                    Ok(transform) => host.transform = transform,
                    Err(error) => {
                        self.fail(event_loop, error);
                        return;
                    }
                }
                host.dirty = true;
            }
            WindowEvent::Focused(focused) => {
                host.controls.focused = focused;
                if !focused {
                    for key in 0..256 {
                        if host.async_keys.is_down(key) {
                            host.window_messages.push([0x101, key as u32, 0]);
                        }
                    }
                    host.controls.keys.fill(false);
                    host.async_keys.clear();
                    host.mouse = 0;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                host.cursor = [position.x as i32, position.y as i32]
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed
                    && event.text.as_ref().is_some_and(|s| !s.is_empty())
                {
                    host.text_key = true;
                }
                if let PhysicalKey::Code(key) = event.physical_key {
                    host.keyboard(key, event.state == ElementState::Pressed);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some((bit, vk)) = match button {
                    MouseButton::Left => Some((1, 1)),
                    MouseButton::Right => Some((2, 2)),
                    MouseButton::Middle => Some((4, 4)),
                    _ => None,
                } {
                    let pressed = state == ElementState::Pressed;
                    if pressed {
                        host.mouse |= bit;
                    } else {
                        host.mouse &= !bit;
                    }
                    host.key(vk, pressed);
                    let message = match button {
                        MouseButton::Left => 0x201,
                        MouseButton::Right => 0x204,
                        _ => 0x207,
                    } + u32::from(!pressed);
                    host.window_messages.push([message, 0, 0]);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(canvas) = self.session.surface(0) {
                    match presentation.draw(&canvas) {
                        Ok(shown) => {
                            host.dirty = !shown;
                            self.presented += usize::from(shown);
                        }
                        Err(error) => self.fail(event_loop, error),
                    }
                }
            }
            _ => {}
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if event_loop.exiting() {
            return;
        }
        let Some(host) = &mut self.host else {
            return;
        };
        if self
            .options
            .run_for
            .is_some_and(|limit| host.started.elapsed() >= limit)
        {
            event_loop.exit();
            return;
        }
        // Bound interpreter work so input and close events stay responsive.
        let deadline = Instant::now() + Duration::from_millis(4);
        for _ in 0..10000 {
            let mut timer_wait = false;
            let result = self.session.step(self.project, host, |event| {
                if let Event::CompatibilitySkip {
                    location,
                    opcode,
                    detail,
                } = event
                {
                    eprintln!(
                        "SKIP {}:{:#x} opcode={opcode:#06x}: {detail}",
                        location.scenario, location.offset
                    );
                }
                if matches!(
                    event,
                    Event::Platform {
                        request: PlatformRequest::WaitTaskTimer { .. },
                        ..
                    }
                ) {
                    timer_wait = true;
                } else if matches!(event, Event::PlatformReply { value: 1, .. }) {
                    timer_wait = false;
                }
                if matches!(
                    event,
                    Event::BinaryInstruction { .. }
                        | Event::Platform { .. }
                        | Event::MouseButtonMapping { .. }
                        | Event::ArchiveSearchPath { .. }
                ) {
                    self.instructions += 1;
                }
                Ok(())
            });
            match result {
                Ok(true) => {}
                Ok(false) => {
                    event_loop.exit();
                    return;
                }
                Err(error) => {
                    self.fail(event_loop, error);
                    return;
                }
            }
            if timer_wait || Instant::now() >= deadline {
                break;
            }
        }
        if host.dirty {
            host.window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(1),
        ));
    }
}

fn keyboard_codes(key: KeyCode) -> Option<(usize, usize)> {
    use KeyCode::*;
    Some(match key {
        Escape => (0x01, 0x1b),
        Tab => (0x0f, 0x09),
        Enter => (0x1c, 0x0d),
        NumpadEnter => (0x9c, 0x0d),
        ControlLeft => (0x1d, 0xa2),
        ControlRight => (0x9d, 0xa3),
        ShiftLeft => (0x2a, 0xa0),
        ShiftRight => (0x36, 0xa1),
        AltLeft => (0x38, 0xa4),
        AltRight => (0xb8, 0xa5),
        Space => (0x39, 0x20),
        Home => (0xc7, 0x24),
        End => (0xcf, 0x23),
        Insert => (0xd2, 0x2d),
        Delete => (0xd3, 0x2e),
        ArrowUp => (0xc8, 0x26),
        ArrowLeft => (0xcb, 0x25),
        ArrowRight => (0xcd, 0x27),
        ArrowDown => (0xd0, 0x28),
        PageUp => (0xc9, 0x21),
        PageDown => (0xd1, 0x22),
        Backspace => (0x0e, 0x08),
        Numpad0 => (0x52, 0x60),
        Numpad1 => (0x4f, 0x61),
        Numpad2 => (0x50, 0x62),
        Numpad3 => (0x51, 0x63),
        Numpad4 => (0x4b, 0x64),
        Numpad5 => (0x4c, 0x65),
        Numpad6 => (0x4d, 0x66),
        Numpad7 => (0x47, 0x67),
        Numpad8 => (0x48, 0x68),
        Numpad9 => (0x49, 0x69),
        F1 => (0x3b, 0x70),
        F2 => (0x3c, 0x71),
        F3 => (0x3d, 0x72),
        F4 => (0x3e, 0x73),
        F5 => (0x3f, 0x74),
        F6 => (0x40, 0x75),
        F7 => (0x41, 0x76),
        F8 => (0x42, 0x77),
        F9 => (0x43, 0x78),
        F10 => (0x44, 0x79),
        F11 => (0x57, 0x7a),
        F12 => (0x58, 0x7b),
        KeyA => (0x1e, 0x41),
        KeyB => (0x30, 0x42),
        KeyC => (0x2e, 0x43),
        KeyD => (0x20, 0x44),
        KeyE => (0x12, 0x45),
        KeyF => (0x21, 0x46),
        KeyG => (0x22, 0x47),
        KeyH => (0x23, 0x48),
        KeyI => (0x17, 0x49),
        KeyJ => (0x24, 0x4a),
        KeyK => (0x25, 0x4b),
        KeyL => (0x26, 0x4c),
        KeyM => (0x32, 0x4d),
        KeyN => (0x31, 0x4e),
        KeyO => (0x18, 0x4f),
        KeyP => (0x19, 0x50),
        KeyQ => (0x10, 0x51),
        KeyR => (0x13, 0x52),
        KeyS => (0x1f, 0x53),
        KeyT => (0x14, 0x54),
        KeyU => (0x16, 0x55),
        KeyV => (0x2f, 0x56),
        KeyW => (0x11, 0x57),
        KeyX => (0x2d, 0x58),
        KeyY => (0x15, 0x59),
        KeyZ => (0x2c, 0x5a),
        Digit1 => (0x02, 0x31),
        Digit2 => (0x03, 0x32),
        Digit3 => (0x04, 0x33),
        Digit4 => (0x05, 0x34),
        Digit5 => (0x06, 0x35),
        Digit6 => (0x07, 0x36),
        Digit7 => (0x08, 0x37),
        Digit8 => (0x09, 0x38),
        Digit9 => (0x0a, 0x39),
        Digit0 => (0x0b, 0x30),
        _ => return None,
    })
}
