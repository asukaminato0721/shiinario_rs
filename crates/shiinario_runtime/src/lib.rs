//! Host-independent project startup and deterministic research traces.
pub mod audio;
pub mod resources;
use anyhow::{Context, Result, bail};
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
    mut emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
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
        let mut platform = TracePlatform::default();
        let mut resources = resources::Resources::default();
        let mut audio = audio::Mixer::default();
        for _ in 0..max_steps {
            let event = vm.scheduled_step()?;
            emit(&event)?;
            if event == Event::SchedulerPoll {
                if !simulate_platform {
                    bail!(
                        "scheduler requires a platform host; use --simulate-platform for a synthetic host"
                    );
                }
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
}
impl Default for TracePlatform {
    fn default() -> Self {
        Self { class_style: 0xb }
    }
}
impl TracePlatform {
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32> {
        match request {
            // The portable trace reports no x86 rendering acceleration.
            PlatformRequest::CpuFeatures => Ok(0),
            PlatformRequest::ReleaseGraphics | PlatformRequest::SetFullscreen { .. } => Ok(1),
            PlatformRequest::InitializeAudio { .. } | PlatformRequest::InitializeGraphics => Ok(1),
            PlatformRequest::ReadIniInteger { default, .. } => Ok(*default),
            // Frozen until a trace supplies elapsed time explicitly.
            PlatformRequest::ClockMilliseconds => Ok(0),
            PlatformRequest::ReadRegistryValue {
                root: 0x80000001,
                path,
                name,
            } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "InstMode" => Ok(0),
            PlatformRequest::PumpMessages => Ok(1),
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
