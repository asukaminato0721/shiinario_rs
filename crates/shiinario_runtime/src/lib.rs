//! Host-independent project startup and deterministic research traces.
use anyhow::{Result, bail};
use shiinario_assets::project::Project;
use shiinario_scenario::{Event, Input, TextScript, TextVm};
use std::path::Path;
pub fn boot(project: &Project) -> Result<()> {
    let name = &project.config.startup;
    let data = project.read(name)?;
    shiinario_scenario::boot_binary(name, &data)
}
/// Text traces are explicit research entry points, not replacements for startup.
pub fn trace(
    project: &Project,
    name: &str,
    max_steps: usize,
    mut emit: impl FnMut(&Event) -> Result<()>,
) -> Result<()> {
    let data = project.read(name)?;
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
