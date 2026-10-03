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
}

/// Assemble API descriptors without loading guest code or opening host devices.
/// The same registry drives loading, signature audits and diagnostic tools.
pub fn api_registry(codec: crate::vorbis::Policy) -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    crate::crt::register(&mut registry);
    crate::compat::register(&mut registry);
    register_graphics(&mut registry);
    crate::font::register(&mut registry);
    adv_dx::dsound::register(&mut registry);
    crate::vorbis::register(&mut registry, codec);
    registry
}

impl Runtime<JitCpu> {
    pub fn with_codec(
        exe: impl AsRef<Path>,
        data: impl AsRef<Path>,
        jit: bool,
        width: u32,
        height: u32,
        codec: crate::vorbis::Policy,
    ) -> Result<Self, String> {
        Self::with_cpu_and_codec(
            exe,
            data,
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
            cpu,
            width,
            height,
            crate::vorbis::Policy::Guest,
        )
    }

    pub fn with_cpu_and_codec(
        exe_path: impl AsRef<Path>,
        data_dir: impl AsRef<Path>,
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
        cfg.screen = (width as i32, height as i32);
        cfg.os.codepage = if exe_name.to_ascii_lowercase().contains("chs") {
            936
        } else {
            932
        };
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
        crate::vorbis::install(&mut machine, codec)?;
        // Load images before allocating the graphics arena, preserving their preferred bases.
        let exe = machine.load(&files, exe_name).map_err(|e| e.to_string())?;
        crate::crt::install(&mut machine)?;
        install_graphics(&mut machine, width, height)?;
        crate::font::install(&mut machine);
        adv_dx::dsound::install(&mut machine)?;
        machine.start_process(exe).map_err(|e| e.to_string())?;
        Ok(Self { machine })
    }

    /// Run until every context waits, the process exits, or a named failure occurs.
    pub fn step(&mut self) -> RunResult {
        let outcome = self.machine.run();
        match self.finish_step(SliceResult::Complete(outcome)) {
            SliceResult::Complete(outcome) => outcome,
            SliceResult::Yielded => unreachable!("an unsliced step cannot soft-yield"),
        }
    }

    /// Resume guest work for approximately `slice`, then service PCM even when guest
    /// contexts remain runnable. Synchronous compilation/handlers can exceed this target.
    pub fn step_for(&mut self, slice: Duration) -> SliceResult {
        let outcome = self.machine.run_slice(slice);
        self.finish_step(outcome)
    }

    fn finish_step(&mut self, outcome: SliceResult) -> SliceResult {
        if matches!(
            outcome,
            SliceResult::Complete(RunResult::Exited(_) | RunResult::Failed(_))
        ) {
            return outcome;
        }
        match adv_dx::dsound::pump(&mut self.machine) {
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

    pub fn audio_stream(&self) -> adv_device::AudioStream {
        self.machine
            .ext
            .get::<adv_dx::dsound::Sound>()
            .audio
            .stream()
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
            .capture_latest()
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
    pub fn set_codepage(&mut self, codepage: u32) {
        let win32 = self.machine.ext.get_mut::<Win32>();
        win32.cfg.os.codepage = codepage;
        win32.codec = adv_os::Codec::new(codepage);
    }

    /// Post host input to the window that would receive it (see
    /// [`adv_win32::user32::host_input::post`]).
    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> bool {
        adv_win32::user32::host_input::post(&mut self.env(), message, wparam, lparam)
    }

    /// Release held keys and buttons when the host loses focus.
    pub fn release_input(&mut self) -> usize {
        adv_win32::user32::host_input::release(&mut self.env())
    }

    /// Handler context outside any guest call, for host-originated work.
    fn env(&mut self) -> Env<'_> {
        Env {
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
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_millis(12)));
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
        use adv_win32::user32::window::{
            ControlState, Rect, WS_CHILD, WS_POPUP, WS_VISIBLE, Window,
        };
        let mut machine = crate::graphics::test_machine();
        Win32::install(
            &mut machine,
            Win32Config::new(std::env::temp_dir(), "input.exe", std::env::temp_dir()),
        )
        .unwrap();
        let window = |hwnd, parent, rect| Window {
            hwnd,
            class: 0,
            proc: 0,
            style: WS_VISIBLE | if parent == 0 { WS_POPUP } else { WS_CHILD },
            ex_style: 0,
            parent,
            owner: 0,
            id: 0,
            instance: 0,
            rect,
            text: String::new(),
            extra: Vec::new(),
            user_data: 0,
            thread: 0,
            children: Vec::new(),
            font: 0,
            needs_paint: false,
            created_us: 0,
            initial_geometry_reported: false,
            destroying: false,
            control: ControlState::default(),
            auto_clicks: 0,
        };
        let user = &mut machine.ext.get_mut::<Win32>().user;
        user.top = vec![0x10020];
        user.windows
            .insert(0x10020, window(0x10020, 0, Rect::new(100, 50, 800, 600)));
        user.windows.insert(
            0x10024,
            window(0x10024, 0x10020, Rect::new(20, 30, 100, 50)),
        );
        user.windows.insert(
            0x10028,
            window(0x10028, 0x10020, Rect::new(40, 50, 100, 50)),
        );
        user.queue(0).focus = 0x10024;
        user.queue(0).capture = 0x10028;
        Runtime { machine }
    }

    #[test]
    fn keyboard_routes_to_valid_focus_and_falls_back_for_hidden_or_missing_focus() {
        let mut runtime = input_runtime();
        assert!(runtime.post_input(0x100, 0x41, 1));
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        assert_eq!(user.queue(0).posted.pop_front().unwrap().hwnd, 0x10024);
        assert_eq!(user.keys[0x41] & 0x80, 0x80);
        user.windows.get_mut(&0x10024).unwrap().style &= !adv_win32::user32::window::WS_VISIBLE;
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
