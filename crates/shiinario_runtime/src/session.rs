//! Shared scenario execution and resource handling for traces and native hosts.
use crate::{
    main_loop::MainLoop,
    resources::{AudioStream, Resources, Sound, Surface},
};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use shiinario_assets::project::Project;
use shiinario_scenario::{BinaryVm, Event, MovieCommand, PlatformRequest, SoundCommand};
use std::sync::Arc;

pub trait Host {
    fn available(&self) -> bool {
        true
    }
    fn simulated(&self) -> bool;
    fn calendar(&mut self, time: bool) -> Result<[u32; 4]> {
        crate::calendar::local(time)
    }
    fn poll(&mut self) -> Result<()>;
    fn respond(&mut self, request: &PlatformRequest) -> Result<u32>;
    fn point(&mut self, request: &PlatformRequest) -> Result<[i32; 2]>;
    fn mouse_mapping(&mut self, _value: u32) {}
    fn take_window_messages(&mut self) -> Vec<[u32; 3]> {
        Vec::new()
    }
    fn install_sound(&mut self, id: u32, sound: Arc<Sound>) -> Result<()>;
    fn sound_command(&mut self, id: u32, command: &SoundCommand) -> Result<u32>;
    fn stop_stream(&mut self, handle: u32) -> Result<()>;
    fn fade_stream(&mut self, handle: u32, interval: u32, step: i32, target: u32) -> Result<()>;
    fn play_stream(&mut self, handle: u32, stream: Arc<AudioStream>, flags: u32) -> Result<()>;
}

/// A session resumes until one event completes or a timer blocks execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Runnable,
    Waiting { epoch: u32, duration: u32 },
    Ended,
}

/// Input wakes may run before a timer expires to deliver window messages.
#[derive(Clone, Copy, Debug)]
pub enum Wake {
    Deadline(u32),
    Input,
}

pub struct Session {
    vm: BinaryVm,
    resources: Resources,
    movies: crate::movie::Movies,
    state: SessionState,
    main_loop: Option<MainLoop>,
    window_timers: std::collections::BTreeMap<u32, (u32, u32)>,
    control_repeat: crate::input::ControlRepeat,
    scan_repeats: std::collections::BTreeMap<u32, crate::input::ControlRepeat>,
}
impl Session {
    pub fn new(project: &Project, name: &str) -> Result<Self> {
        let mut vm = BinaryVm::with_version(name, project.read(name)?, project.config.version)?;
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
            movies: Default::default(),
            state: SessionState::Runnable,
            main_loop: Some(MainLoop::new()),
            window_timers: Default::default(),
            control_repeat: Default::default(),
            scan_repeats: Default::default(),
        })
    }
    pub fn surface(&self, id: u32) -> Option<Surface> {
        self.resources.surface(id)
    }
    pub fn display(&self) -> Option<&Surface> {
        self.resources.display()
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
    /// Resume the session without changing the VM's task order or quantum.
    pub fn resume(
        &mut self,
        project: &Project,
        host: &mut impl Host,
        emit: impl FnMut(&Event) -> Result<()>,
    ) -> Result<SessionState> {
        self.step(project, host, emit)?;
        Ok(self.state)
    }
    /// Resume the main coroutine until a timer, execution budget or end yields.
    /// Check the host's time budget after each runnable event.
    /// Deadline wakes leave the VM untouched until a script or movie is due.
    pub fn resume_main(
        &mut self,
        project: &Project,
        host: &mut impl Host,
        wake: Wake,
        mut emit: impl FnMut(&Event) -> Result<()>,
        budget_expired: impl FnMut() -> bool,
    ) -> Result<SessionState> {
        if self.state == SessionState::Ended {
            return Ok(SessionState::Ended);
        }
        let release_waiter = match wake {
            Wake::Input => true,
            Wake::Deadline(now) => self.execution_delay_ms(now) == Some(0),
        };
        if !release_waiter && !self.main_loop.as_ref().is_some_and(MainLoop::is_notified) {
            return Ok(self.state);
        }
        let mut main_loop = self.main_loop.take().context("missing main coroutine")?;
        let result = main_loop.resume(
            release_waiter,
            || self.resume(project, host, &mut emit),
            budget_expired,
        );
        self.main_loop = Some(main_loop);
        result
    }
    /// Connect ready async tasks to a native proxy or a browser worker wake.
    pub fn set_wake_callback(&self, callback: impl Fn() + Send + Sync + 'static) {
        if let Some(main_loop) = &self.main_loop {
            main_loop.set_wake_callback(callback);
        }
    }
    /// Return the delay until a ready task or script/movie deadline needs work.
    /// The clock uses the host's wrapping u32 millisecond time.
    /// Input loops remain runnable because bytecode can inspect shared state.
    pub fn next_wake_delay_ms(&self, now: u32) -> Option<u32> {
        if self.state != SessionState::Ended
            && self.main_loop.as_ref().is_some_and(MainLoop::is_notified)
        {
            return Some(0);
        }
        self.execution_delay_ms(now)
    }
    fn execution_delay_ms(&self, now: u32) -> Option<u32> {
        match self.state {
            SessionState::Runnable => Some(0),
            SessionState::Waiting { epoch, duration } => {
                let delay = self
                    .window_timers
                    .values()
                    .map(|&(epoch, period)| period.saturating_sub(now.wrapping_sub(epoch)))
                    .fold(duration.saturating_sub(now.wrapping_sub(epoch)), u32::min);
                Some(
                    self.movies
                        .next_wake_delay_ms(now)
                        .map_or(delay, |movie| delay.min(movie)),
                )
            }
            SessionState::Ended => None,
        }
    }
    /// Execute one scheduler event. False means the game has ended.
    pub fn step(
        &mut self,
        project: &Project,
        host: &mut impl Host,
        mut emit: impl FnMut(&Event) -> Result<()>,
    ) -> Result<bool> {
        self.state = SessionState::Runnable;
        let vm = &mut self.vm;
        if !self.window_timers.is_empty() {
            let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
            for (&id, (epoch, period)) in &mut self.window_timers {
                if now.wrapping_sub(*epoch) >= *period {
                    *epoch = now;
                    vm.window_message(0x113, id, 0);
                }
            }
        }
        for [message, wparam, lparam] in host.take_window_messages() {
            vm.window_message(message, wparam, lparam);
        }
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
        if matches!(event, Event::TextTick { .. })
            && let Some(rect) = resources.commit_text_display()
        {
            // Updating the retained pixels alone does not schedule a native
            // redraw or a browser frame. Notify the host just as the original
            // glyph renderer's InvalidateRect call does.
            host.respond(&PlatformRequest::InvalidateRect { rect })?;
        }
        if event == Event::SchedulerPoll {
            if !host.available() {
                bail!(
                    "scheduler requires a platform host; use --simulate-platform for a synthetic host"
                );
            }
            host.poll()?;
            self.movies.update(host, resources)?;
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
        if let Event::ArchiveSearchPathRemoved { name, .. } = &event {
            resources.unregister_archive(name);
        }
        if matches!(event, Event::End) {
            self.movies.stop_all(host)?;
            self.state = SessionState::Ended;
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
            if let PlatformRequest::PostWindowMessage {
                window,
                message,
                wparam,
                lparam,
            } = &request
            {
                ensure!(
                    *window == 0 && *message == 0x10,
                    "unsupported posted window message: {request:?}"
                );
                // Queue WM_CLOSE so a registered window procedure can consume
                // it. An unhandled close ends the session via its normal path.
                vm.window_message(*message, *wparam, *lparam);
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if matches!(
                request,
                PlatformRequest::CursorPosition
                    | PlatformRequest::MapCursor { .. }
                    | PlatformRequest::MouseButtons
            ) {
                let point = host.point(&request)?;
                vm.respond_point(point)?;
                emit(&Event::PointReply {
                    point,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::Movie { id, command } = &request {
                if matches!(command, MovieCommand::Dimensions) {
                    let point = self.movies.dimensions(*id)?;
                    vm.respond_point(point)?;
                    emit(&Event::PointReply {
                        point,
                        simulated: false,
                    })?;
                } else {
                    let value = if let MovieCommand::Open { name, .. } = command {
                        self.movies
                            .open(*id, command, project.read(name)?, resources, host)
                    } else {
                        self.movies.command(*id, command, resources, host)
                    }
                    .with_context(|| {
                        format!(
                            "{}:{:#x}: movie slot {id}: {command:?}",
                            location.scenario, location.offset
                        )
                    })?;
                    vm.respond(value)?;
                    emit(&Event::PlatformReply {
                        value,
                        simulated: host.simulated(),
                    })?;
                }
                return Ok(true);
            }
            if let PlatformRequest::PlayMovies { ids } = &request {
                for id in ids {
                    self.movies
                        .command(*id, &MovieCommand::Play, resources, host)?;
                }
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if matches!(request, PlatformRequest::DirectShowAvailable) {
                // This probes COM support, not the legacy 05b5 movie backend.
                // Keep the script on the portable MPEG path on all hosts.
                vm.respond(0)?;
                emit(&Event::PlatformReply {
                    value: 0,
                    simulated: false,
                })?;
                return Ok(true);
            }
            if matches!(&request, PlatformRequest::ReadRepeatedControls) {
                let mask = host.respond(&PlatformRequest::ReadControls)?;
                let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
                let value = self.control_repeat.query(mask, now);
                vm.respond(value)?;
                emit(&Event::PlatformReply {
                    value,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::ReadRepeatedScanKey { scan } = &request {
                let mask = host.respond(&PlatformRequest::ReadScanKey { scan: *scan })?;
                let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
                let value = self.scan_repeats.entry(*scan).or_default().query(mask, now);
                vm.respond(value)?;
                emit(&Event::PlatformReply {
                    value,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::ImageOrigin { id, frame } = &request {
                let [x, y, _, _] = resources.image_bounds(*id, *frame)?;
                vm.respond_point([x, y])?;
                emit(&Event::PointReply {
                    point: [x, y],
                    simulated: false,
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
            if let PlatformRequest::BindImageMemory { id, address } = &request {
                let memory = vm.share_allocation(*address)?;
                resources.bind_image_memory(*id, *address, memory)?;
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: false,
                })?;
                return Ok(true);
            }
            if let PlatformRequest::LoadSoundMemory { id, address, flags } = &request {
                resources.load_sound(*id, &vm.allocation_bytes(*address)?, *flags)?;
                host.install_sound(
                    *id,
                    resources
                        .sound_buffer(*id)
                        .context("loaded sound missing")?,
                )?;
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: false,
                })?;
                return Ok(true);
            }
            if let PlatformRequest::CreateAudioStream { address, flags } = &request {
                let handle = resources
                    .create_audio_stream(&vm.allocation_bytes(*address)?, *flags)
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
            if let PlatformRequest::FadeAudioStream {
                handle,
                interval,
                step,
                target,
            } = &request
            {
                if *handle != 0 {
                    resources
                        .audio_stream(*handle)
                        .context("unknown audio stream in fade")?;
                    host.fade_stream(*handle, *interval, *step, *target)?;
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
                if matches!(command, SoundCommand::Release) {
                    resources.release_sound(*id);
                }
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
            if let PlatformRequest::LocalCalendar { time } = &request {
                vm.respond_calendar(host.calendar(*time)?)?;
                return Ok(true);
            }
            if let PlatformRequest::OpenFile { name, access } = &request {
                vm.respond_file_open(resources.open_file(project, name, *access)?)?;
                return Ok(true);
            }
            if let PlatformRequest::ReadFile { handle, length, .. } = &request {
                vm.respond_file_read(&resources.read_file(*handle, *length)?)?;
                return Ok(true);
            }
            if let PlatformRequest::WriteFileHandle { handle, .. } = &request {
                let count = resources.write_file_handle(
                    project,
                    *handle,
                    vm.file_write_bytes()?,
                    host.simulated(),
                )?;
                vm.respond(count)?;
                return Ok(true);
            }
            if let PlatformRequest::CloseFile { handle } = &request {
                resources.close_file(*handle)?;
                vm.respond(1)?;
                return Ok(true);
            }
            if let PlatformRequest::SeekFile {
                handle,
                distance,
                origin,
            } = &request
            {
                vm.respond(resources.seek_file(*handle, *distance, *origin)?)?;
                return Ok(true);
            }
            if let PlatformRequest::ReadFlags { name, .. } = &request {
                vm.respond_flags(&resources.read_file_contents(project, name)?)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::WriteFlags { name, .. } = &request {
                resources.write_file(project, name, vm.flag_write_bytes()?, host.simulated())?;
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::WriteFile { name, .. } = &request {
                resources
                    .write_file(project, name, vm.file_write_bytes()?, host.simulated())
                    .with_context(|| {
                        format!("{}:{:#x}: write {name}", location.scenario, location.offset)
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
                if name.to_ascii_lowercase().ends_with(".txt") {
                    vm.detect_text_encoding(&bytes);
                }
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
            if let PlatformRequest::LoadAsset { name } | PlatformRequest::BeginAssetLoad { name } =
                &request
            {
                let bytes = resources.read_asset(project, name).with_context(|| {
                    format!(
                        "{}:{:#x}: load asset {name}",
                        location.scenario, location.offset
                    )
                })?;
                if name.to_ascii_lowercase().ends_with(".txt") {
                    vm.detect_text_encoding(&bytes);
                }
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
            if let PlatformRequest::SetWindowTimer { id, milliseconds } = &request {
                let id = if *id == 0 {
                    (1..=256)
                        .find(|id| !self.window_timers.contains_key(id))
                        .context("window timer slots exhausted")?
                } else {
                    *id
                };
                ensure!(
                    self.window_timers.contains_key(&id) || self.window_timers.len() < 256,
                    "window timer slots exhausted"
                );
                let now = host.respond(&PlatformRequest::ClockMilliseconds)?;
                self.window_timers
                    .insert(id, (now, (*milliseconds).clamp(10, 0x7fffffff)));
                vm.respond(id)?;
                emit(&Event::PlatformReply {
                    value: id,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::KillWindowTimer { id } = &request {
                ensure!(
                    self.window_timers.remove(id).is_some(),
                    "window timer {id} does not exist"
                );
                vm.respond(1)?;
                emit(&Event::PlatformReply {
                    value: 1,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if let PlatformRequest::FindMedia { name } = &request {
                let bytes = if project.loose_path_exists(name)? {
                    b".\\".as_slice()
                } else {
                    &[]
                };
                vm.respond_bytes(bytes)?;
                emit(&Event::PlatformBytesReply {
                    bytes: bytes.to_vec(),
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            if matches!(
                &request,
                PlatformRequest::ProjectDirectory | PlatformRequest::ReadRegistryString { .. }
            ) {
                // Registry paths must be nonempty: scripts use an empty value
                // to detect a missing installation. Keep paths project-relative.
                let bytes: &[u8] = match &request {
                    PlatformRequest::ReadRegistryString { name, .. }
                        if name.eq_ignore_ascii_case("DataPath")
                            && project.directory_exists("savedata")? =>
                    {
                        b"savedata\\"
                    }
                    PlatformRequest::ReadRegistryString { .. } => b".\\",
                    _ => &[],
                };
                emit(&Event::PlatformBytesReply {
                    bytes: bytes.to_vec(),
                    simulated: host.simulated(),
                })?;
                vm.respond_bytes(bytes)?;
                return Ok(true);
            }
            if matches!(&request, PlatformRequest::ReadRegistryValue { .. }) {
                vm.respond(0)?;
                emit(&Event::PlatformReply {
                    value: 0,
                    simulated: host.simulated(),
                })?;
                return Ok(true);
            }
            let resource_reply = resources.respond(project, &request).with_context(|| {
                format!(
                    "{}:{:#x}: resource request {request:?}",
                    location.scenario, location.offset
                )
            })?;
            if matches!(&request, PlatformRequest::ClearSurface { id: 0 }) {
                let rect = [
                    0,
                    0,
                    project.config.width as i32,
                    project.config.height as i32,
                ];
                resources.commit_display(rect);
                host.respond(&PlatformRequest::InvalidateRect { rect })?;
            }
            if let PlatformRequest::BitBlt { copy, .. } = &request
                && copy.destination.id == 0
            {
                let rect = [
                    copy.destination.x,
                    copy.destination.y,
                    copy.destination
                        .x
                        .checked_add(copy.size[0])
                        .context("BitBlt rectangle overflow")?,
                    copy.destination
                        .y
                        .checked_add(copy.size[1])
                        .context("BitBlt rectangle overflow")?,
                ];
                resources.commit_display(rect);
                host.respond(&PlatformRequest::InvalidateRect { rect })?;
            }
            // Bulk drawing modifies the working DIB until SCN invalidates it.
            // Text also submits its own glyph rectangles at TextTick, matching
            // the native text renderer without exposing the rest of the DIB.
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
                u32::from(resources.file_exists(project, path)?)
            } else if let PlatformRequest::CreateDirectory { path } = &request {
                u32::from(resources.create_directory(project, path, host.simulated())?)
            } else if let PlatformRequest::DeleteFile { path } = &request {
                u32::from(resources.delete_file(project, path, host.simulated())?)
            } else {
                if let PlatformRequest::InvalidateRect { rect } = &request {
                    resources.commit_display(*rect);
                }
                host.respond(&request)?
            };
            // A task can remain in a timer retry without reaching another
            // scheduler poll. Movie playback must keep advancing during it.
            if matches!(
                request,
                PlatformRequest::WaitTaskTimer { .. } | PlatformRequest::PumpMessages
            ) {
                self.movies.update(host, resources)?;
            }
            emit(&Event::PlatformReply {
                value,
                simulated: resource_reply.is_none() && host.simulated(),
            })?;
            vm.respond(value)?;
            if let PlatformRequest::WaitTaskTimer { epoch, duration } = request
                && value == 0
            {
                self.state = SessionState::Waiting { epoch, duration };
            }
        }
        Ok(true)
    }
}
