//! Shared scenario execution and resource handling for traces and native hosts.
use crate::resources::{AudioStream, Resources, Sound, Surface};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use shiinario_assets::project::Project;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest, SoundCommand};
use std::sync::Arc;

pub trait Host {
    fn available(&self) -> bool {
        true
    }
    fn simulated(&self) -> bool;
    fn poll(&mut self) -> Result<()>;
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32>;
    fn point(&mut self, request: &PlatformRequest) -> Result<[i32; 2]>;
    fn mouse_mapping(&mut self, _value: u32) {}
    fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()>;
    fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32>;
    fn stop_stream(&mut self, handle: u32) -> Result<()>;
    fn play_stream(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()>;
}

pub struct Session {
    vm: BinaryVm,
    resources: Resources,
}
impl Session {
    pub fn set_best_effort(&mut self, enabled: bool) {
        self.vm.set_best_effort(enabled);
    }
    pub fn new(project: &Project, name: &str) -> Result<Self> {
        let mut vm = BinaryVm::new(name, project.read(name)?)?;
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
        Ok(Self {
            vm,
            resources: Resources::for_project(project)?,
        })
    }
    pub fn surface(&self, id: u32) -> Option<Surface> {
        self.resources.surface(id)
    }
    pub fn location(&self) -> shiinario_scenario::Location {
        self.vm.location()
    }
    pub fn scenario_buffers(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.vm.scenario_buffers()
    }
    pub fn digests(&self, mut emit: impl FnMut(&Event) -> Result<()>) -> Result<()> {
        for (name, data) in self.vm.scenario_buffers() {
            emit(&Event::ScenarioDigest {
                name: name.to_owned(),
                sha256: format!("{:x}", Sha256::digest(data)),
            })?;
        }
        Ok(())
    }
    /// Execute one scheduler event. False means the game has ended.
    pub fn step(
        &mut self,
        project: &Project,
        host: &mut impl Host,
        mut emit: impl FnMut(&Event) -> Result<()>,
    ) -> Result<bool> {
        let vm = &mut self.vm;
        let resources = &mut self.resources;
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
        if let Event::MouseButtonMapping { value, .. } = &event {
            host.mouse_mapping(*value);
        }
        emit(&event)?;
        if event == Event::SchedulerPoll {
            if !host.available() {
                bail!(
                    "scheduler requires a platform host; use --simulate-platform for a synthetic host"
                );
            }
            host.poll()?;
            vm.respond(1)?;
            emit(&Event::PlatformReply {
                value: 1,
                simulated: host.simulated(),
            })?;
            return Ok(true);
        }
        if let Event::ArchiveSearchPath { name, .. } = &event {
            resources.register_archive(name);
        }
        if matches!(event, Event::End) {
            return Ok(false);
        }
        if let Event::Platform { location, request } = event {
            if !host.available() {
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
                let point = host.point(&request)?;
                vm.respond_point(point)?;
                emit(&Event::PointReply {
                    point,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::ImageBounds { id, frame } = &request {
                let bounds = resources.image_bounds(*id, *frame).with_context(|| {
                    format!("{}:{:#x}: image bounds", location.scenario, location.offset)
                })?;
                vm.respond_image_bounds(bounds)?;
                emit(&Event::ImageBoundsReply { bounds })?;
                return Ok(true);
            }
            if let PlatformRequest::SurfacePixels { id } = &request {
                let address = vm.respond_surface_pixels(resources.surface_memory(*id))?;
                emit(&Event::PlatformReply {
                    value: address,
                    simulated: false,
                })?;
                return Ok(true);
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
                return Ok(true);
            }
            if let PlatformRequest::FinishAudioFade {
                handle,
                interval,
                step,
                target,
            } = &request
            {
                emit(&Event::CompatibilitySkip {
                    location: location.clone(),
                    opcode: 0x06ec,
                    detail: format!(
                        "audio fade interval {interval} ms omitted; stream {handle:#x} completes at volume {}",
                        target & 0x7fffffff
                    ),
                })?;
                if *handle != 0 {
                    let stream = resources
                        .audio_stream(*handle)
                        .context("unknown audio stream in fade")?;
                    let percent = target & 0x7fffffff;
                    if stream.volume.percent() != percent {
                        stream.volume.set_percent(percent)?;
                        if *step <= 0 && target & 0x80000000 == 0 {
                            host.stop_stream(*handle)?;
                        }
                    }
                }
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
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
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::Sound { id, command } = &request {
                let value = host.sound_command(*id, command).with_context(|| {
                    format!(
                        "{}:{:#x}: sound slot {id}: {command:?}",
                        location.scenario, location.offset
                    )
                })?;
                vm.respond(value)?;
                emit(&Event::PlatformReply {
                    value,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::StopAudioStream { handle }
            | PlatformRequest::ReleaseAudioStream { handle } = &request
            {
                if *handle != 0 {
                    resources.audio_stream(*handle).with_context(|| {
                        format!(
                            "{}:{:#x}: unknown audio stream handle {handle:#x}",
                            location.scenario, location.offset
                        )
                    })?;
                    host.stop_stream(*handle).with_context(|| {
                        format!(
                            "{}:{:#x}: stop audio stream",
                            location.scenario, location.offset
                        )
                    })?;
                }
                if matches!(request, PlatformRequest::ReleaseAudioStream { .. }) {
                    resources.release_audio_stream(*handle)?;
                }
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::PlayAudioStream { handle, flags } = &request {
                let stream = resources
                    .audio_stream(*handle)
                    .context("unknown audio stream handle")?;
                host.play_stream(*handle, stream.clone(), *flags)
                    .with_context(|| {
                        format!(
                            "{}:{:#x}: play audio stream",
                            location.scenario, location.offset
                        )
                    })?;
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::AssetSizes { name } = &request {
                let sizes = resources.asset_sizes(project, name).with_context(|| {
                    format!(
                        "{}:{:#x}: asset sizes {name}",
                        location.scenario, location.offset
                    )
                })?;
                vm.respond_asset_sizes(sizes)?;
                emit(&Event::AssetSizesReply { sizes })?;
                return Ok(true);
            }
            if let PlatformRequest::ReadAssetInto { name, .. } = &request {
                let bytes = resources.read_asset(project, name).with_context(|| {
                    format!(
                        "{}:{:#x}: read asset {name}",
                        location.scenario, location.offset
                    )
                })?;
                vm.respond_asset_into(&bytes).with_context(|| {
                    format!(
                        "{}:{:#x}: asset destination",
                        location.scenario, location.offset
                    )
                })?;
                emit(&Event::PlatformReply {
                    value: bytes.len() as u32,
                    simulated: false,
                })?;
                return Ok(true);
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
                return Ok(true);
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
                return Ok(true);
            }
            if matches!(&request, PlatformRequest::ProjectDirectory)
                || matches!(&request, PlatformRequest::ReadRegistryString { root: 0x80000001, path, name } if path == "software\\GuiltyPLUS\\Ran→Sem(DL)" && name == "DataPath")
            {
                // An empty registry DataPath makes the SCN ask for the
                // installation directory. Empty project-relative paths
                // refer to Project.root; no host absolute path enters SCN.
                emit(&Event::PlatformBytesReply {
                    bytes: Vec::new(),
                    simulated: host.simulated(),
                })?;
                vm.respond_bytes(&[])?;
                return Ok(true);
            }
            let resource_reply = resources.respond(project, &request).with_context(|| {
                format!(
                    "{}:{:#x}: resource request {request:?}",
                    location.scenario, location.offset
                )
            })?;
            if matches!(&request, PlatformRequest::DrawGlyph { surface: 0, .. })
                || matches!(&request, PlatformRequest::StretchSurface(stretch) if stretch.destination.id==0)
            {
                host.respond(&PlatformRequest::InvalidateRect {
                    rect: [
                        0,
                        0,
                        project.config.width as i32,
                        project.config.height as i32,
                    ],
                })?;
            }
            if let PlatformRequest::LoadSound { id, .. } = &request {
                host.install_sound(
                    *id,
                    resources
                        .sound_buffer(*id)
                        .context("loaded sound missing")?,
                )
                .with_context(|| {
                    format!(
                        "{}:{:#x}: install sound {id}",
                        location.scenario, location.offset
                    )
                })?;
            }
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
                host.respond(&request)?
            };
            emit(&Event::PlatformReply {
                value,
                simulated: resource_reply.is_none() && host.simulated(),
            })?;
            vm.respond(value)?;
        }
        Ok(true)
    }
}
