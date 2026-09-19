//! Host-independent project startup and deterministic research traces.
use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_scenario::{BinaryVm, Event, Input, TextScript, TextVm};
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
    mut emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
    let data = project.read(name)?;
    if name.to_ascii_lowercase().ends_with(".scn") {
        let mut vm = BinaryVm::new(name, data)?;
        for _ in 0..max_steps {
            emit(&vm.step()?)?;
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
pub fn open(path: impl AsRef<Path>) -> Result<Project> {
    Project::open(path)
}
