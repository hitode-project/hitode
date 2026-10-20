//! Diagnostics of a [`Runtime`]: the plain-text run report and the missing-API list.
use std::collections::BTreeMap;

use adv_cpu::backend::Backend;
use adv_win32::Win32;

use crate::boot::Runtime;
use crate::graphics::RunControl;
use crate::{Graphics, Snapshot};

impl<B: Backend> Runtime<B> {
    pub fn last_indirect_branch(&self) -> Option<adv_cpu::IndirectBranch> {
        self.machine.cpu.last_indirect_branch()
    }
    pub fn first_cpu_fault(&self) -> Option<&adv_machine::exception::CpuFaultSnapshot> {
        self.machine.sys.first_cpu_fault.as_ref()
    }
    pub fn latest_cpu_fault(&self) -> Option<&adv_machine::exception::CpuFaultSnapshot> {
        self.machine.sys.latest_cpu_fault.as_ref()
    }
    fn api_registry(&self) -> &adv_abi::Registry {
        (self.machine.sys.thunks.resolver() as &dyn std::any::Any)
            .downcast_ref()
            .expect("runtime ABI registry")
    }

    /// Select before the first step. Discovery results are never compatibility passes.
    pub fn set_api_mode(&mut self, mode: adv_abi::RunMode) {
        self.machine
            .sys
            .thunks
            .resolver_mut::<adv_abi::Registry>()
            .expect("runtime ABI registry")
            .set_mode(mode);
    }

    pub fn api_mode(&self) -> adv_abi::RunMode {
        self.api_registry().mode()
    }

    pub fn api_diagnostics(&self) -> Option<&adv_abi::ApiDiagnostics> {
        self.machine.ext.try_get()
    }

    pub fn api_signature_conflicts(&self) -> Vec<adv_abi::SignatureConflict> {
        self.api_registry().signature_conflicts()
    }

    pub fn api_coverage(&self) -> adv_abi::Coverage {
        self.api_registry().coverage()
    }

    /// Necessary ABI conditions only; this does not certify graphics, audio or playability.
    pub(crate) fn selected_api_acceptance_eligible(&self) -> bool {
        self.api_mode() == adv_abi::RunMode::Strict
            && self
                .api_diagnostics()
                .is_none_or(|d| d.acceptance_eligible(self.api_mode()))
            && self.api_signature_conflicts().is_empty()
            && self
                .machine
                .sys
                .recent
                .iter()
                .all(|(_, thunk, _)| self.machine.sys.thunks.get(*thunk).implemented)
    }

    pub fn api_acceptance_eligible(&self) -> bool {
        self.selected_api_acceptance_eligible()
            && self
                .family
                .records
                .iter()
                .all(|p| p.acceptance_eligible && p.failure.is_none())
            && self.family.parked.values().all(|machine| {
                let registry = (machine.sys.thunks.resolver() as &dyn std::any::Any)
                    .downcast_ref::<adv_abi::Registry>()
                    .unwrap();
                registry.mode() == adv_abi::RunMode::Strict
                    && registry.signature_conflicts().is_empty()
                    && machine
                        .ext
                        .try_get::<adv_abi::ApiDiagnostics>()
                        .is_none_or(|d| d.acceptance_eligible(registry.mode()))
                    && machine
                        .sys
                        .recent
                        .iter()
                        .all(|(_, thunk, _)| machine.sys.thunks.get(*thunk).implemented)
            })
    }

    pub fn missing_apis(&self) -> Vec<String> {
        let mut missing: std::collections::BTreeSet<_> =
            self.selected_missing_apis().into_iter().collect();
        for record in &self.family.records {
            missing.extend(record.missing_apis.iter().cloned());
        }
        for machine in self.family.parked.values() {
            let called: std::collections::BTreeSet<_> = machine
                .sys
                .recent
                .iter()
                .map(|(_, thunk, _)| *thunk)
                .collect();
            missing.extend(
                machine
                    .sys
                    .thunks
                    .iter()
                    .filter(|(index, t)| !t.implemented && (t.requested || called.contains(index)))
                    .map(|(_, t)| format!("{}!{}", t.module, t.name)),
            );
        }
        missing.into_iter().collect()
    }

    pub fn guest_window_count(&self) -> usize {
        let user = &self.machine.ext.get::<Win32>().user;
        user.windows
            .values(&self.machine.sys.objects)
            .filter(|window| window.hwnd != user.desktop)
            .count()
    }

    /// Imports with no handler; resolved probes are included even if never called.
    pub(crate) fn selected_missing_apis(&self) -> Vec<String> {
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
            .filter(|(index, t)| !t.implemented && (t.requested || called.contains(index)))
            .map(|(_, t)| format!("{}!{}", t.module, t.name))
            .collect()
    }

    /// Guest threads: state, wait, the host call a blocked one is in, its
    /// caller, and its message queue.
    pub fn contexts_report(&self) -> String {
        let user = &self.machine.ext.get::<Win32>().user;
        let mut report = String::new();
        let now = self.machine.sys.clock.now_us();
        report.push_str(&format!("Guest contexts (clock {now} us):"));
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
                    adv_machine::Wait::Until(deadline) => Some(*deadline),
                    adv_machine::Wait::Object { deadline, .. }
                    | adv_machine::Wait::Objects { deadline, .. } => *deadline,
                    adv_machine::Wait::Yield => None,
                };
                if let Some(deadline) = deadline {
                    report.push_str(&format!(
                        " (in {:.3} ms)",
                        deadline.saturating_sub(now) as f64 / 1000.0
                    ));
                }
                if let adv_machine::Wait::Object { handle, .. } = wait {
                    report.push_str(&format!(
                        " object {:?}",
                        self.machine.sys.objects.get(*handle)
                    ));
                }
                if let adv_machine::Wait::Objects { handles, .. } = wait {
                    for handle in handles.iter().take(8) {
                        report.push_str(&format!(
                            "\n    handle {handle:#x} {:?}",
                            self.machine.sys.objects.get(*handle)
                        ));
                    }
                }
            }
            if let Some(thunk) = context.blocked_thunk() {
                let call = self.machine.sys.thunks.get(thunk);
                report.push_str(&format!(" in {}!{}", call.module, call.name));
            }
            if let Some(caller) = context.blocked_stack().and_then(|stack| {
                adv_cpu::backend::GuestExt::read_u32(&self.machine.cpu, stack).ok()
            }) {
                let place = adv_os::modules(&self.machine.ext)
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
        report
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
        report.push_str(&format!(
            "\nnon-leaf host calls: {}",
            self.machine
                .sys
                .host_calls
                .saturating_sub(self.machine.sys.stats.leaf_calls)
        ));
        #[cfg(feature = "gpu")]
        if self.machine.ext.try_get::<adv_dx::d3d9::State>().is_some() {
            let frames = self.gpu_diagnostics().frames;
            report.push_str(&format!("\nD3D9 recording: commands {}; flushes {:?}; eliminated frames {}; verification mismatches {}; full executions {}; clipped executions {}; clipped area sum {:.4}",
                frames.recorded_commands,frames.flushes,frames.eliminated_frames,frames.verification_mismatches,frames.full_executions,frames.clipped_executions,frames.clipped_area_sum));
            report.push_str(&frames.work_report());
        }
        let g = &graphics.stats;
        let seconds = g
            .software_started
            .map_or(1., |at| at.elapsed().as_secs_f64().max(1e-9));
        report.push_str(&format!(
            "\nSoftware frames: published {}; flushes [wait, message, timeout, immediate] {:?}; empty damage {}; mailbox copied {} bytes ({:.3} B/s); converted {} bytes ({:.3} B/s)",
            g.published_frames, g.software_flushes, g.empty_damage, g.copied_bytes, g.copied_bytes as f64 / seconds,
            g.converted_bytes, g.converted_bytes as f64 / seconds));
        for process in self.process_statuses() {
            report.push_str(&format!(
                "\nProcess {} parent {:?}: {}; exit {:?}; failure {:?}",
                process.pid, process.parent, process.image, process.exit_code, process.failure
            ));
        }
        let user = &self.machine.ext.get::<Win32>().user;
        report.push_str(&format!(
            "\nGuest input: cursor {:?}; display {:?}",
            user.cursor_pos, user.screen
        ));
        for window in user
            .windows
            .values(&self.machine.sys.objects)
            .filter(|window| window.hwnd != user.desktop)
        {
            report.push_str(&format!(
                "\n  window {:#x}: style {:#x}; rect {:?}; client {:?}; origin {:?}",
                window.hwnd,
                window.style,
                user.screen_rect(&self.machine.sys.objects, window.hwnd),
                window.client_size(),
                user.client_origin(&self.machine.sys.objects, window.hwnd)
            ));
        }
        report.push_str(&format!(
            "\nAPI mode: {:?}; ABI acceptance eligible: {}; signature conflicts: {:?}",
            self.api_mode(),
            self.api_acceptance_eligible(),
            self.api_signature_conflicts()
        ));
        if let Some(diagnostics) = self.api_diagnostics() {
            report.push_str(&format!(
                "\nCalled API gaps: {:?}; stub calls: {}; structural mismatches: {}",
                diagnostics.gaps, diagnostics.stub_calls, diagnostics.structural_mismatches
            ));
            for call in &diagnostics.recent_stubs {
                report.push_str(&format!(
                    "\n  stub {}!{} from {:#010x}, context {}: {}",
                    call.module,
                    call.name,
                    call.caller,
                    call.context,
                    call.arguments.join(", ")
                ));
            }
            report.push_str(&format!(
                "\nFailed dynamic queries: {}; captured messages: {}; dropped queries/messages: {}/{}; truncated fields: {}",
                diagnostics.dynamic_lookup_failure_count,
                diagnostics.message_box_count,
                diagnostics.dropped_dynamic_lookup_failures,
                diagnostics.dropped_message_boxes,
                diagnostics.truncated_diagnostic_fields,
            ));
            for failure in &diagnostics.dynamic_lookup_failures {
                report.push_str(&format!(
                    "\n  lookup #{} {}!{} from {:#010x}, context {}, {}: {}",
                    failure.sequence,
                    failure.module,
                    failure.name,
                    failure.caller,
                    failure.context,
                    failure.kind.as_str(),
                    failure.reason,
                ));
            }
            for message in &diagnostics.message_boxes {
                report.push_str(&format!(
                    "\n  message #{} at frame {} {} from {:#010x}, context {}, style {:#x}, result {}: {:?}: {:?}",
                    message.sequence, message.frame, message.api, message.caller, message.context,
                    message.style, message.result, message.caption, message.text,
                ));
            }
            personality_gaps(&mut report, diagnostics);
        }
        for (label, fault) in [
            ("First", self.first_cpu_fault()),
            ("Latest", self.latest_cpu_fault()),
        ] {
            let Some(fault) = fault else {
                continue;
            };
            report.push_str(&format!(
                "\n{label} CPU fault: context {} at {:#010x}: {:?};",
                fault.context, fault.fault.pc, fault.fault.kind
            ));
            if let Some(branch) = fault.last_indirect_branch {
                report.push_str(&format!(
                    " last indirect {:?} {:#010x} -> {:#010x};",
                    branch.kind, branch.source, branch.target
                ));
            }
            for (register, value) in fault.registers {
                report.push_str(&format!(" {register:?}={value:#010x}"));
            }
            report.push_str(&format!(
                "\n{label} fault stack at {:#010x} ({} of at most 16 readable words):",
                fault.stack_address,
                fault.stack.len()
            ));
            for word in &fault.stack {
                report.push_str(&format!(" {word:08x}"));
            }
        }
        report.push_str(&format!(
            "\nActive CPU context {}:",
            self.machine.sys.current
        ));
        if let Some(branch) = self.last_indirect_branch() {
            report.push_str(&format!(
                " last indirect {:?} {:#010x} -> {:#010x};",
                branch.kind, branch.source, branch.target
            ));
        }
        for register in [
            adv_cpu::backend::Reg::Eip,
            adv_cpu::backend::Reg::Esp,
            adv_cpu::backend::Reg::Ebp,
            adv_cpu::backend::Reg::Eax,
            adv_cpu::backend::Reg::Ebx,
            adv_cpu::backend::Reg::Ecx,
            adv_cpu::backend::Reg::Edx,
            adv_cpu::backend::Reg::Esi,
            adv_cpu::backend::Reg::Edi,
        ] {
            report.push_str(&format!(
                " {register:?}={:#010x}",
                self.machine.cpu.reg(register)
            ));
        }
        let stack = self.machine.cpu.reg(adv_cpu::backend::Reg::Esp);
        report.push_str("\nActive stack words:");
        for index in 0..64u32 {
            let Some(address) = stack.checked_add(index * 4) else {
                break;
            };
            match adv_cpu::backend::GuestExt::read_u32(&self.machine.cpu, address) {
                Ok(value) => report.push_str(&format!(" {value:08x}")),
                Err(_) => {
                    report.push_str(" <unmapped>");
                    break;
                }
            }
        }
        if let Some(sound) = self.machine.ext.try_get::<adv_dx::dsound::Sound>() {
            report.push_str(&format!(
                "\nAudio: enabled {}; queued {} samples; nonzero {}",
                sound.enabled, sound.queued_samples, sound.nonzero_samples
            ));
            let pcm = sound.audio.stream().metrics();
            report.push_str(&format!(
                "\nPCM: low_lead_events {}; min_lead_ratio_permille {}",
                pcm.low_lead_events, pcm.min_lead_ratio_permille
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
        report.push_str(&self.contexts_report());
        report.push_str("\nVisible windows:");
        report.push_str(&format!(
            "\nActive time skips: {}; skipped guest microseconds: {}",
            self.machine.sys.scheduling.active_time_skips,
            self.machine.sys.scheduling.active_time_skip_us
        ));
        let (clamps, discarded_us) = self.machine.sys.clock.passive_stalls();
        report.push_str(&format!(
            "\nPassive stalls: {clamps}; discarded guest microseconds: {discarded_us}"
        ));
        for window in user
            .windows
            .values(&self.machine.sys.objects)
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

    pub fn report_with_profile(&self, samples: &BTreeMap<u32, adv_cpu::ProfileSample>) -> String {
        let mut report = self.report();
        let mut samples: Vec<_> = samples.iter().collect();
        samples.sort_by_key(|(_, s)| std::cmp::Reverse(s.native_ns + s.interpreter_ns));
        if !samples.is_empty() {
            report.push_str("\nGuest execution profile (includes synchronous API work):");
        }
        for (&address, sample) in samples.into_iter().take(20) {
            let place = adv_os::modules(&self.machine.ext)
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

/// What the guest asked of the personality and did not get
/// (PERS-EXPORT-8): rejected discovery probes, reviewed
/// exports answered with their failure contract, and static stub calls,
/// whose names and callers are listed under the called API gaps.
fn personality_gaps(report: &mut String, diagnostics: &adv_abi::ApiDiagnostics) {
    report.push_str(&format!(
        "\nPersonality gaps: rejected probes: {} (dropped names: {}); reviewed failure calls: {}; stub calls: {}",
        diagnostics.rejected_probes.values().map(|probe| probe.count).sum::<u64>(),
        diagnostics.dropped_rejected_probes,
        diagnostics.reviewed_calls.values().sum::<u64>(),
        diagnostics.stub_calls,
    ));
    for (name, probe) in &diagnostics.rejected_probes {
        report.push_str(&format!(
            "\n  rejected {name} ({}) x{}, first from {:#010x}",
            probe
                .class
                .map_or("undeclared", adv_abi::ExportClass::as_str),
            probe.count,
            probe.first_caller,
        ));
    }
    for (name, count) in &diagnostics.reviewed_calls {
        report.push_str(&format!("\n  reviewed {name} x{count}"));
    }
}
