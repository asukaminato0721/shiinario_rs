//! Host-independent project startup and deterministic research traces.
pub mod audio;
pub mod input;
pub mod resources;
pub mod viewport;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use shiinario_assets::project::Project;
use shiinario_scenario::{BinaryVm, Event, Input, PlatformRequest, TextScript, TextVm};
use std::path::Path;
pub fn boot(project: &Project) -> Result<()> {
    let name = &project.config.startup;
    let data = project.read(name)?;
    shiinario_scenario::boot_binary(name, &data)
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
    let data = project.read(name)?;
    if name.to_ascii_lowercase().ends_with(".scn") {
        let mut vm = BinaryVm::new(name, data)?;
        vm.set_viewport(project.config.width, project.config.height)?;
        if let Some(value) = project.config.values.get("background") {
            let value: i32 = value.parse().context("invalid Background configuration")?;
            if value != -1 {
                vm.set_background_mode(value as u32);
            }
        }
        if let Some(value) = project.config.values.get("turbo") {
            let value: i32 = value.parse().context("invalid Turbo configuration")?;
            if value != -1 {
                vm.set_dispatch_quantum((value as u32).max(1))?;
            }
        }
        let mut platform = TracePlatform {
            tick_ms,
            ..TracePlatform::default()
        };
        let mut resources = resources::Resources::for_project(project)?;
        let mut audio = audio::Mixer::default();
        for _ in 0..max_steps {
            let event = match vm.scheduled_step() {
                Ok(event) => event,
                Err(error) => {
                    for (name, data) in vm.scenario_buffers() {
                        emit(&Event::ScenarioDigest {
                            name: name.to_owned(),
                            sha256: format!("{:x}", Sha256::digest(data)),
                        })?;
                    }
                    return Err(error);
                }
            };
            emit(&event)?;
            if event == Event::SchedulerPoll {
                if !simulate_platform {
                    bail!(
                        "scheduler requires a platform host; use --simulate-platform for a synthetic host"
                    );
                }
                platform.poll();
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: true,
                })?;
                continue;
            }
            if let Event::ArchiveSearchPath { name, .. } = &event {
                resources.register_archive(name);
            }
            if matches!(event, Event::End) {
                return Ok(());
            }
            if let Event::Platform { location, request } = event {
                if !simulate_platform {
                    bail!(
                        "{}:{:#x}: platform reply required: {request:?}; use --simulate-platform for a synthetic host",
                        location.scenario,
                        location.offset
                    );
                }
                if matches!(
                    request,
                    PlatformRequest::CursorPosition | PlatformRequest::MapCursor { .. }
                ) {
                    let point = match request {
                        PlatformRequest::CursorPosition => [0, 0],
                        PlatformRequest::MapCursor { point } => {
                            viewport::ViewportTransform::default().to_logical(point)?
                        }
                        _ => unreachable!(),
                    };
                    vm.respond_point(point)?;
                    emit(&Event::PointReply {
                        point,
                        simulated: true,
                    })?;
                    continue;
                }
                if let PlatformRequest::ImageBounds { id, frame } = &request {
                    let bounds = resources.image_bounds(*id, *frame).with_context(|| {
                        format!("{}:{:#x}: image bounds", location.scenario, location.offset)
                    })?;
                    vm.respond_image_bounds(bounds)?;
                    emit(&Event::ImageBoundsReply { bounds })?;
                    continue;
                }
                if let PlatformRequest::SurfacePixels { id } = &request {
                    let address = vm.respond_surface_pixels(resources.surface_memory(*id))?;
                    emit(&Event::PlatformReply {
                        value: address,
                        simulated: false,
                    })?;
                    continue;
                }
                if let PlatformRequest::CreateAudioStream { address, flags } = &request {
                    let handle = resources
                        .create_audio_stream(vm.allocation_bytes(*address)?, *flags)
                        .with_context(|| {
                            format!(
                                "{}:{:#x}: create audio stream",
                                location.scenario, location.offset
                            )
                        })?;
                    vm.respond(handle)?;
                    emit(&Event::PlatformReply {
                        value: handle,
                        simulated: false,
                    })?;
                    continue;
                }
                if let PlatformRequest::SetAudioStreamVolume { handle, percent } = &request {
                    if *handle != 0 {
                        resources
                            .audio_stream(*handle)
                            .with_context(|| {
                                format!(
                                    "{}:{:#x}: unknown audio stream handle {handle:#x}",
                                    location.scenario, location.offset
                                )
                            })?
                            .volume
                            .set_percent(*percent)?;
                    }
                    vm.respond(1)?;
                    emit(&Event::PlatformReply {
                        value: 1,
                        simulated: true,
                    })?;
                    continue;
                }
                if let PlatformRequest::PlayAudioStream { handle, flags } = &request {
                    let stream = resources
                        .audio_stream(*handle)
                        .context("unknown audio stream handle")?;
                    audio
                        .play(*handle, stream.clone(), *flags)
                        .with_context(|| {
                            format!(
                                "{}:{:#x}: play audio stream",
                                location.scenario, location.offset
                            )
                        })?;
                    vm.respond(1)?;
                    emit(&Event::PlatformReply {
                        value: 1,
                        simulated: true,
                    })?;
                    continue;
                }
                if let PlatformRequest::LoadAsset { name } = &request {
                    let bytes = resources.read_asset(project, name).with_context(|| {
                        format!(
                            "{}:{:#x}: load asset {name}",
                            location.scenario, location.offset
                        )
                    })?;
                    let address = vm.respond_asset(bytes)?;
                    emit(&Event::PlatformReply {
                        value: address,
                        simulated: false,
                    })?;
                    continue;
                }
                if let PlatformRequest::LoadScenario { name, .. } = &request {
                    let bytes = resources.read_asset(project, name).with_context(|| {
                        format!(
                            "{}:{:#x}: load scenario {name}",
                            location.scenario, location.offset
                        )
                    })?;
                    vm.respond_scenario(bytes)?;
                    emit(&Event::PlatformReply {
                        value: 1,
                        simulated: false,
                    })?;
                    continue;
                }
                if matches!(&request, PlatformRequest::ProjectDirectory)
                    || matches!(&request, PlatformRequest::ReadRegistryString { root: 0x80000001, path, name } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "DataPath")
                {
                    // An empty registry DataPath makes the SCN ask for the
                    // installation directory. Empty project-relative paths
                    // refer to Project.root; no host absolute path enters SCN.
                    emit(&Event::PlatformBytesReply {
                        bytes: Vec::new(),
                        simulated: true,
                    })?;
                    vm.respond_bytes(&[])?;
                    continue;
                }
                let resource_reply = resources.respond(project, &request).with_context(|| {
                    format!(
                        "{}:{:#x}: resource request {request:?}",
                        location.scenario, location.offset
                    )
                })?;
                if let PlatformRequest::DrawImages { id, .. } = &request {
                    let memory = resources
                        .surface_memory(*id)
                        .context("draw surface missing")?;
                    let bgr_sha256 = format!("{:x}", Sha256::digest(memory.read(0, memory.len())?));
                    emit(&Event::SurfaceDigest {
                        id: *id,
                        bgr_sha256,
                    })?;
                }
                let value = if let Some(value) = resource_reply {
                    value
                } else if let PlatformRequest::FileExists { path } = &request {
                    u32::from(project.loose_path_exists(path)?)
                } else {
                    platform.respond(&request)?
                };
                emit(&Event::PlatformReply {
                    value,
                    simulated: resource_reply.is_none(),
                })?;
                vm.respond(value)?;
            }
        }
        bail!(
            "{}:{:#x}: trace step budget {max_steps} exhausted",
            vm.location().scenario,
            vm.location().offset
        );
    }
    let script = TextScript::parse(name, &data)?;
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
    class_style: u32,
    clock_ms: u32,
    tick_ms: u32,
}
impl Default for TracePlatform {
    fn default() -> Self {
        Self {
            class_style: 0xb,
            clock_ms: 0,
            tick_ms: 0,
        }
    }
}
impl TracePlatform {
    fn poll(&mut self) {
        self.clock_ms = self.clock_ms.wrapping_add(self.tick_ms);
    }
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        match request {
            PlatformRequest::ReadControls => Ok(input::ControlState::default().mask()),
            // The portable trace reports no x86 rendering acceleration.
            PlatformRequest::CpuFeatures => Ok(0),
            PlatformRequest::ReleaseGraphics | PlatformRequest::SetFullscreen { .. } => Ok(1),
            PlatformRequest::InitializeAudio { .. } | PlatformRequest::InitializeGraphics => Ok(1),
            PlatformRequest::ReadIniInteger { default, .. } => Ok(*default),
            PlatformRequest::ClockMilliseconds => Ok(self.clock_ms),
            PlatformRequest::InvalidateRect { .. } => Ok(1),
            PlatformRequest::ReadRegistryValue {
                root: 0x80000001,
                path,
                name,
            } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "InstMode" => Ok(0),
            PlatformRequest::PumpMessages => {
                self.poll();
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
        clock.poll();
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
        frozen.poll();
        frozen.respond(&PlatformRequest::PumpMessages).unwrap();
        assert_eq!(
            frozen.respond(&PlatformRequest::ClockMilliseconds).unwrap(),
            0
        );
    }
}
