//! Host-independent project startup and deterministic research traces.
use anyhow::{Result, bail};
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
        let mut platform = TracePlatform::default();
        for _ in 0..max_steps {
            let event = vm.step()?;
            emit(&event)?;
            if let Event::Platform { location, request } = event {
                if !simulate_platform {
                    bail!(
                        "{}:{:#x}: platform reply required: {request:?}; use --simulate-platform for a synthetic host",
                        location.scenario,
                        location.offset
                    );
                }
                let value = platform.respond(&request)?;
                emit(&Event::PlatformReply {
                    value,
                    simulated: true,
                })?;
                vm.respond(value)?;
            }
        }
        bail!(
            "{}:{:#x}: trace step budget {max_steps} exhausted",
            name,
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
