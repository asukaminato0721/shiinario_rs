//! Worker-driven browser player using the same session, software canvas and mixer.
use crate::{
    audio::Mixer,
    input::{AsyncKeys, ControlState},
    resources::{AudioStream, Sound},
    session::{Host, Session, SessionState, Wake},
    viewport::ViewportTransform,
};
use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_scenario::{MouseButtonMapping, PlatformRequest, SoundCommand};
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
    #[wasm_bindgen(js_name = shiinarioWake)]
    fn browser_wake();
    #[wasm_bindgen(js_name = shiinarioMessageBox)]
    fn browser_message_box(text: &str, title: &str, flags: u32);
    #[wasm_bindgen(js_name = shiinarioEdit)]
    fn browser_edit(window: u32, text: &str, x: i32, y: i32, width: i32, height: i32);
    #[wasm_bindgen(js_name = shiinarioDestroyEdit)]
    fn browser_destroy_edit(window: u32);
}
fn js_error(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&format!("{error:#}"))
}
struct BrowserHost {
    edits: crate::edit::Edits,
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
    wake_input: bool,
    dialog_pending: bool,
    dialog_reply: Option<u32>,
}
impl Host for BrowserHost {
    fn window_text(&mut self, window: u32, capacity: u32) -> Result<Vec<u8>> {
        self.edits.text(window, capacity)
    }
    fn message_box(&mut self, text: &str, title: &str, flags: u32) -> Result<Option<u32>> {
        crate::dialog::default_reply(flags)?;
        if let Some(value) = self.dialog_reply.take() {
            self.dialog_pending = false;
            return Ok(Some(value));
        }
        if !self.dialog_pending {
            browser_message_box(text, title, flags);
            self.dialog_pending = true;
        }
        Ok(None)
    }
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
            PlatformRequest::ReadJoystick => Ok([0, 0]),
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
    fn pause_stream(&mut self, handle: u32, paused: bool) -> Result<()> {
        self.audio.pause(handle, paused);
        Ok(())
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
        if let Some(value) = self.edits.respond(request)? {
            match request {
                PlatformRequest::CreateEdit { text, rect } => {
                    browser_edit(value, text, rect[0], rect[1], rect[2], rect[3])
                }
                PlatformRequest::DestroyEdit { window } => browser_destroy_edit(*window),
                PlatformRequest::SetWindowTitle { window, title } => {
                    browser_edit(*window, title, 0, 0, 0, 0)
                }
                _ => {}
            }
            self.dirty = true;
            return Ok(value);
        }

        match request {
            PlatformRequest::SetCursorPosition { position } => {
                // Browsers cannot move the operating-system pointer.
                self.cursor = self.transform.to_window_point(*position)?;
                Ok(1)
            }
            PlatformRequest::TextInput { clear } => {
                if *clear {
                    self.text_key = false;
                    return Ok(0);
                }
                self.controls.mouse_buttons = self.mapping.map_buttons(self.mouse);
                Ok(self.controls.merged_mask(false) | if self.text_key { 0x10000 } else { 0 })
            }
            PlatformRequest::ReadKeyState { key } => {
                Ok(self.async_keys.query(*key, self.controls.focused))
            }
            PlatformRequest::ReadControls => {
                self.controls.mouse_buttons = self.mapping.map_buttons(self.mouse);
                Ok(self.controls.mask())
            }
            PlatformRequest::ReadMergedControls => {
                self.controls.mouse_buttons = self.mapping.map_buttons(self.mouse);
                Ok(self.controls.merged_mask(false))
            }
            PlatformRequest::ReadScanKey { scan } => Ok(
                if self.controls.focused && self.controls.keys.get(*scan as usize) == Some(&true) {
                    0x80
                } else {
                    0
                },
            ),
            PlatformRequest::ReadMouseButtons => {
                Ok(u32::from(self.mapping.map_buttons(self.mouse)))
            }
            PlatformRequest::GetAudioStreamStatus { handle } => {
                Ok(self.audio.stream_status(*handle))
            }
            PlatformRequest::ClockMilliseconds => Ok(self.elapsed),
            PlatformRequest::WaitTaskTimer { epoch, duration } => {
                Ok(u32::from((self.elapsed).wrapping_sub(*epoch) >= *duration))
            }
            PlatformRequest::PumpMessages => {
                self.poll()?;
                Ok(1)
            }
            PlatformRequest::InvalidateRect { .. } | PlatformRequest::UpdateWindow => {
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
            // Browser pages do not control the browser window's title-bar buttons.
            PlatformRequest::SetMinimizeButton { .. } => Ok(1),
            PlatformRequest::InitializeAudio { .. } => Ok(1),
            PlatformRequest::ReleaseAudio => {
                self.audio.release_sounds();
                Ok(1)
            }
            PlatformRequest::InitializeGraphics | PlatformRequest::ReleaseGraphics => Ok(1),
            // Select the verified scalar renderer; SIMD CPU flags describe the original x86 host.
            PlatformRequest::CpuFeatures => Ok(0),
            // SM_SWAPBUTTON: browser input already uses primary/secondary buttons.
            PlatformRequest::SystemMetric { index: 23 } => Ok(0),
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
            // No menu exists for the null handle. DestroyMenu returns FALSE.
            PlatformRequest::DestroyMenu { menu: 0 } => Ok(0),
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
            session.set_wake_callback(browser_wake);
            let size = [project.config.width, project.config.height];
            Ok(Self {
                project,
                session,
                stopped: false,
                host: BrowserHost {
                    edits: Default::default(),
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
                    dialog_pending: false,
                    dialog_reply: None,
                    wake_input: false,
                },
            })
        };
        create().map_err(js_error)
    }
    /// Resume the main coroutine. The worker schedules its next wake.
    pub fn resume(&mut self, elapsed_ms: u32) -> std::result::Result<bool, JsValue> {
        if self.stopped {
            return Ok(false);
        }
        self.host.elapsed = elapsed_ms;
        let deadline = browser_now() + 4.0;
        let wake = if std::mem::take(&mut self.host.wake_input) {
            Wake::Input
        } else {
            Wake::Deadline(elapsed_ms)
        };
        match self.session.resume_main(
            &self.project,
            &mut self.host,
            wake,
            |_| Ok(()),
            || browser_now() >= deadline,
        ) {
            Ok(SessionState::Runnable | SessionState::Waiting { .. }) => Ok(true),
            Ok(SessionState::Ended) => {
                self.stopped = true;
                Ok(false)
            }
            Err(error) => {
                self.stopped = true;
                Err(js_error(error.context(format!(
                    "{}:{:#x}",
                    self.session.location().scenario,
                    self.session.location().offset
                ))))
            }
        }
    }
    /// The worker can sleep until this delay, subject to its audio deadline.
    pub fn next_wake_delay_ms(&self, elapsed_ms: u32) -> u32 {
        self.session.next_wake_delay_ms(elapsed_ms).unwrap_or(0)
    }
    pub fn edit_text(&mut self, window: u32, text: String) -> std::result::Result<(), JsValue> {
        self.host.edits.set_text(window, &text).map_err(js_error)
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
        self.host.wake_input = true;
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
        self.host.wake_input = true;
        self.host.text_key |= pressed && text;
        self.host.controls.keys[scan as usize] = pressed;
        self.host.async_keys.set(vk as usize, pressed);
        self.host
            .window_messages
            .push(crate::input::keyboard_message(
                vk,
                pressed,
                self.host.controls.keys[0x38] || self.host.controls.keys[0xb8],
            ));
        for (key, left, right) in [(0x10, 0x2a, 0x36), (0x11, 0x1d, 0x9d), (0x12, 0x38, 0xb8)] {
            self.host.async_keys.set(
                key,
                self.host.controls.keys[left] || self.host.controls.keys[right],
            );
        }
    }
    pub fn message_box_reply(&mut self, value: u32) -> std::result::Result<(), JsValue> {
        if !self.host.dialog_pending || !matches!(value, 1 | 2 | 6 | 7) {
            return Err(JsValue::from_str("invalid message box reply"));
        }
        self.host.dialog_reply = Some(value);
        self.host.wake_input = true;
        Ok(())
    }
    pub fn focus(&mut self, focused: bool) {
        self.host.wake_input = true;
        self.host.controls.focused = focused;
        if !focused {
            self.host.controls.keys.fill(false);
            self.host.async_keys.clear();
            self.host.mouse = 0;
        }
    }
}
