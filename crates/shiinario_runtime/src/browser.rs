//! Worker-driven browser player using the same session, software canvas and mixer.
use crate::{
    audio::Mixer,
    input::{AsyncKeys, ControlState},
    resources::{AudioStream, Sound},
    session::{Host, Session},
    viewport::ViewportTransform,
};
use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_scenario::{Event, MouseButtonMapping, PlatformRequest, SoundCommand};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = shiinarioTitle)]
    fn browser_title(title: &str);
    #[wasm_bindgen(js_name = shiinarioFullscreen)]
    fn browser_fullscreen(enabled: bool);
    #[wasm_bindgen(js_name = shiinarioNow)]
    fn browser_now() -> f64;
}
fn js_error(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&format!("{error:#}"))
}
struct BrowserHost {
    elapsed: u32,
    controls: ControlState,
    async_keys: AsyncKeys,
    mouse: u8,
    mapping: MouseButtonMapping,
    cursor: [i32; 2],
    transform: ViewportTransform,
    class_style: u32,
    dirty: bool,
    audio: Mixer,
    text_key: bool,
    window_messages: Vec<[u32; 3]>,
}
impl Host for BrowserHost {
    fn take_window_messages(&mut self) -> Vec<[u32; 3]> {
        std::mem::take(&mut self.window_messages)
    }
    fn simulated(&self) -> bool {
        false
    }
    fn poll(&mut self) -> Result<()> {
        Ok(())
    }
    fn mouse_mapping(&mut self, value: u32) {
        self.mapping.value = value;
    }
    fn point(&mut self, request: &PlatformRequest) -> Result<[i32; 2]> {
        match request {
            PlatformRequest::MouseButtons => {
                let buttons = self.mapping.map_buttons(self.mouse);
                Ok([i32::from(buttons & 1 != 0), i32::from(buttons & 2 != 0)])
            }
            PlatformRequest::CursorPosition | PlatformRequest::MapCursor { .. } => {
                self.transform.cursor_reply(request, self.cursor)
            }
            _ => bail!("unsupported point request: {request:?}"),
        }
    }
    fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()> {
        self.audio.install_sound(id, sound)
    }
    fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32> {
        self.audio.sound_command(id, command)
    }
    fn fade_stream(&mut self, handle: u32, interval: u32, step: i32, target: u32) -> Result<()> {
        self.audio.fade(handle, interval, step, target)
    }
    fn stop_stream(&mut self, handle: u32) -> Result<()> {
        self.audio.stop(handle);
        Ok(())
    }
    fn play_stream(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()> {
        self.audio.play(handle, stream, flags)?;
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
            PlatformRequest::ClockMilliseconds => Ok(self.elapsed),
            PlatformRequest::WaitTaskTimer { epoch, duration } => {
                Ok(u32::from((self.elapsed).wrapping_sub(*epoch) >= *duration))
            }
            PlatformRequest::PumpMessages => {
                self.poll()?;
                Ok(1)
            }
            PlatformRequest::InvalidateRect { .. } => {
                self.dirty = true;
                Ok(1)
            }
            PlatformRequest::SetWindowTitle { window: 0, title } => {
                browser_title(title);
                Ok(1)
            }
            PlatformRequest::SetFullscreen { enabled } => {
                browser_fullscreen(*enabled);
                Ok(1)
            }
            PlatformRequest::InitializeAudio { .. } => Ok(1),
            PlatformRequest::InitializeGraphics | PlatformRequest::ReleaseGraphics => Ok(1),
            // Select the verified scalar renderer; SIMD CPU flags describe the original x86 host.
            PlatformRequest::CpuFeatures => Ok(0),
            PlatformRequest::DeviceCaps {
                device: 0,
                index: 12,
            } => Ok(32),
            PlatformRequest::DisableIme => Ok(0),
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
            _ => bail!("unsupported browser platform request: {request:?}"),
        }
    }
}
#[wasm_bindgen]
pub struct BrowserPlayer {
    project: Project,
    session: Session,
    host: BrowserHost,
    stopped: bool,
}
#[wasm_bindgen]
impl BrowserPlayer {
    #[wasm_bindgen(constructor)]
    pub fn new(font: Vec<u8>) -> std::result::Result<BrowserPlayer, JsValue> {
        console_error_panic_hook::set_once();
        let create = || -> Result<Self> {
            crate::text_render::set_browser_font(font)?;
            let project = crate::open(".")?;
            let session = Session::new(&project, &project.config.startup)?;
            let size = [project.config.width, project.config.height];
            Ok(Self {
                project,
                session,
                stopped: false,
                host: BrowserHost {
                    elapsed: 0,
                    controls: ControlState {
                        focused: true,
                        ..Default::default()
                    },
                    async_keys: AsyncKeys::default(),
                    mouse: 0,
                    mapping: Default::default(),
                    cursor: [0; 2],
                    transform: ViewportTransform::fit(size, size)?,
                    class_style: 0xb,
                    dirty: true,
                    audio: Mixer::default(),
                    text_key: false,
                    window_messages: Vec::new(),
                },
            })
        };
        create().map_err(js_error)
    }
    /// Advance a bounded slice; the worker schedules another slice after yielding.
    pub fn step(&mut self, elapsed_ms: u32) -> std::result::Result<bool, JsValue> {
        if self.stopped {
            return Ok(false);
        }
        self.host.elapsed = elapsed_ms;
        let deadline = browser_now() + 4.0;
        for _ in 0..10000 {
            let mut waiting = false;
            let result = self.session.step(&self.project, &mut self.host, |event| {
                if matches!(
                    event,
                    Event::Platform {
                        request: PlatformRequest::WaitTaskTimer { .. },
                        ..
                    }
                ) {
                    waiting = true;
                } else if matches!(event, Event::PlatformReply { value: 1, .. }) {
                    waiting = false;
                }
                Ok(())
            });
            match result {
                Ok(true) => {}
                Ok(false) => {
                    self.stopped = true;
                    return Ok(false);
                }
                Err(error) => {
                    self.stopped = true;
                    return Err(js_error(error.context(format!(
                        "{}:{:#x}",
                        self.session.location().scenario,
                        self.session.location().offset
                    ))));
                }
            }
            if waiting || browser_now() >= deadline {
                break;
            }
        }
        Ok(true)
    }
    pub fn frame(&mut self) -> Vec<u8> {
        if !self.host.dirty {
            return Vec::new();
        }
        if let Some(surface) = self.session.display() {
            self.host.dirty = false;
            return surface.rgba.clone();
        }
        Vec::new()
    }
    pub fn audio(&mut self, frames: u32, rate: u32) -> std::result::Result<Vec<f32>, JsValue> {
        if frames > 8192 || !(8000..=192000).contains(&rate) {
            return Err(JsValue::from_str("invalid audio request"));
        }
        let mut samples = vec![0.0; frames as usize * 2];
        self.host
            .audio
            .render(&mut samples, rate, 2)
            .map_err(js_error)?;
        Ok(samples)
    }
    pub fn pointer(&mut self, x: i32, y: i32, button: u8, pressed: bool) {
        self.host.cursor = [x, y];
        if !(1..=3).contains(&button) {
            return;
        }
        let (mask, vk, message) = match button {
            1 => (1, 1, 0x201),
            2 => (2, 2, 0x204),
            _ => (4, 4, 0x207),
        };
        if pressed {
            self.host.mouse |= mask;
        } else {
            self.host.mouse &= !mask;
        }
        self.host.async_keys.set(vk, pressed);
        self.host
            .window_messages
            .push([message + u32::from(!pressed), 0, 0]);
    }
    pub fn key(&mut self, scan: u32, vk: u32, pressed: bool, text: bool) {
        if scan >= 256 || vk >= 256 {
            return;
        }
        self.host.text_key |= pressed && text;
        self.host.controls.keys[scan as usize] = pressed;
        self.host.async_keys.set(vk as usize, pressed);
        self.host
            .window_messages
            .push([if pressed { 0x100 } else { 0x101 }, vk, 0]);
        for (key, left, right) in [(0x10, 0x2a, 0x36), (0x11, 0x1d, 0x9d), (0x12, 0x38, 0xb8)] {
            self.host.async_keys.set(
                key,
                self.host.controls.keys[left] || self.host.controls.keys[right],
            );
        }
    }
    pub fn focus(&mut self, focused: bool) {
        self.host.controls.focused = focused;
        if !focused {
            self.host.controls.keys.fill(false);
            self.host.async_keys.clear();
            self.host.mouse = 0;
        }
    }
}
