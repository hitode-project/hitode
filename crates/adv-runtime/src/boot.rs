//! PE startup, Win32 services and software graphics on the shared guest address space.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use adv_jit::pal::JitCpu;
use pal_cpu::{Backend, GuestExt, Prot};
use pal_machine::{Clock, Conv, Env, Machine, Outcome, Registry, RunResult, SliceResult};
use pal_win32::{Win32, Win32Config};

use crate::graphics::{API_SPECS, DEVICE_METHODS, GuestMemory, SURFACE_METHODS};
use crate::{Graphics, Snapshot, api};

/// A running executable. Original game files are read-only; guest writes use `data_dir`.
pub struct Runtime<B: Backend = JitCpu> {
    pub machine: Machine<B>,
}

#[derive(Default)]
struct RunControl {
    frame_limit: Option<u64>,
    first_frame: Option<Instant>,
    last_frame: Option<Instant>,
    frame_timing: bool,
    frame_intervals: Vec<Duration>,
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
            JitCpu::new(pal_machine::layout::PARK_ADDR, jit)?,
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
            JitCpu::new(pal_machine::layout::PARK_ADDR, jit)?,
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
        cfg.codepage = if exe_name.to_ascii_lowercase().contains("chs") {
            936
        } else {
            932
        };
        cfg.write_game = false;
        let files = pal_win32::Vfs::new(&cfg);
        let selection = crate::vorbis::select_policy_with_reason(&files, exe_name, codec)?;
        let codec = selection.policy;
        let mut registry = Registry::default();
        pal_win32::register(&mut registry);
        crate::crt::register(&mut registry);
        crate::compat::register(&mut registry);
        register_graphics(&mut registry);
        crate::font::register(&mut registry);
        crate::sound::register(&mut registry);
        crate::vorbis::register(&mut registry, codec);
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
        crate::sound::install(&mut machine)?;
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
        match crate::sound::pump(&mut self.machine) {
            Ok(Some(at)) => match outcome {
                SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(next_deadline.map_or(at, |old| old.min(at))),
                    })
                }
                other => other,
            },
            Ok(None) => outcome,
            Err(message) => SliceResult::Complete(RunResult::Failed(pal_machine::MachineError {
                ctx: self.machine.sys.current,
                pc: self.machine.cpu.reg(pal_cpu::Reg::Eip),
                message,
                recent: self.machine.sys.recent_calls(),
            })),
        }
    }

    /// Optional PCM device emulation; native device output is owned by the frontend.
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.machine.ext.get_mut::<crate::sound::Sound>().enabled = enabled;
    }

    pub fn audio_stream(&self) -> pal_audio::AudioStream {
        self.machine.ext.get::<crate::sound::Sound>().audio.stream()
    }

    /// Queue and missing-PCM observations, independent of audible sample values.
    pub fn audio_pcm_metrics(&self) -> pal_audio::PcmMetrics {
        self.audio_stream().metrics()
    }
    pub fn audio_source_status(&self) -> crate::AudioSourceStatus {
        self.machine
            .ext
            .get::<crate::sound::Sound>()
            .source_status(self.machine.sys.clock.now_us())
    }

    /// Hash the first four secondary-buffer starts for decoder differential checks.
    /// Disabled normally: hashing a whole PCM buffer is diagnostic work.
    pub fn enable_audio_pcm_digests(&mut self) {
        self.machine
            .ext
            .get_mut::<crate::sound::Sound>()
            .capture_digests = true;
    }

    pub fn audio_pcm_digests(&self) -> &[crate::PcmDigest] {
        &self.machine.ext.get::<crate::sound::Sound>().digests
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
        wake: pal_host::WakeEvent,
    ) {
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .set_frame_mailbox(mailbox, wake);
    }
    pub fn set_host_keys(&mut self, keys: std::sync::Arc<pal_host::input::KeyState>) {
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
        win32.cfg.codepage = codepage;
        win32.codec = pal_win32::Codec::new(codepage);
    }

    /// Post input to the foremost visible window's thread. Keyboard messages use
    /// its valid focus window; mouse messages use its valid capture window.
    pub fn post_input(&mut self, message: u32, wparam: u32, lparam: u32) -> bool {
        let user = &mut self.machine.ext.get_mut::<Win32>().user;
        let Some((top, hwnd)) = input_target(user, message) else {
            return false;
        };
        let mut lparam = lparam;
        if matches!(message, 0x100 | 0x104 | 0x101 | 0x105) && wparam < 256 {
            let key = &mut user.keys[wparam as usize];
            if matches!(message, 0x100 | 0x104) {
                *key |= 0x80;
            } else {
                *key &= 0x7f;
            }
        }
        if (0x200..=0x20e).contains(&message) {
            let x = (lparam as u16 as i16) as i32;
            let y = ((lparam >> 16) as u16 as i16) as i32;
            // Frontend coordinates describe the displayed top-level client.
            // A captured child receives coordinates in its own client area.
            let (ox, oy) = user.client_origin(top);
            user.cursor_pos = (x + ox, y + oy);
            if message == 0x20a || message == 0x20e {
                lparam = pal_win32::user32::window::make_long(x + ox, y + oy);
            } else if hwnd != top {
                let (tx, ty) = user.client_origin(hwnd);
                lparam = pal_win32::user32::window::make_long(x + ox - tx, y + oy - ty);
            }
            for (down, up, vk) in [(0x201, 0x202, 1), (0x204, 0x205, 2), (0x207, 0x208, 4)] {
                if message == down {
                    user.keys[vk] |= 0x80;
                }
                if message == up {
                    user.keys[vk] &= 0x7f;
                }
            }
        }
        let mut env = Env {
            cpu: &mut self.machine.cpu,
            sys: &mut self.machine.sys,
            ext: &mut self.machine.ext,
            thunk: 0,
        };
        pal_win32::user32::message::post_input(&mut env, hwnd, message, wparam, lparam)
    }

    /// Release keys/buttons when the native host loses focus. Preserve toggle
    /// bits and enqueue releases so guest procedures cannot retain a held key.
    pub fn release_input(&mut self) -> usize {
        let (pressed, position) = {
            let user = &mut self.machine.ext.get_mut::<Win32>().user;
            let pressed = user.keys;
            for key in &mut user.keys {
                *key &= 0x7f;
            }
            let position = input_target(user, 0x200).map_or(0, |(top, _)| {
                let (ox, oy) = user.client_origin(top);
                pal_win32::user32::window::make_long(user.cursor_pos.0 - ox, user.cursor_pos.1 - oy)
            });
            (pressed, position)
        };
        let mut posted = 0;
        for (key, _) in pressed
            .into_iter()
            .enumerate()
            .filter(|(_, state)| state & 0x80 != 0)
        {
            let (message, wparam, lparam) = match key {
                1 => (0x202, 0, position),
                2 => (0x205, 0, position),
                4 => (0x208, 0, position),
                5 | 6 => (0x20c, ((key as u32 - 4) << 16), position),
                0x12 => (0x105, key as u32, 0xe0000001),
                _ => (0x101, key as u32, 0xc0000001),
            };
            posted += usize::from(self.post_input(message, wparam, lparam));
        }
        for queue in self.machine.ext.get_mut::<Win32>().user.queues.values_mut() {
            queue.capture = 0;
        }
        posted
    }

    /// Imports with no handler; resolved probes are included even if never called.
    pub fn missing_apis(&self) -> Vec<String> {
        let called: std::collections::BTreeSet<_> = self
            .machine
            .sys
            .recent
            .iter()
            .map(|(_, thunk, _)| *thunk)
            .collect();
        self.machine
            .sys
            .thunks
            .iter()
            .filter(|(index, t)| {
                !t.implemented
                    || (called.contains(index)
                        && std::ptr::fn_addr_eq(
                            t.f.handler,
                            unsupported_graphics as pal_machine::Handler,
                        ))
            })
            .map(|(_, t)| format!("{}!{}", t.module, t.name))
            .collect()
    }

    pub fn report(&self) -> String {
        let graphics = self.machine.ext.get::<Graphics<Snapshot>>();
        let mut report = format!(
            "host calls: {}; frames: {}; surfaces: {}; bytes blitted: {}; CPU: {}\nmissing APIs: {}",
            self.machine.sys.host_calls,
            graphics.stats.presentations,
            graphics.live_surfaces(),
            graphics.stats.bytes_blitted,
            std::any::type_name::<B>(),
            self.missing_apis().join(", ")
        );
        let user = &self.machine.ext.get::<Win32>().user;
        report.push_str(&format!(
            "\nActive CPU context {}:",
            self.machine.sys.current
        ));
        for register in [
            pal_cpu::Reg::Eip,
            pal_cpu::Reg::Esp,
            pal_cpu::Reg::Ebp,
            pal_cpu::Reg::Eax,
            pal_cpu::Reg::Ebx,
            pal_cpu::Reg::Ecx,
            pal_cpu::Reg::Edx,
            pal_cpu::Reg::Esi,
            pal_cpu::Reg::Edi,
        ] {
            report.push_str(&format!(
                " {register:?}={:#010x}",
                self.machine.cpu.reg(register)
            ));
        }
        let stack = self.machine.cpu.reg(pal_cpu::Reg::Esp);
        report.push_str("\nActive stack words:");
        for index in 0..64u32 {
            let Some(address) = stack.checked_add(index * 4) else {
                break;
            };
            match pal_cpu::GuestExt::read_u32(&self.machine.cpu, address) {
                Ok(value) => report.push_str(&format!(" {value:08x}")),
                Err(_) => {
                    report.push_str(" <unmapped>");
                    break;
                }
            }
        }
        if let Some(sound) = self.machine.ext.try_get::<crate::sound::Sound>() {
            report.push_str(&format!(
                "\nAudio: enabled {}; queued {} samples; nonzero {}",
                sound.enabled, sound.queued_samples, sound.nonzero_samples
            ));
        }
        let control = self.machine.ext.get::<RunControl>();
        if let Some(last) = control.last_frame {
            report.push_str(&format!(
                "\nLast presented guest frame: {} ({:.3} ms ago)",
                graphics.stats.presentations,
                last.elapsed().as_secs_f64() * 1000.0
            ));
        }
        let now = self.machine.sys.clock.now_us();
        report.push_str(&format!("\nGuest contexts (clock {now} us):"));
        for context in &self.machine.sys.contexts {
            report.push_str(&format!(
                "\n  [{}] {:?} tid {:#x}: {}",
                context.id,
                context.name,
                context.tid,
                context.state_name()
            ));
            if let Some(wait) = context.waiting_on() {
                report.push_str(&format!(" {wait:?}"));
                let deadline = match wait {
                    pal_machine::Wait::Until(deadline) => Some(*deadline),
                    pal_machine::Wait::Object { deadline, .. }
                    | pal_machine::Wait::Objects { deadline, .. } => *deadline,
                    pal_machine::Wait::Yield => None,
                };
                if let Some(deadline) = deadline {
                    report.push_str(&format!(
                        " (in {:.3} ms)",
                        deadline.saturating_sub(now) as f64 / 1000.0
                    ));
                }
                if let pal_machine::Wait::Object { handle, .. } = wait {
                    report.push_str(&format!(
                        " object {:?}",
                        self.machine.sys.objects.get(*handle)
                    ));
                }
            }
            if let Some(thunk) = context.blocked_thunk() {
                let call = self.machine.sys.thunks.get(thunk);
                report.push_str(&format!(" in {}!{}", call.module, call.name));
            }
            if let Some(caller) = context
                .blocked_stack()
                .and_then(|stack| pal_cpu::GuestExt::read_u32(&self.machine.cpu, stack).ok())
            {
                let place = self
                    .machine
                    .sys
                    .modules
                    .iter()
                    .find(|(_, module)| module.contains(caller))
                    .map_or_else(
                        || format!("{caller:#010x}"),
                        |(_, module)| {
                            format!(
                                "{}+{:#x} ({caller:#010x})",
                                module.name,
                                caller - module.base
                            )
                        },
                    );
                report.push_str(&format!(" from {place}"));
            }
            if let Some(args) = context.blocked_args() {
                report.push_str(&format!(
                    " args [{}]",
                    args.iter()
                        .map(|a| format!("{a:x}"))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
            if let Some(exit) = context.exit_code() {
                report.push_str(&format!(" exit {exit}"));
            }
            if let Some(queue) = user.queues.get(&context.id) {
                report.push_str(&format!("\n    queue event {:#x} {:?}; posted {}; quit {:?}; focus {:#x}; active {:#x}; capture {:#x}",queue.event,self.machine.sys.objects.get(queue.event),queue.posted.len(),queue.quit,queue.focus,queue.active,queue.capture));
                for message in queue.posted.iter().take(8) {
                    report.push_str(&format!(
                        "\n      posted hwnd {:#x} msg {:#x} wp {:#x} lp {:#x}",
                        message.hwnd, message.msg, message.wparam, message.lparam
                    ));
                }
                for timer in &queue.timers {
                    report.push_str(&format!("\n      timer hwnd {:#x} id {:#x} interval {:.3} ms due in {:.3} ms proc {:#x}",timer.hwnd,timer.id,timer.interval_us as f64/1000.0,timer.due_us.saturating_sub(now)as f64/1000.0,timer.proc));
                }
            }
        }
        report.push_str("\nVisible windows:");
        for window in user
            .windows
            .values()
            .filter(|w| w.hwnd != user.desktop && w.visible())
        {
            let class = &user.classes[window.class].name;
            let (width, height) = window.client_size();
            report.push_str(&format!(
                "\n  {:#010x} class {:?} text {:?} client {}x{} parent {:#010x} thread {}",
                window.hwnd, class, window.text, width, height, window.parent, window.thread
            ));
        }
        report.push_str("\nRecent host calls (oldest first):");
        for call in self.machine.sys.recent_calls() {
            report.push_str(&format!("\n  {call}"));
        }
        report
    }

    pub fn report_with_profile(&self, samples: &BTreeMap<u32, adv_jit::ProfileSample>) -> String {
        let mut report = self.report();
        let mut samples: Vec<_> = samples.iter().collect();
        samples.sort_by_key(|(_, s)| std::cmp::Reverse(s.native_ns + s.interpreter_ns));
        if !samples.is_empty() {
            report.push_str("\nGuest execution profile (includes synchronous API work):");
        }
        for (&address, sample) in samples.into_iter().take(20) {
            let place = self
                .machine
                .sys
                .modules
                .iter()
                .find(|(_, module)| module.contains(address))
                .map_or_else(
                    || format!("{address:#010x}"),
                    |(_, module)| format!("{}+{:#x}", module.name, address - module.base),
                );
            report.push_str(&format!(
                "\n  {place}: native {} entries {:.3} ms; interpreter {} entries {} units {:.3} ms",
                sample.native_entries,
                sample.native_ns as f64 / 1e6,
                sample.interpreter_entries,
                sample.interpreter_instructions,
                sample.interpreter_ns as f64 / 1e6
            ));
        }
        report
    }
}

fn input_target(user: &pal_win32::user32::User, message: u32) -> Option<(u32, u32)> {
    let top = user.top.iter().copied().find(|h| {
        user.windows
            .get(h)
            .is_some_and(|w| w.visible() && !w.destroying)
    })?;
    let thread = user.windows[&top].thread;
    let queue = user.queues.get(&thread);
    let candidate = if (0x100..=0x109).contains(&message) {
        queue.map(|q| q.focus)
    } else if (0x200..=0x20e).contains(&message) {
        queue.map(|q| q.capture)
    } else {
        None
    };
    let target = candidate
        .filter(|&hwnd| valid_input_window(user, hwnd, thread))
        .unwrap_or(top);
    Some((top, target))
}

fn valid_input_window(
    user: &pal_win32::user32::User,
    hwnd: u32,
    thread: pal_machine::CtxId,
) -> bool {
    if !user.windows.get(&hwnd).is_some_and(|w| w.thread == thread) {
        return false;
    }
    let mut current = hwnd;
    // Bound malformed parent chains as well as checking inherited visibility.
    for _ in 0..=user.windows.len() {
        let Some(window) = user.windows.get(&current) else {
            return false;
        };
        if !window.visible() || !window.enabled() || window.destroying {
            return false;
        }
        if window.parent == 0 {
            return true;
        }
        current = window.parent;
    }
    false
}

/// Graphics registrations replace the Win32 state-only drawing handlers.
pub fn register_graphics(registry: &mut Registry) {
    registry.add(
        "user32.dll",
        "BeginPaint",
        Conv::Stdcall { argc: 2 },
        begin_paint,
    );
    for (interface, methods) in [
        ("IDirectDraw7", DEVICE_METHODS),
        ("IDirectDrawSurface7", SURFACE_METHODS),
    ] {
        for method in methods {
            registry.add(
                "ddraw.dll",
                &format!("{interface}::{method}"),
                Conv::Cdecl { argc: 0 },
                unsupported_graphics,
            );
        }
    }
    for &(id, module, name, argc) in API_SPECS {
        // PAL's GetLastError follows each context's TEB, including file and heap calls.
        if id != api::LAST_ERROR {
            registry.add(module, name, Conv::Stdcall { argc }, graphics_call);
        }
    }
}

/// Allocate shared surfaces and bind their vtables to the registry's actual thunks.
pub fn install_graphics<B: Backend>(
    machine: &mut Machine<B>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    machine.ext.insert(RunControl::default());
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err("invalid surface dimensions".into());
    }
    let arena_len = 64 * 1024 * 1024;
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, arena_len, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut entries = BTreeMap::new();
    for (base, interface, methods) in [
        (400, "IDirectDraw7", DEVICE_METHODS),
        (500, "IDirectDrawSurface7", SURFACE_METHODS),
    ] {
        for (slot, method) in methods.iter().enumerate() {
            entries.insert(
                base + slot as u32,
                machine
                    .resolve_import("ddraw.dll", &format!("{interface}::{method}"))
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    for &(id, module, name, _) in API_SPECS {
        if id != api::LAST_ERROR {
            entries.insert(
                id,
                machine
                    .resolve_import(module, name)
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    let snapshot = Snapshot {
        width,
        height,
        bgra: vec![0; width as usize * height as usize * 4],
        frames: 0,
    };
    machine.ext.insert(Graphics::new(
        width, height, arena, arena_len, entries, snapshot,
    )?);
    Ok(())
}

pub(crate) fn graphics_call(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let Some(&(id, _, _, argc)) = API_SPECS
        .iter()
        .find(|(_, module, name, _)| *module == thunk.module && *name == thunk.name)
    else {
        return Outcome::Fail("unknown graphics thunk".into());
    };
    let args: Vec<u32> = (0..u32::from(argc)).map(|i| env.arg(i)).collect();
    let hwnd = match id {
        api::GET_DC => Some(args[0]),
        api::COOPERATIVE if args[2] == 8 => Some(args[1]),
        api::CLIPPER_SET_HWND => Some(args[2]),
        _ => None,
    };
    let geometry = hwnd
        .and_then(|hwnd| {
            env.ext.try_get_mut::<Win32>().and_then(|win32| {
                win32.user.windows.get(&hwnd).map(|window| {
                    let (w, h) = window.client_size();
                    (w, h, win32.user.client_origin(hwnd))
                })
            })
        })
        .filter(|&(w, h, _)| w > 0 && h > 0);
    let graphics = env.ext.get_mut::<Graphics<Snapshot>>();
    if let Some((width, height, origin)) = geometry {
        if let Err(error) =
            graphics.configure_window(width as u32, height as u32, &mut GuestMemory(env.cpu))
        {
            return Outcome::Fail(format!("window graphics: {error}"));
        }
        if id == api::CLIPPER_SET_HWND {
            graphics.configure_clipper(args[0], origin);
        }
    }
    let before = graphics.stats.presentations;
    let out = graphics.handle(id, &args, &mut GuestMemory(env.cpu));
    let presented = graphics.stats.presentations;
    if let Ok(value) = &out {
        // Raster storage stays in Graphics; USER32/GDI object metadata must
        // refer to the same HDC so fonts and palettes share its real lifetime.
        let added = match id {
            api::GET_DC if *value != 0 => Some((*value, args[0])),
            api::CREATE_DC if *value != 0 => Some((*value, 0)),
            api::SURFACE_DC if *value == 0 => env.cpu.read_u32(args[1]).ok().map(|dc| (dc, 0)),
            _ => None,
        };
        let removed = match id {
            api::DELETE_DC if *value != 0 => Some(args[0]),
            api::RELEASE_DC if *value != 0 => Some(args[1]),
            api::SURFACE_RELEASE_DC if *value == 0 => Some(args[1]),
            _ => None,
        };
        let selection = added
            .map(|(dc, _)| dc)
            .or_else(|| if id == api::SELECT && *value != 0 { Some(args[0]) } else { None })
            .and_then(|dc| graphics.dc_bitmap(dc).map(|bitmap| (dc, bitmap)));
        if (added.is_some() || removed.is_some() || selection.is_some())
            && let Some(win32) = env.ext.try_get_mut::<Win32>()
        {
            if let Some((dc, hwnd)) = added {
                win32.gdi.register_external_dc(dc, hwnd);
            }
            if let Some(dc) = removed {
                win32.gdi.remove_external_dc(dc);
            }
            if let Some((dc, bitmap)) = selection {
                win32.gdi.set_external_bitmap(dc, bitmap);
            }
        }
        if let Some(dc) = removed {
            crate::font::retire_dc(env, dc);
        }
    }
    if presented != before {
        env.sys.frame = presented as u32;
        if out.is_ok() {
            env.sys.complete_frame();
        }
        let control = env.ext.get_mut::<RunControl>();
        let now = Instant::now();
        control.first_frame.get_or_insert(now);
        if control.frame_timing
            && let Some(last) = control.last_frame
        {
            control.frame_intervals.push(now.duration_since(last));
        }
        control.last_frame = Some(now);
        if out.is_ok() && control.frame_limit.is_some_and(|limit| presented >= limit) {
            return Outcome::ExitProcess(0);
        }
    }
    match out {
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("graphics: {error}")),
    }
}

fn unsupported_graphics(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    Outcome::Fail(format!("unimplemented: {}!{}", thunk.module, thunk.name))
}

fn begin_paint(env: &mut Env<'_>) -> Outcome {
    let (hwnd, paint) = (env.arg(0), env.arg(1));
    let Some(window) = env.ext.get::<Win32>().user.windows.get(&hwnd) else {
        return Outcome::Return(0);
    };
    let (width, height) = window.client_size();
    let graphics = env.ext.get_mut::<Graphics<Snapshot>>();
    if width > 0
        && height > 0
        && let Err(error) =
            graphics.configure_window(width as u32, height as u32, &mut GuestMemory(env.cpu))
    {
        return Outcome::Fail(format!("BeginPaint resize: {error}"));
    }
    let dc = match graphics.handle(api::GET_DC, &[hwnd], &mut GuestMemory(env.cpu)) {
        Ok(dc) => dc,
        Err(error) => return Outcome::Fail(format!("BeginPaint: {error}")),
    };
    env.ext
        .get_mut::<Win32>()
        .gdi
        .register_external_dc(dc, hwnd);
    let mut bytes = [0u8; 64];
    bytes[..4].copy_from_slice(&dc.to_le_bytes());
    bytes[16..20].copy_from_slice(&width.to_le_bytes());
    bytes[20..24].copy_from_slice(&height.to_le_bytes());
    if let Err(error) = env.cpu.write(paint, &bytes) {
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    pal_win32::user32::message::validate(env, hwnd);
    Outcome::Return(dc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;
    use pal_cpu::{Guest, GuestExt};

    fn machine() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        register_graphics(&mut registry);
        let cpu = JitCpu::new(pal_machine::layout::PARK_ADDR, false).unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        install_graphics(&mut machine, 4, 2).unwrap();
        machine
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
            JitCpu::new(pal_machine::layout::PARK_ADDR, false).unwrap(),
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

    fn graphics(machine: &mut Machine<JitCpu>, id: u32, args: &[u32]) -> u32 {
        machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .handle(id, args, &mut GuestMemory(&mut machine.cpu))
            .unwrap()
    }

    fn input_runtime() -> Runtime {
        use pal_win32::user32::window::{
            ControlState, Rect, WS_CHILD, WS_POPUP, WS_VISIBLE, Window,
        };
        let mut machine = machine();
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
        user.windows.get_mut(&0x10024).unwrap().style &= !pal_win32::user32::window::WS_VISIBLE;
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
        let top_position = pal_win32::user32::window::make_long(60, 80);
        assert!(runtime.post_input(0x200, 0, top_position));
        let user = &mut runtime.machine.ext.get_mut::<Win32>().user;
        let message = user.queue(0).posted.pop_front().unwrap();
        assert_eq!(message.hwnd, 0x10028);
        assert_eq!(message.lparam, pal_win32::user32::window::make_long(20, 30));
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
            pal_win32::user32::window::make_long(160, 130)
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
        assert!(runtime.post_input(0x201, 1, pal_win32::user32::window::make_long(60, 80)));
        let event = runtime
            .machine
            .sys
            .objects
            .insert(pal_machine::Object::Event {
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
            Some(pal_machine::Object::Event { signaled: true, .. })
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

    #[test]
    fn pal_trap_presents_pixels_from_the_shared_guest_store() {
        let mut m = machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let mut desc = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 7), (8, 2), (12, 4), (104, 0x40)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let source = m.cpu.read_u32(data).unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(
            graphics(&mut m, api::LOCK, &[source, 0, data + 16, 0, 0]),
            0
        );
        let pixels = m.cpu.read_u32(data + 52).unwrap();
        let expected: Vec<u8> = (0..32u8).collect();
        m.cpu.write(pixels, &expected).unwrap();
        assert_eq!(graphics(&mut m, api::UNLOCK, &[source, 0]), 0);
        desc.fill(0);
        for (offset, value) in [(0, 108u32), (4, 1), (104, 0x200)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let primary = m.cpu.read_u32(data).unwrap();
        let table = m.cpu.read_u32(primary).unwrap();
        let blt = m.cpu.read_u32(table + 7 * 4).unwrap();
        let code = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        for value in [0, 0, source, 0, 0, primary] {
            assembler.push(value).unwrap();
        }
        assembler.call(u64::from(blt)).unwrap();
        assembler.ret().unwrap();
        m.cpu
            .write(code, &assembler.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("graphics", code, &[], 65536).unwrap();
        assert!(matches!(m.run(), RunResult::Exited(0)));
        let graphics = m.ext.get::<Graphics<Snapshot>>();
        assert_eq!(graphics.presenter.bgra, expected);
        assert_eq!(graphics.presenter.frames, 1);
        assert_eq!(
            m.sys.completed_frames, 1,
            "the real graphics trap did not publish its completed frame"
        );
    }

    #[test]
    fn unsupported_com_slot_fails_by_method_name() {
        let mut m = machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let table = m.cpu.read_u32(device).unwrap();
        let compact = m.cpu.read_u32(table + 3 * 4).unwrap();
        let code = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        assembler.push(device).unwrap();
        assembler.call(u64::from(compact)).unwrap();
        assembler.ret().unwrap();
        m.cpu
            .write(code, &assembler.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("missing graphics", code, &[], 65536).unwrap();
        let RunResult::Failed(error) = m.run() else {
            panic!("missing COM implementation must fail")
        };
        assert_eq!(
            error.message,
            "unimplemented: ddraw.dll!IDirectDraw7::Compact"
        );
    }

    #[test]
    fn clipper_translates_screen_rectangles_and_resize_preserves_surface_identity() {
        let mut m = machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let mut desc = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 7), (8, 2), (12, 4), (104, 0x40)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let source = m.cpu.read_u32(data).unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(
            graphics(&mut m, api::LOCK, &[source, 0, data + 16, 0, 0]),
            0
        );
        let pixels = m.cpu.read_u32(data + 52).unwrap();
        let expected: Vec<u8> = (0..32u8).collect();
        m.cpu.write(pixels, &expected).unwrap();
        assert_eq!(graphics(&mut m, api::UNLOCK, &[source, 0]), 0);
        desc.fill(0);
        for (offset, value) in [(0, 108u32), (4, 1), (104, 0x200)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let primary = m.cpu.read_u32(data).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_CLIPPER, &[device, 0, data, 0]),
            0
        );
        let clipper = m.cpu.read_u32(data).unwrap();
        assert_eq!(graphics(&mut m, api::SET_CLIPPER, &[primary, clipper]), 0);
        m.ext
            .get_mut::<Graphics<Snapshot>>()
            .configure_clipper(clipper, (100, 50));
        m.cpu
            .write(
                data + 16,
                &[99u32, 50, 103, 52]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert_eq!(
            graphics(&mut m, api::BLT, &[primary, data + 16, source, 0, 0, 0]),
            0
        );
        let frame = &m.ext.get::<Graphics<Snapshot>>().presenter;
        assert_eq!(&frame.bgra[..12], &expected[4..16]);
        assert_eq!(&frame.bgra[16..28], &expected[20..32]);
        assert_eq!(&frame.bgra[12..16], &[0; 4]);
        m.ext
            .get_mut::<Graphics<Snapshot>>()
            .configure_window(6, 3, &mut GuestMemory(&mut m.cpu))
            .unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(graphics(&mut m, api::GET_DESC, &[primary, data + 16]), 0);
        assert_eq!(m.cpu.read_u32(data + 28).unwrap(), 6);
        assert_eq!(m.cpu.read_u32(data + 24).unwrap(), 3);
        assert_eq!(graphics(&mut m, api::SURFACE_DC, &[primary, data]), 0);
        let dc = m.cpu.read_u32(data).unwrap();
        assert_ne!(graphics(&mut m, api::SURFACE_RELEASE_DC, &[source, dc]), 0);
        assert!(
            m.ext.get::<Graphics<Snapshot>>().owns_dc(dc),
            "a rejected ReleaseDC must retain the original surface DC"
        );
        assert_eq!(graphics(&mut m, api::SURFACE_RELEASE_DC, &[primary, dc]), 0);
        let frame = &m.ext.get::<Graphics<Snapshot>>().presenter;
        assert_eq!((frame.width, frame.height), (6, 3));
        assert_eq!(&frame.bgra[..12], &expected[4..16]);
        assert_eq!(&frame.bgra[24..36], &expected[20..32]);
        assert!(frame.bgra[48..].iter().all(|&byte| byte == 0));
    }
}
