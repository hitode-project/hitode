//! PE startup, Win32 services and software graphics on the shared guest address space.
use std::path::Path;
use std::time::{Duration, Instant};

use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::Backend;
use adv_machine::{Clock, Env, Machine, RunResult, SliceResult};
use adv_os::Process;
use adv_win32::{Win32, Win32Config};

use crate::graphics::{RunControl, install_graphics, register_graphics};
use crate::{Graphics, Snapshot};

/// A running executable. Original game files are read-only; guest writes use `data_dir`.
pub struct Runtime<B: Backend = JitCpu> {
    pub machine: Machine<B>,
    pub(crate) family: crate::family::Family<B>,
}

/// One immutable presentation and its owned asynchronous pixel transfer.
/// The source stays pinned even when the guest presents a newer frame.
#[cfg(feature = "gpu")]
pub struct NativeCapture {
    source: adv_device::gpu::NativeFrame,
    composition: adv_device::surface::PendingFrame,
    readback: adv_device::gpu::Readback,
    completed: bool,
}
#[cfg(feature = "gpu")]
impl NativeCapture {
    pub fn serial(&self) -> u64 {
        self.source.serial
    }
    pub fn width(&self) -> u32 {
        self.source.image.width()
    }
    pub fn height(&self) -> u32 {
        self.source.image.height()
    }
    pub fn take_completion_job(&mut self) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        self.readback.take_completion_job()
    }
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, adv_device::gpu::Error>> {
        let result = self.readback.poll();
        self.completed |= result.is_some();
        result
    }
    pub fn cancel(&mut self) {
        if !self.completed {
            self.readback.cancel();
            self.completed = true;
        }
    }
}
#[cfg(feature = "gpu")]
impl Drop for NativeCapture {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Assemble API descriptors without loading guest code or opening host devices.
/// The same registry drives loading, signature audits and diagnostic tools.
pub fn api_registry(codec: crate::vorbis::Policy) -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    crate::crt::register(&mut registry);
    crate::compat::register(&mut registry);
    register_graphics(&mut registry);
    adv_dx::dsound::register(&mut registry);
    adv_dx::d3dx9::register(&mut registry);
    adv_dx::dshow::register(&mut registry);
    adv_dx::wmvcore::register(&mut registry);
    adv_dx::dxdiag::register(&mut registry);
    adv_dx::dinput::register(&mut registry);
    #[cfg(feature = "gpu")]
    adv_dx::d3d9::register(&mut registry);
    crate::vorbis::register(&mut registry, codec);
    registry
}

/// DirectInput observes the same physical state and foreground window as USER.
pub(crate) fn input_snapshot(
    ext: &adv_machine::Ext,
    objects: &adv_machine::Objects,
    hwnd: u32,
) -> Option<adv_dx::dinput::InputSnapshot> {
    let user = &ext.get::<Win32>().user;
    let window = user.windows.get(objects, &hwnd)?;
    if window.destroying {
        return None;
    }
    let foreground = user.top.iter().copied().find(|top| {
        user.windows
            .get(objects, top)
            .is_some_and(|w| w.visible() && !w.destroying)
    });
    Some(adv_dx::dinput::InputSnapshot {
        keys: user
            .host_keys
            .as_ref()
            .map_or(user.keys, |keys| keys.snapshot()),
        cursor: user.cursor_pos,
        foreground: foreground == Some(user.root(objects, hwnd)),
    })
}

/// Guest root of a game inside the install folder: the path it was installed to.
fn installed_root(game_dir: &Path, install: &Path) -> Result<Option<String>, String> {
    let Ok(installed) = game_dir.strip_prefix(install) else {
        return Ok(None);
    };
    let parts = installed
        .iter()
        .map(|c| c.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or("installed game path is not valid Unicode")?;
    Ok(Some(
        std::iter::once("C:\\Program Files")
            .chain(parts)
            .collect::<Vec<_>>()
            .join("\\"),
    ))
}

impl Runtime<JitCpu> {
    pub fn with_codec(
        exe: impl AsRef<Path>,
        data: impl AsRef<Path>,
        install: Option<&Path>,
        jit: bool,
        width: u32,
        height: u32,
        codec: crate::vorbis::Policy,
    ) -> Result<Self, String> {
        Self::with_cpu_and_codec(
            exe,
            data,
            install,
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit)?,
            width,
            height,
            codec,
        )
    }
    pub fn new(
        exe_path: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
        jit: bool,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        Self::with_cpu(
            exe_path,
            data_dir,
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit)?,
            width,
            height,
        )
    }
}

impl<B: Backend> Runtime<B> {
    /// Supply a CPU backend without changing loader, devices, scheduler or API behavior.
    /// Differential backends are injected by dev-only tests and tools.
    pub fn with_cpu(
        exe_path: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
        cpu: B,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        Self::with_cpu_and_codec(
            exe_path,
            data_dir,
            None,
            cpu,
            width,
            height,
            crate::vorbis::Policy::Guest,
        )
    }

    /// `install` backs guest `C:\Program Files` (see [`adv_os::OsConfig::install_dir`]);
    /// a game inside it runs from the guest path it was installed to.
    pub fn with_cpu_and_codec(
        exe_path: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
        install: Option<&Path>,
        cpu: B,
        width: u32,
        height: u32,
        codec: crate::vorbis::Policy,
    ) -> Result<Self, String> {
        let exe_path = exe_path
            .as_ref()
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let game_dir = exe_path.parent().ok_or("executable has no directory")?;
        let exe_name = exe_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("invalid executable filename")?;
        let mut cfg = Win32Config::new(game_dir, exe_name, data_dir.as_ref());
        if let Some(install) = install {
            let install = install
                .canonicalize()
                .map_err(|e| format!("install folder {}: {e}", install.display()))?;
            if let Some(root) = installed_root(game_dir, &install)? {
                cfg.os.guest_root = root;
            }
            cfg.os.install_dir = Some(install);
        }
        // The initial render canvas is independent of the virtual desktop.
        // Games query the desktop before choosing their own window/backbuffer size.
        let image = std::fs::read(&exe_path).map_err(|e| format!("{}: {e}", exe_path.display()))?;
        cfg.os.codepage = crate::codepage::detect(exe_name, &image);
        cfg.os.write_game = false;
        let files = adv_os::Vfs::new(&cfg.os);
        let selection = crate::vorbis::select_policy_with_reason(&files, exe_name, codec)?;
        let codec = selection.policy;
        let registry = api_registry(codec);
        let mut machine = Machine::new(cpu, registry, Clock::real()).map_err(|e| e.to_string())?;
        machine.ext.insert(selection);
        // An interactive process can make progress while remaining Ready across
        // indefinitely many slices. A cumulative hard run budget must be explicit;
        // step_for still bounds guest execution by its soft slice and quantum.
        machine.set_execution_budget(None);
        Win32::install(&mut machine, cfg).map_err(|e| e.to_string())?;
        machine
            .ext
            .insert(adv_dx::dinput::InputProvider(input_snapshot));
        crate::vorbis::install(&mut machine, codec)?;
        // Load images before allocating the graphics arena, preserving their preferred bases.
        let exe = machine.load(&files, exe_name).map_err(|e| e.to_string())?;
        crate::crt::install(&mut machine)?;
        install_graphics(&mut machine, width, height)?;
        #[cfg(feature = "gpu")]
        adv_dx::d3d9::install(&mut machine, crate::graphics::d3d9_provider())?;
        adv_dx::dsound::install(&mut machine)?;
        adv_dx::dshow::install(&mut machine);
        adv_dx::dxdiag::install(&mut machine);
        machine.start_process(exe).map_err(|e| e.to_string())?;
        let family = crate::family::Family::new(&mut machine, width, height, codec);
        Ok(Self { machine, family })
    }

    /// Install reviewed codec-library bindings after loading, before guest execution.
    pub fn install_codec_kernels(
        &mut self,
        mode: crate::codec_kernels::Mode,
        signatures: &str,
    ) -> Result<crate::codec_kernels::InstallReport, String> {
        crate::codec_kernels::install(&mut self.machine, mode, signatures)
    }

    /// Run the process family until all processes wait, exit or fail.
    pub fn step(&mut self) -> RunResult {
        loop {
            match self.step_family(None) {
                SliceResult::Complete(outcome) => return outcome,
                SliceResult::Yielded => {}
            }
        }
    }

    /// Cooperatively run each process with a bounded slice. Host requests never
    /// escape the supervisor and cannot execute another process inside a trap.
    pub fn step_for(&mut self, slice: Duration) -> SliceResult {
        self.step_family(Some(slice))
    }

    pub(crate) fn finish_step(&mut self, outcome: SliceResult) -> SliceResult {
        if matches!(
            outcome,
            SliceResult::Complete(RunResult::Exited(_) | RunResult::Failed(_))
        ) {
            #[cfg(feature = "gpu")]
            adv_dx::d3d9::cancel_pending(&mut self.machine);
            return outcome;
        }
        let service_started = self.machine.sys.stats.start();
        let video = adv_dx::dshow::pump_video(&mut self.machine);
        self.machine.sys.stats.add_render(service_started);
        if let Err(message) = video {
            return SliceResult::Complete(RunResult::Failed(adv_machine::MachineError {
                ctx: self.machine.sys.current,
                pc: self.machine.cpu.reg(adv_cpu::backend::Reg::Eip),
                message,
                recent: self.machine.sys.recent_calls(),
            }));
        }
        #[cfg(feature = "gpu")]
        let graphics = {
            let service_started = self.machine.sys.stats.start();
            let graphics = adv_dx::d3d9::pump(&mut self.machine);
            self.machine.sys.stats.add_render(service_started);
            graphics
        };
        #[cfg(feature = "gpu")]
        match graphics {
            Ok(true) if matches!(outcome, SliceResult::Complete(RunResult::Idle { .. })) => {
                return SliceResult::Yielded;
            }
            Ok(_) => {}
            Err(message) => {
                return SliceResult::Complete(RunResult::Failed(adv_machine::MachineError {
                    ctx: self.machine.sys.current,
                    pc: self.machine.cpu.reg(adv_cpu::backend::Reg::Eip),
                    message,
                    recent: self.machine.sys.recent_calls(),
                }));
            }
        }
        let service_started = self.machine.sys.stats.start();
        let audio = adv_dx::dsound::pump(&mut self.machine);
        self.machine.sys.stats.add_audio(service_started);
        match audio {
            Ok(Some(at)) => match outcome {
                SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(next_deadline.map_or(at, |old| old.min(at))),
                    })
                }
                other => other,
            },
            Ok(None) => outcome,
            Err(message) => SliceResult::Complete(RunResult::Failed(adv_machine::MachineError {
                ctx: self.machine.sys.current,
                pc: self.machine.cpu.reg(adv_cpu::backend::Reg::Eip),
                message,
                recent: self.machine.sys.recent_calls(),
            })),
        }
    }

    /// Optional PCM device emulation; native device output is owned by the frontend.
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.machine.ext.get_mut::<adv_dx::dsound::Sound>().enabled = enabled;
    }
    /// Attach the shell's surface-compatible GPU before executing guest code.
    #[cfg(feature = "gpu")]
    pub fn install_gpu(&mut self, gpu: adv_device::gpu::Gpu) -> Result<(), String> {
        adv_dx::d3d9::set_gpu(&mut self.machine.ext, gpu)
    }
    /// SM2 is reported by default; SM3 is an explicit opt-in set before guest
    /// code runs.
    #[cfg(feature = "gpu")]
    pub fn set_d3d9_shader_caps(&mut self, sm3: bool) {
        let caps = adv_dx::d3d9::ShaderCaps {
            sm3,
            ..Default::default()
        };
        adv_dx::d3d9::set_shader_caps(&mut self.machine.ext, caps);
    }
    #[cfg(feature = "gpu")]
    pub fn set_d3d9_shader_cache(&mut self, directory: Option<std::path::PathBuf>) {
        adv_dx::d3d9::set_shader_cache(&mut self.machine.ext, directory);
    }
    #[cfg(feature = "gpu")]
    pub fn set_gpu_background(
        &mut self,
        sender: adv_sync::background::LocalSender,
        wake: adv_sync::WakeEvent,
    ) {
        adv_dx::d3d9::set_background(&mut self.machine.ext, sender, wake);
    }
    #[cfg(feature = "gpu")]
    pub fn gpu_diagnostics(&self) -> adv_dx::d3d9::Diagnostics {
        adv_dx::d3d9::diagnostics(&self.machine.ext, &self.machine.sys.objects)
    }

    pub fn audio_stream(&self) -> adv_device::AudioStream {
        self.machine
            .ext
            .get::<adv_dx::dsound::Sound>()
            .audio
            .stream()
    }

    /// Frontend lifecycle pause. Stop/join the device callback before calling
    /// this, and reopen it only after resuming. Emulated audio stays enabled.
    pub fn set_suspended(&mut self, suspended: bool) -> bool {
        if suspended {
            self.machine.sys.clock.pause();
        } else {
            self.machine.sys.clock.resume();
        }
        self.machine
            .ext
            .get::<adv_dx::dsound::Sound>()
            .audio
            .suspend(suspended)
    }

    /// Queue and missing-PCM observations, independent of audible sample values.
    pub fn audio_pcm_metrics(&self) -> adv_device::PcmMetrics {
        self.audio_stream().metrics()
    }
    pub fn audio_source_status(&self) -> crate::AudioSourceStatus {
        self.machine
            .ext
            .get::<adv_dx::dsound::Sound>()
            .source_status(&self.machine.sys.objects, self.machine.sys.clock.now_us())
    }

    /// Hash the first four secondary-buffer starts for decoder differential checks.
    /// Disabled normally: hashing a whole PCM buffer is diagnostic work.
    pub fn enable_audio_pcm_digests(&mut self) {
        self.machine
            .ext
            .get_mut::<adv_dx::dsound::Sound>()
            .capture_digests = true;
    }

    pub fn audio_pcm_digests(&self) -> &[crate::PcmDigest] {
        &self.machine.ext.get::<adv_dx::dsound::Sound>().digests
    }

    /// Last captured completed frame, packed top-down BGRA8. Frontends which enable
    /// deferred capture call `capture_latest_frame` at their display deadline.
    pub fn frame(&self) -> &Snapshot {
        &self.machine.ext.get::<Graphics<Snapshot>>().presenter
    }

    pub fn presented_frames(&self) -> u64 {
        self.machine
            .ext
            .get::<Graphics<Snapshot>>()
            .stats
            .presentations
    }

    pub fn set_deferred_presentation(&mut self, enabled: bool) -> Result<(), String> {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .set_deferred_presentation(enabled)
    }
    pub fn set_frame_mailbox(
        &mut self,
        mailbox: crate::graphics::FrameProducer,
        wake: adv_sync::WakeEvent,
    ) {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .set_frame_mailbox(mailbox, wake);
        // The UI consumes published frames independently; returning after every
        // intermediate guest frame would only repeat the outer service loop.
        self.machine.set_frame_handoff(false);
    }
    /// Explicit bounded, read-only diagnostics at a completed execution boundary.
    pub fn inspect_memory(&self, address: u32, len: u32) -> Result<Vec<u8>, String> {
        if !(1..=64).contains(&len) {
            return Err("diagnostic memory length must be 1..=64".into());
        }
        let mut bytes = vec![0; len as usize];
        self.machine
            .cpu
            .read(address, &mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    pub fn set_host_keys(&mut self, keys: std::sync::Arc<adv_sync::input::KeyState>) {
        self.machine.ext.get_mut::<Win32>().user.host_keys = Some(keys);
    }

    /// Opt-in receipt observation for ordered diagnostic control. It does not
    /// alter guest queues, physical key state or scheduling.
    pub fn enable_input_receipts(&mut self) {
        self.machine
            .ext
            .get_mut::<Win32>()
            .user
            .input_tracking
            .get_or_insert_with(Default::default);
    }
    pub fn latest_input_receipt(&self) -> u64 {
        self.machine
            .ext
            .get::<Win32>()
            .user
            .input_tracking
            .as_ref()
            .map_or(0, |tracking| tracking.latest())
    }
    pub fn input_receipt_completed(&mut self, receipt: u64, full_or_idle_boundary: bool) -> bool {
        let frame = self.machine.sys.frame;
        let context = self
            .machine
            .ext
            .get::<Win32>()
            .user
            .input_tracking
            .as_ref()
            .and_then(|tracking| tracking.context(receipt));
        let target_boundary = context.is_some_and(|ctx| {
            self.machine
                .sys
                .contexts
                .get(ctx as usize)
                .is_none_or(|context| !context.is_ready())
                || (full_or_idle_boundary && self.machine.sys.current == ctx)
        });
        self.machine
            .ext
            .get_mut::<Win32>()
            .user
            .input_tracking
            .as_mut()
            .is_none_or(|tracking| {
                tracking.fence_reached(receipt, frame, full_or_idle_boundary, target_boundary)
            })
    }

    pub fn has_pending_frame(&self) -> bool {
        self.machine
            .ext
            .get::<Graphics<Snapshot>>()
            .has_pending_frame()
    }

    pub fn capture_latest_frame(&mut self) -> Result<bool, String> {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .capture_latest(&crate::graphics::guest_memory(&mut self.machine.cpu))
    }
    #[cfg(feature = "gpu")]
    pub fn has_native_frame(&self) -> bool {
        self.machine
            .ext
            .get::<Graphics<Snapshot>>()
            .latest_native_frame()
            .is_some()
    }
    #[cfg(feature = "gpu")]
    pub fn native_frame_serial(&self) -> Option<u64> {
        self.machine
            .ext
            .get::<Graphics<Snapshot>>()
            .latest_native_frame()
            .map(|frame| frame.serial)
    }
    /// Frontend readback which may finish after a newer presentation. The token
    /// retains its immutable source, independently of the latest-frame mailbox.
    #[cfg(feature = "gpu")]
    pub fn begin_native_frame_capture(
        &mut self,
    ) -> Result<Option<NativeCapture>, adv_device::gpu::Error> {
        let Some(composition) = self
            .machine
            .ext
            .get::<Graphics<Snapshot>>()
            .latest_frame()
            .filter(|frame| frame.native().is_some())
            .cloned()
        else {
            return Ok(None);
        };
        let source = composition.native().unwrap().clone();
        let readback = adv_dx::d3d9::readback_image(&mut self.machine.ext, &source.image)?;
        Ok(Some(NativeCapture {
            source,
            composition,
            readback,
            completed: false,
        }))
    }
    #[cfg(feature = "gpu")]
    pub fn finish_native_frame_capture(
        &mut self,
        capture: &NativeCapture,
        bgra: &[u8],
    ) -> Result<bool, String> {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .capture_native_frame_pixels(&capture.composition, bgra)
    }
    /// Explicit export/diagnostic capture. Callers enqueue its fence job on the
    /// existing background service and consume the typed completion separately.
    #[cfg(feature = "gpu")]
    pub fn begin_native_capture(
        &mut self,
    ) -> Result<Option<(u64, adv_device::gpu::Readback)>, adv_device::gpu::Error> {
        // A report followed by a capture can observe the same presentation.
        // Its pixels are already retained; a duplicate readback would be
        // rejected by the monotonic capture serial guard as superseded.
        if !self
            .machine
            .ext
            .get::<Graphics<Snapshot>>()
            .has_pending_frame()
        {
            return Ok(None);
        }
        let Some(frame) = self
            .machine
            .ext
            .get::<Graphics<Snapshot>>()
            .latest_native_frame()
            .cloned()
        else {
            return Ok(None);
        };
        adv_dx::d3d9::readback_image(&mut self.machine.ext, &frame.image)
            .map(|ticket| Some((frame.serial, ticket)))
    }
    #[cfg(feature = "gpu")]
    pub fn finish_native_capture(
        &mut self,
        serial: u64,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> Result<bool, String> {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .capture_native_pixels(serial, width, height, bgra)
    }

    /// End a bounded diagnostic run immediately after this presentation count.
    /// This deliberately terminates the run and is not a natural guest exit.
    pub fn set_frame_limit(&mut self, limit: Option<u64>) {
        self.machine.ext.get_mut::<RunControl>().frame_limit = limit;
    }

    pub fn first_frame_time(&self) -> Option<Instant> {
        self.machine.ext.get::<RunControl>().first_frame
    }

    /// Measure guest presentation boundaries, including multiple frames in one step.
    pub fn enable_frame_timing(&mut self) {
        self.machine.ext.get_mut::<RunControl>().frame_timing = true;
    }

    pub fn frame_intervals(&self) -> &[Duration] {
        &self.machine.ext.get::<RunControl>().frame_intervals
    }

    /// Headless dialog policy; `None` leaves dialogs to the host frontend.
    pub fn set_auto_dialog_ms(&mut self, delay: Option<u32>) {
        let win32 = self.machine.ext.get_mut::<Win32>();
        win32.cfg.auto_dialog_ms = delay;
        win32.user.auto_dialog_ms = delay;
    }
    /// ANSI decoding for paths, text and font names; configure before the first step.
    /// The guest ANSI code page.
    pub fn codepage(&self) -> u32 {
        self.machine.ext.get::<Win32>().codec.acp
    }

    pub fn set_codepage(&mut self, codepage: u32) -> Result<(), String> {
        adv_win32::set_codepage(&mut self.env(), codepage).map_err(|error| error.to_string())
    }

    /// Attach media before execution; child processes inherit the same immutable media.
    pub fn mount_iso(&mut self, drive: char, image: &Path) -> Result<(), String> {
        self.machine
            .ext
            .get_mut::<Win32>()
            .vfs
            .mount_iso(drive, image)
            .map_err(|error| format!("mount optical drive {drive}: {error}"))
    }

    /// Post host input to the window that would receive it (see
    /// [`adv_win32::user32::host_input::post`]).
    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> bool {
        let accepted =
            adv_win32::user32::host_input::post(&mut self.env(), message, wparam, lparam);
        if accepted {
            adv_dx::dinput::post_input(&mut self.env(), message, wparam, lparam);
        }
        accepted
    }

    /// Apply host-selected fonts before guest execution begins.
    pub fn configure_fonts(
        &mut self,
        overrides: adv_device::text::Overrides,
        paths: &[std::path::PathBuf],
    ) -> Result<(), String> {
        use std::io::Read;
        let mut remaining = adv_device::text::MAX_FONT_RESOURCE_BYTES;
        let mut files = Vec::new();
        for path in paths {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(remaining as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            remaining = remaining
                .checked_sub(bytes.len())
                .ok_or("font resources exceed the byte limit")?;
            files.push(bytes);
        }
        adv_win32::gdi32::font::configure(&mut self.machine, overrides, &files)
    }
    /// Post text committed by a host input method to the keyboard focus (see
    /// [`adv_win32::user32::host_input::post_text`]); returns the characters posted.
    pub fn post_text(&mut self, text: &str) -> usize {
        adv_win32::user32::host_input::post_text(&mut self.env(), text)
    }

    /// Immutable native-menu projection; this is called only by execution.
    pub fn menu_key(&self) -> crate::MenuKey {
        adv_win32::user32::menu::presentation_key(
            &self.machine.ext.get::<Win32>().user,
            &self.machine.sys.objects,
        )
    }
    pub fn menu_bar(&self) -> Result<crate::MenuBar, String> {
        adv_win32::user32::menu::presentation(
            &self.machine.ext.get::<Win32>().user,
            &self.machine.sys.objects,
        )
    }
    pub fn post_menu_event(&mut self, event: crate::MenuEvent) -> bool {
        adv_win32::user32::menu::post_host_event(&mut self.env(), event)
    }

    /// Immutable dialog presentation, obtained only by the execution owner.
    pub fn dialogs(&self) -> Result<crate::DialogSet, String> {
        adv_win32::user32::dialog::presentation(
            self.machine.ext.get::<Win32>(),
            &self.machine.sys.objects,
        )
    }
    pub fn post_dialog_event(&mut self, event: crate::DialogEvent) -> bool {
        adv_win32::user32::dialog::post_host_event(&mut self.env(), event)
    }

    /// Release held keys and buttons when the host loses focus.
    pub fn release_input(&mut self) -> usize {
        let released = adv_win32::user32::host_input::release(&mut self.env());
        adv_dx::dinput::release_input(&mut self.env());
        released
    }

    /// Handler context outside any guest call, for host-originated work.
    fn env(&mut self) -> Env<'_> {
        Env {
            context: self.machine.sys.current,
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_games_run_from_their_install_path() {
        let install = Path::new("/library/install");
        assert_eq!(
            installed_root(Path::new("/library/install/Co/ゲーム"), install),
            Ok(Some("C:\\Program Files\\Co\\ゲーム".into()))
        );
        assert_eq!(
            installed_root(install, install),
            Ok(Some("C:\\Program Files".into()))
        );
        assert_eq!(
            installed_root(Path::new("/library/other"), install),
            Ok(None)
        );
    }

    fn looping_executable_runtime() -> Runtime<JitCpu> {
        // A standalone authored PE exercises with_cpu, loader and startup policy
        // without sample files, imported services or an external compiler.
        let mut pe = vec![0u8; 1024];
        let put16 = |pe: &mut [u8], offset: usize, value: u16| {
            pe[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put32 = |pe: &mut [u8], offset: usize, value: u32| {
            pe[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put16(&mut pe, 0, 0x5a4d);
        put32(&mut pe, 0x3c, 0x80);
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        for (offset, value) in [
            (0x84, 0x14c),
            (0x86, 1),
            (0x94, 224),
            (0x96, 0x102),
            (0x98, 0x10b),
        ] {
            put16(&mut pe, offset, value);
        }
        for (offset, value) in [
            (0x9c, 0x200),
            (0xa8, 0x1000),
            (0xac, 0x1000),
            (0xb4, 0x0040_0000),
            (0xb8, 4096),
            (0xbc, 512),
            (0xd0, 0x2000),
            (0xd4, 0x200),
            (0xe0, 0x100000),
            (0xe4, 4096),
            (0xe8, 0x100000),
            (0xec, 4096),
            (0xf4, 16),
            (0x180, 2),
            (0x184, 0x1000),
            (0x188, 0x200),
            (0x18c, 0x200),
            (0x19c, 0x60000020),
        ] {
            put32(&mut pe, offset, value);
        }
        put16(&mut pe, 0xc0, 4);
        put16(&mut pe, 0xc8, 4);
        put16(&mut pe, 0xdc, 3);
        pe[0x178..0x17d].copy_from_slice(b".text");
        pe[0x200..0x202].copy_from_slice(&[0xeb, 0xfe]); // jmp $
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("adv-runtime-budget-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let exe = directory.join("loop.exe");
        std::fs::write(&exe, pe).unwrap();
        let runtime = Runtime::with_cpu(
            &exe,
            directory.join("overlay"),
            JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
            4,
            2,
        )
        .unwrap();
        std::fs::remove_file(exe).unwrap();
        // Loader owns the mapped image now; remove only the known empty directories.
        let _ = std::fs::remove_dir(directory.join("overlay"));
        let _ = std::fs::remove_dir(directory);
        runtime
    }

    #[test]
    fn initial_canvas_does_not_resize_the_guest_desktop() {
        let runtime = looping_executable_runtime();
        let win32 = runtime.machine.ext.get::<Win32>();
        let desktop = win32
            .user
            .windows
            .get(&runtime.machine.sys.objects, &win32.user.desktop)
            .unwrap();
        assert_eq!(
            (desktop.rect.width(), desktop.rect.height()),
            win32.cfg.screen
        );
        assert_eq!(win32.cfg.screen, (1920, 1080));
        assert_eq!((runtime.frame().width, runtime.frame().height), (4, 2));
    }

    #[test]
    fn runtime_default_has_no_cumulative_budget_and_returns_bounded_slices() {
        let mut runtime = looping_executable_runtime();
        assert_eq!(runtime.machine.cpu.execution_budget(), None);
        for _ in 0..4 {
            assert!(matches!(
                runtime.step_for(Duration::from_millis(2)),
                SliceResult::Yielded
            ));
        }
        assert_eq!(runtime.machine.sys.context(0).state_name(), "Ready");
    }

    #[test]
    fn runtime_explicit_budget_still_fails_across_soft_slices() {
        let mut runtime = looping_executable_runtime();
        // Many 2 ms slices fit the budget even after a slow first (debug) compile.
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_millis(200)));
        let started = Instant::now();
        let mut yielded = false;
        loop {
            assert!(started.elapsed() < Duration::from_secs(3));
            match runtime.step_for(Duration::from_millis(2)) {
                SliceResult::Yielded => yielded = true,
                SliceResult::Complete(RunResult::Failed(error)) => {
                    assert!(error.message.contains("execution budget exhausted"));
                    assert_eq!(error.ctx, 0);
                    assert!(
                        yielded,
                        "soft slicing did not retain the explicit allowance"
                    );
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
    }

    fn input_runtime() -> Runtime {
        use adv_win32::user32::window::{Rect, WS_CHILD, WS_POPUP, WS_VISIBLE, Window};
        let mut machine = crate::graphics::test_machine();
        Win32::install(
            &mut machine,
            Win32Config::new(std::env::temp_dir(), "input.exe", std::env::temp_dir()),
        )
        .unwrap();
        machine
            .ext
            .insert(adv_dx::dinput::InputProvider(input_snapshot));
        let window = |hwnd, parent, rect| {
            Window::new(
                hwnd,
                0,
                0,
                WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
                0,
                parent,
                rect,
                0,
            )
        };
        let user = &mut machine.ext.get_mut::<Win32>().user;
        user.top = vec![0x10020];
        assert!(user.windows.insert(
            &mut machine.sys.objects,
            0x10020,
            window(0x10020, 0, Rect::new(100, 50, 800, 600)),
        ));
        assert!(user.windows.insert(
            &mut machine.sys.objects,
            0x10024,
            window(0x10024, 0x10020, Rect::new(20, 30, 100, 50)),
        ));
        assert!(user.windows.insert(
            &mut machine.sys.objects,
            0x10028,
            window(0x10028, 0x10020, Rect::new(40, 50, 100, 50)),
        ));
        user.queue(0).focus = 0x10024;
        user.queue(0).capture = 0x10028;
        let family = crate::family::Family::new(&mut machine, 4, 2, crate::vorbis::Policy::Guest);
        Runtime { machine, family }
    }

    #[test]
    fn legacy_input_observes_live_keys_and_routes_buffered_transitions_through_runtime() {
        use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
        use adv_machine::Outcome;
        let mut runtime = input_runtime();
        adv_dx::dinput::register(
            runtime
                .machine
                .sys
                .thunks
                .resolver_mut::<Registry>()
                .unwrap(),
        );
        let data = runtime
            .machine
            .sys
            .vm
            .alloc(&mut runtime.machine.cpu, 4096, Prot::RW)
            .unwrap();
        fn call(runtime: &mut Runtime, data: u32, name: &str, args: &[u32]) -> u32 {
            runtime.machine.resolve_import("dinput.dll", name).unwrap();
            let thunk = runtime
                .machine
                .sys
                .thunks
                .iter()
                .find(|(_, t)| t.module == "dinput.dll" && t.name == name)
                .unwrap()
                .0;
            let handler = runtime.machine.sys.thunks.get(thunk).f.handler;
            runtime.machine.cpu.set_reg(Reg::Esp, data + 3500);
            for (index, arg) in args.iter().enumerate() {
                runtime
                    .machine
                    .cpu
                    .write_u32(data + 3504 + index as u32 * 4, *arg)
                    .unwrap();
            }
            let mut env = runtime.env();
            env.thunk = thunk;
            match handler(&mut env) {
                Outcome::Return(value) => value,
                _ => panic!("unexpected guest callback"),
            }
        }
        assert_eq!(
            call(
                &mut runtime,
                data,
                "DirectInputCreateA",
                &[0x400000, 0x500, data, 0]
            ),
            0
        );
        let factory = runtime.machine.cpu.read_u32(data).unwrap();
        runtime
            .machine
            .cpu
            .write(
                data + 32,
                &[
                    0x61, 0x2b, 0x1d, 0x6f, 0xa0, 0xd5, 0xcf, 0x11, 0xbf, 0xc7, 0x44, 0x45, 0x53,
                    0x54, 0, 0,
                ],
            )
            .unwrap();
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputA::CreateDevice",
                &[factory, data + 32, data + 4, 0]
            ),
            0
        );
        let device = runtime.machine.cpu.read_u32(data + 4).unwrap();
        for (offset, words) in [
            (128, vec![24, 16, 2, 1, 1, data + 160]),
            (160, vec![0, 0, 0x1e0c, 0]),
            (192, vec![20, 16, 0, 0, 4]),
        ] {
            for (index, word) in words.into_iter().enumerate() {
                runtime
                    .machine
                    .cpu
                    .write_u32(data + offset + index as u32 * 4, word)
                    .unwrap();
            }
        }
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::SetDataFormat",
                &[device, data + 128]
            ),
            0
        );
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::SetCooperativeLevel",
                &[device, 0x10020, 6]
            ),
            0
        );
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::SetProperty",
                &[device, 1, data + 192]
            ),
            0
        );
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::Acquire",
                &[device]
            ),
            0
        );
        let live = std::sync::Arc::new(adv_sync::input::KeyState::default());
        runtime.set_host_keys(live.clone());
        live.set(0x41, true);
        live.set(0x41, false);
        assert!(runtime.post_input(0x100, 0x41, 0x1e0001));
        assert!(runtime.post_input(0x101, 0x41, 0xc01e0001));
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::GetDeviceState",
                &[device, 1, data + 256]
            ),
            0
        );
        assert_eq!(runtime.machine.cpu.read_u8(data + 256).unwrap(), 0);
        runtime.machine.cpu.write_u32(data + 224, 4).unwrap();
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::GetDeviceData",
                &[device, 16, data + 256, data + 224, 0]
            ),
            0
        );
        assert_eq!(runtime.machine.cpu.read_u32(data + 224).unwrap(), 2);
        assert_eq!(runtime.machine.cpu.read_u32(data + 260).unwrap(), 0x80);
        assert_eq!(runtime.machine.cpu.read_u32(data + 276).unwrap(), 0);
        runtime.release_input();
        assert_eq!(
            call(
                &mut runtime,
                data,
                "IDirectInputDeviceA::GetDeviceState",
                &[device, 1, data + 256]
            ),
            0x8007000c
        );
    }

    #[test]
    fn keyboard_routes_to_valid_focus_and_falls_back_for_hidden_or_missing_focus() {
        let mut runtime = input_runtime();
        assert!(runtime.post_input(0x100, 0x41, 1));
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        assert_eq!(user.queue(0).posted.pop_front().unwrap().hwnd, 0x10024);
        assert_eq!(user.keys[0x41] & 0x80, 0x80);
        user.windows
            .get_mut(&mut runtime.machine.sys.objects, &0x10024)
            .unwrap()
            .style &= !adv_win32::user32::window::WS_VISIBLE;
        assert!(runtime.post_input(0x101, 0x41, 0xc0000001));
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        assert_eq!(user.queue(0).posted.pop_front().unwrap().hwnd, 0x10020);
        assert_eq!(user.keys[0x41] & 0x80, 0);
        user.queue(0).focus = 0xdeadbeef;
        assert!(runtime.post_input(0x102, u32::from(b'a'), 1));
        assert_eq!(
            runtime
                .machine
                .ext
                .get_mut::<Win32>()
                .user
                .queue(0)
                .posted
                .pop_front()
                .unwrap()
                .hwnd,
            0x10020
        );
    }

    // spec: WIN32-USER-16
    #[test]
    fn mouse_coordinates_round_trip_over_the_whole_displayed_client() {
        use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
        use adv_win32::user32::window::{
            Rect, WS_CAPTION, WS_POPUP, WS_THICKFRAME, WS_VISIBLE, make_long,
        };
        let mut runtime = input_runtime();
        let data = runtime
            .machine
            .sys
            .vm
            .alloc(&mut runtime.machine.cpu, 4096, Prot::RW)
            .unwrap();
        let invoke = |runtime: &mut Runtime, name: &str, args: &[u32]| {
            let address = runtime.machine.resolve_import("user32.dll", name).unwrap();
            let thunk =
                (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let handler = runtime.machine.sys.thunks.get(thunk).f.handler;
            runtime.machine.cpu.set_reg(Reg::Esp, data + 2048);
            for (i, &arg) in args.iter().enumerate() {
                runtime
                    .machine
                    .cpu
                    .write_u32(data + 2052 + i as u32 * 4, arg)
                    .unwrap();
            }
            let mut env = runtime.env();
            env.thunk = thunk;
            assert!(matches!(handler(&mut env), adv_machine::Outcome::Return(1)));
        };
        for style in [
            WS_POPUP | WS_VISIBLE,
            WS_CAPTION | WS_THICKFRAME | WS_VISIBLE,
        ] {
            for (left, top) in [(-80, -60), (560, 240), (1500, 900)] {
                let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
                let window = user
                    .windows
                    .get_mut(&mut runtime.machine.sys.objects, &0x10020)
                    .unwrap();
                window.rect = Rect::new(left, top, 800, 600);
                window.style = style;
                let (width, height) = window.client_size();
                user.queue(0).capture = 0;
                let origin = user.client_origin(&runtime.machine.sys.objects, 0x10020);
                for (x, y) in [(0, 0), (400, 300), (-12, -34)] {
                    let packed = make_long(x, y);
                    // The host shows the whole client, so the pointer reaches it
                    // past the (0, 0)-(1920, 1080) display, but not beyond both.
                    let expected_screen = (
                        (x + origin.0).clamp(origin.0.min(0), (origin.0 + width).max(1920) - 1),
                        (y + origin.1).clamp(origin.1.min(0), (origin.1 + height).max(1080) - 1),
                    );
                    let local = (expected_screen.0 - origin.0, expected_screen.1 - origin.1);
                    assert!(runtime.post_input(0x200, 0, packed));
                    let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
                    let message = user.queue(0).posted.pop_front().unwrap();
                    assert_eq!(
                        (message.hwnd, message.lparam),
                        (0x10020, make_long(local.0, local.1))
                    );
                    let screen = user.cursor_pos;
                    assert_eq!(screen, expected_screen);
                    invoke(&mut runtime, "GetCursorPos", &[data]);
                    assert_eq!(
                        (
                            runtime.machine.cpu.read_u32(data).unwrap() as i32,
                            runtime.machine.cpu.read_u32(data + 4).unwrap() as i32
                        ),
                        screen
                    );
                    invoke(&mut runtime, "ScreenToClient", &[0x10020, data]);
                    assert_eq!(
                        (
                            runtime.machine.cpu.read_u32(data).unwrap() as i32,
                            runtime.machine.cpu.read_u32(data + 4).unwrap() as i32
                        ),
                        local
                    );
                    invoke(&mut runtime, "ClientToScreen", &[0x10020, data]);
                    assert_eq!(
                        (
                            runtime.machine.cpu.read_u32(data).unwrap() as i32,
                            runtime.machine.cpu.read_u32(data + 4).unwrap() as i32
                        ),
                        screen
                    );
                    assert!(runtime.post_input(0x20a, 120 << 16, packed));
                    let wheel = runtime
                        .machine
                        .ext
                        .get_mut::<Win32>()
                        .user
                        .queue(0)
                        .posted
                        .pop_front()
                        .unwrap();
                    assert_eq!(wheel.lparam, make_long(screen.0, screen.1));
                }
                runtime.machine.ext.get_mut::<Win32>().user.queue(0).capture = 0x10028;
                let child_origin = runtime
                    .machine
                    .ext
                    .get::<Win32>()
                    .user
                    .client_origin(&runtime.machine.sys.objects, 0x10028);
                let screen = (60 + origin.0, 80 + origin.1);
                assert!(runtime.post_input(0x200, 0, make_long(60, 80)));
                let message = runtime
                    .machine
                    .ext
                    .get_mut::<Win32>()
                    .user
                    .queue(0)
                    .posted
                    .pop_front()
                    .unwrap();
                assert_eq!(
                    (message.hwnd, message.lparam),
                    (
                        0x10028,
                        make_long(screen.0 - child_origin.0, screen.1 - child_origin.1)
                    )
                );
            }
        }
    }

    #[test]
    fn mouse_capture_receives_child_coordinates_and_missing_capture_falls_back() {
        let mut runtime = input_runtime();
        let top_position = adv_win32::user32::window::make_long(60, 80);
        assert!(runtime.post_input(0x200, 0, top_position));
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        let message = user.queue(0).posted.pop_front().unwrap();
        assert_eq!(message.hwnd, 0x10028);
        assert_eq!(message.lparam, adv_win32::user32::window::make_long(20, 30));
        assert_eq!(user.cursor_pos, (160, 130));
        assert!(runtime.post_input(0x20a, 120 << 16, top_position));
        assert_eq!(
            runtime
                .machine
                .ext
                .get_mut::<Win32>()
                .user
                .queue(0)
                .posted
                .pop_front()
                .unwrap()
                .lparam,
            adv_win32::user32::window::make_long(160, 130)
        );
        runtime.machine.ext.get_mut::<Win32>().user.queue(0).capture = 0xdeadbeef;
        assert!(runtime.post_input(0x200, 0, top_position));
        let message = runtime
            .machine
            .ext
            .get_mut::<Win32>()
            .user
            .queue(0)
            .posted
            .pop_front()
            .unwrap();
        assert_eq!((message.hwnd, message.lparam), (0x10020, top_position));
    }

    #[test]
    fn focus_loss_clears_held_state_preserves_toggles_and_posts_releases_once() {
        let mut runtime = input_runtime();
        assert!(runtime.post_input(0x100, 0x41, 1));
        assert!(runtime.post_input(0x104, 0x12, 1));
        assert!(runtime.post_input(0x201, 1, adv_win32::user32::window::make_long(60, 80)));
        let event = runtime
            .machine
            .sys
            .objects
            .insert(adv_machine::Object::Event {
                manual_reset: false,
                signaled: false,
            });
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        user.queue(0).posted.clear();
        user.queue(0).event = event;
        user.keys[0x14] = 0x81;
        assert_eq!(runtime.release_input(), 4);
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        assert!(user.keys.iter().all(|key| key & 0x80 == 0));
        assert_eq!(user.keys[0x14], 1);
        assert_eq!(user.queue(0).capture, 0);
        assert_eq!(user.queue(0).focus, 0x10024);
        let releases: Vec<_> = user
            .queue(0)
            .posted
            .iter()
            .map(|m| (m.hwnd, m.msg, m.wparam))
            .collect();
        assert_eq!(
            releases,
            vec![
                (0x10028, 0x202, 0),
                (0x10024, 0x105, 0x12),
                (0x10024, 0x101, 0x14),
                (0x10024, 0x101, 0x41)
            ]
        );
        assert!(matches!(
            runtime.machine.sys.objects.get(event),
            Some(adv_machine::Object::Event { signaled: true, .. })
        ));
        assert_eq!(runtime.release_input(), 0);
        assert_eq!(
            runtime
                .machine
                .ext
                .get_mut::<Win32>()
                .user
                .queue(0)
                .posted
                .len(),
            4
        );
    }
}
