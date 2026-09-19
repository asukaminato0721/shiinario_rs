//! Host-independent project startup and deterministic research traces.
pub mod audio;
mod buffer_audio;
pub mod input;
pub mod native;
mod presentation;
pub mod resources;
pub mod session;
pub mod viewport;
mod text_render;
use anyhow::{Result, bail};
use session::Host;
use shiinario_assets::project::Project;
use shiinario_scenario::{Event, Input, PlatformRequest, TextScript, TextVm};
use std::path::Path;
use std::sync::Arc;
pub fn boot(project: &Project) -> Result<()> {
    native::run(project, native::Options::default())
}
/// Traces execute the verified SCN or TXT subset and fail at unknown operations.
pub fn trace(
    project: &Project,
    name: &str,
    max_steps: usize,
    emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
    trace_with_platform(project, name, max_steps, false, emit)
}

/// Simulated platform replies are opt-in and appear explicitly in the trace.
/// They establish interpreter control flow, not an original-engine UI match.
pub fn trace_with_platform(
    project: &Project,
    name: &str,
    max_steps: usize,
    simulate_platform: bool,
    emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
    trace_with_clock(project, name, max_steps, simulate_platform, 0, emit)
}

/// Each host poll advances the explicit synthetic clock by tick_ms.
pub fn trace_with_clock(
    project: &Project,
    name: &str,
    max_steps: usize,
    simulate_platform: bool,
    tick_ms: u32,
    mut emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
    anyhow::ensure!(
        simulate_platform || tick_ms == 0,
        "trace clock requires a simulated platform"
    );
    if name.to_ascii_lowercase().ends_with(".scn") {
        let mut session = session::Session::new(project, name)?;
        let mut platform = TracePlatform {
            tick_ms,
            enabled: simulate_platform,
            ..Default::default()
        };
        for _ in 0..max_steps {
            if !session.step(project, &mut platform, &mut emit)? {
                return Ok(());
            }
        }
        session.digests(&mut emit)?;
        bail!(
            "{}:{:#x}: trace step budget {max_steps} exhausted",
            session.location().scenario,
            session.location().offset
        );
    }
    let script = TextScript::parse(name, &project.read(name)?)?;
    let mut vm = TextVm::new(script);
    for _ in 0..max_steps {
        let event = vm.step()?;
        emit(&event)?;
        match event {
            Event::End => return Ok(()),
            Event::Wait { remaining_ms } => vm.input(Input::Tick(remaining_ms))?,
            Event::AwaitInput => vm.input(Input::Advance)?,
            _ => {}
        }
    }
    let l = vm.location();
    bail!(
        "{}:{:#x}: trace step budget {max_steps} exhausted",
        l.scenario,
        l.offset
    )
}

struct TracePlatform {
    enabled: bool,
    mixer: audio::Mixer,
    class_style: u32,
    clock_ms: u32,
    tick_ms: u32,
}
impl Default for TracePlatform {
    fn default() -> Self {
        Self {
            enabled: true,
            mixer: audio::Mixer::default(),
            class_style: 0xb,
            clock_ms: 0,
            tick_ms: 0,
        }
    }
}
impl Host for TracePlatform {
    fn available(&self) -> bool {
        self.enabled
    }
    fn simulated(&self) -> bool {
        true
    }
    fn poll(&mut self) -> Result<()> {
        self.clock_ms = self.clock_ms.wrapping_add(self.tick_ms);
        self.mixer.advance_ms(self.tick_ms);
        Ok(())
    }
    fn point(&mut self, request: &PlatformRequest) -> Result<[i32; 2]> {
        match request {
            PlatformRequest::CursorPosition => Ok([0, 0]),
            PlatformRequest::MapCursor { point } => {
                viewport::ViewportTransform::default().to_logical(*point)
            }
            _ => bail!("unsupported point request: {request:?}"),
        }
    }
    fn install_sound(&mut self, id: u32, sound: Arc<resources::Sound>) -> Result<()> {
        self.mixer.install_sound(id, sound)
    }
    fn sound_command(
        &mut self,
        id: u32,
        command: &shiinario_scenario::SoundCommand,
    ) -> Result<u32> {
        self.mixer.sound_command(id, command)
    }
    fn stop_stream(&mut self, handle: u32) -> Result<()> {
        self.mixer.stop(handle);
        Ok(())
    }
    fn play_stream(
        &mut self,
        handle: u32,
        stream: Arc<resources::AudioStream>,
        flags: u32,
    ) -> Result<()> {
        self.mixer.play(handle, stream, flags)
    }
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        match request {
            PlatformRequest::TextInput { .. } => Ok(0),
            PlatformRequest::ReadKeyState { .. } => Ok(input::key_state_reply(0, true)),
            PlatformRequest::ReadControls => Ok(input::ControlState::default().mask()),
            // The portable trace reports no x86 rendering acceleration.
            PlatformRequest::CpuFeatures => Ok(0),
            PlatformRequest::ReleaseGraphics | PlatformRequest::SetFullscreen { .. } => Ok(1),
            PlatformRequest::InitializeAudio { .. } | PlatformRequest::InitializeGraphics => Ok(1),
            PlatformRequest::ReadIniInteger { default, .. } => Ok(*default),
            PlatformRequest::ClockMilliseconds => Ok(self.clock_ms),
            PlatformRequest::WaitTaskTimer { epoch, duration } => {
                let ready = self.clock_ms.wrapping_sub(*epoch) >= *duration;
                // Model the original Sleep(1) between unsuccessful checks. A
                // zero tick retains the explicitly frozen research clock.
                if !ready && self.tick_ms != 0 {
                    self.clock_ms = self.clock_ms.wrapping_add(1);
                    self.mixer.advance_ms(1);
                }
                Ok(u32::from(ready))
            }
            PlatformRequest::InvalidateRect { .. } => Ok(1),
            PlatformRequest::ReadRegistryValue {
                root: 0x80000001,
                path,
                name,
            } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "InstMode" => Ok(0),
            PlatformRequest::PumpMessages => {
                self.poll()?;
                Ok(1)
            }
            PlatformRequest::DisableIme => Ok(0),
            PlatformRequest::DeviceCaps {
                device: 0,
                index: 12,
            } => Ok(32),
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
            PlatformRequest::SetWindowTitle { window: 0, .. } => Ok(1),
            _ => bail!("unsupported simulated platform request: {request:?}"),
        }
    }
}
pub fn open(path: impl AsRef<Path>) -> Result<Project> {
    Project::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timer_wait_checks_advance_one_millisecond_without_polling() {
        let mut clock = TracePlatform {
            clock_ms: u32::MAX - 1,
            tick_ms: 16,
            ..Default::default()
        };
        let request = PlatformRequest::WaitTaskTimer {
            epoch: u32::MAX - 1,
            duration: 3,
        };
        for now in [u32::MAX, 0, 1] {
            assert_eq!(clock.respond(&request).unwrap(), 0);
            assert_eq!(clock.clock_ms, now);
        }
        assert_eq!(clock.respond(&request).unwrap(), 1);
        assert_eq!(clock.clock_ms, 1);
        let mut frozen = TracePlatform::default();
        assert_eq!(
            frozen
                .respond(&PlatformRequest::WaitTaskTimer {
                    epoch: 0,
                    duration: 1
                })
                .unwrap(),
            0
        );
        assert_eq!(frozen.clock_ms, 0);
    }
    #[test]
    fn simulated_clock_advances_only_at_polls_and_wraps() {
        let mut clock = TracePlatform {
            clock_ms: u32::MAX - 7,
            tick_ms: 16,
            ..Default::default()
        };
        for _ in 0..2 {
            assert_eq!(
                clock.respond(&PlatformRequest::ClockMilliseconds).unwrap(),
                u32::MAX - 7
            );
        }
        clock.poll().unwrap();
        assert_eq!(
            clock.respond(&PlatformRequest::ClockMilliseconds).unwrap(),
            8
        );
        clock.respond(&PlatformRequest::PumpMessages).unwrap();
        assert_eq!(
            clock.respond(&PlatformRequest::ClockMilliseconds).unwrap(),
            24
        );
        let mut frozen = TracePlatform::default();
        frozen.poll().unwrap();
        frozen.respond(&PlatformRequest::PumpMessages).unwrap();
        assert_eq!(
            frozen.respond(&PlatformRequest::ClockMilliseconds).unwrap(),
            0
        );
    }
}
