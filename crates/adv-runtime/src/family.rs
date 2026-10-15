//! Session-owned process supervisor. Every service runs after the backend has
//! collected its leases, never reentrantly from an API trap.
//!
//! Each guest process owns a `Machine` with its own CPU, address space, heaps,
//! modules, PEB/TEB, handles and threads; host devices and file services are
//! shared. CreateProcess maps the EXE and the main thread, and TLS and DllMain
//! run only after resume. The loader then reads the import directory from
//! guest memory, so Detours-style remote import rewriting works unchanged.
//! Rules: PROC-* in docs/spec/processes.md.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use std::{cell::Cell, rc::Rc};

use adv_cpu::backend::{Backend, Reg};
use adv_machine::{Machine, MachineError, RunResult, SliceResult};
use adv_os::{
    Loader, Modules, Process,
    loader::PreparedProcess,
    processes::{self, Created, Error, Parameters, Request, Services, Spawn},
};
use adv_win32::Win32;

use crate::{
    Graphics, Snapshot,
    boot::{Runtime, api_registry},
    graphics::{RunControl, install_graphics},
};

#[derive(Clone, Debug)]
pub struct ProcessStatus {
    pub pid: u32,
    pub parent: Option<u32>,
    pub image: String,
    pub exit_code: Option<u32>,
    pub failure: Option<String>,
    pub missing_apis: Vec<String>,
    pub acceptance_eligible: bool,
    pub structural_mismatches: u64,
    pub structural_warnings: Vec<(String, String, u64)>,
    /// Owned backend evidence survives process selection and child retirement.
    pub first_cpu_fault: Option<adv_machine::exception::CpuFaultSnapshot>,
    pub latest_cpu_fault: Option<adv_machine::exception::CpuFaultSnapshot>,
}

pub(crate) struct Family<B: Backend> {
    pub(crate) parked: BTreeMap<u32, Machine<B>>,
    prepared: BTreeMap<u32, PreparedProcess>,
    publication: BTreeMap<u32, Rc<Cell<processes::Publication>>>,
    first_failure: Option<MachineError>,
    completed: BTreeSet<u32>,
    pub(crate) records: Vec<ProcessStatus>,
    services: Services,
    foreground: u32,
    last: u32,
    width: u32,
    height: u32,
    codec: crate::vorbis::Policy,
}

impl<B: Backend> Family<B> {
    pub(crate) fn new(
        machine: &mut Machine<B>,
        width: u32,
        height: u32,
        codec: crate::vorbis::Policy,
    ) -> Self {
        let services = Services::default();
        services.register(&machine.sys.process_identity);
        machine.ext.insert(services.clone());
        Self {
            parked: BTreeMap::new(),
            prepared: BTreeMap::new(),
            publication: BTreeMap::new(),
            first_failure: None,
            completed: BTreeSet::new(),
            records: vec![ProcessStatus {
                pid: machine.sys.pid,
                parent: None,
                image: machine.ext.get::<Win32>().exe_path(),
                exit_code: None,
                failure: None,
                missing_apis: Vec::new(),
                acceptance_eligible: true,
                structural_mismatches: 0,
                structural_warnings: Vec::new(),
                first_cpu_fault: None,
                latest_cpu_fault: None,
            }],
            services,
            foreground: machine.sys.pid,
            last: 0,
            width,
            height,
            codec,
        }
    }
}

impl<B: Backend> Runtime<B> {
    pub fn process_statuses(&self) -> &[ProcessStatus] {
        &self.family.records
    }
    /// Live or retained structural evidence follows its process owner.
    pub fn process_structural_diagnostics(&self, pid: u32) -> (u64, Vec<(String, String, u64)>) {
        let machine = if self.machine.sys.pid == pid {
            Some(&self.machine)
        } else {
            self.family.parked.get(&pid)
        };
        if let Some(machine) = machine {
            return machine
                .ext
                .try_get::<adv_abi::ApiDiagnostics>()
                .map_or_else(Default::default, |d| {
                    let warnings = if d.structural_mismatches == 0 {
                        Vec::new()
                    } else {
                        d.warnings
                            .iter()
                            .map(|((api, reason), count)| (api.clone(), reason.clone(), *count))
                            .collect()
                    };
                    (d.structural_mismatches, warnings)
                });
        }
        self.family
            .records
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| (p.structural_mismatches, p.structural_warnings.clone()))
            .unwrap_or_default()
    }
    pub fn foreground_pid(&self) -> u32 {
        self.family.foreground
    }

    /// Final host stop, after execution leases have been collected. Retire
    /// guest GPU work before capturing the last completed frame; a parked
    /// process must not retain all shared readback slots. This does not publish
    /// a guest exit or failure, and callers must not resume guest execution.
    #[cfg(feature = "gpu")]
    pub fn cancel_pending_graphics(&mut self) {
        adv_dx::d3d9::cancel_pending(&mut self.machine);
        for machine in self.family.parked.values_mut() {
            adv_dx::d3d9::cancel_pending(machine);
        }
    }

    // Only execution ownership moves here. Presentation ownership changes only
    // in set_foreground, so background slices cannot steal the UI's mailbox.
    fn select_process(&mut self, pid: u32) -> bool {
        if self.machine.sys.pid == pid {
            return true;
        }
        let Some(mut next) = self.family.parked.remove(&pid) else {
            return false;
        };
        // One authoritative session clock, carried between scheduling turns.
        // Copying the settled state avoids a fresh epoch on child creation and
        // does not grant an idle allowance while another process is runnable.
        self.machine.sys.clock.begin_work();
        next.sys.clock = self.machine.sys.clock.clone();
        std::mem::swap(&mut self.machine, &mut next);
        self.family.parked.insert(next.sys.pid, next);
        self.machine.sys.trace.process(pid);
        true
    }

    fn set_foreground(&mut self, pid: u32) -> Result<(), String> {
        if self.family.foreground == pid {
            self.select_process(pid);
            return Ok(());
        }
        let old = self.family.foreground;
        if !self.select_process(old) {
            return Err("foreground process disappeared".into());
        }
        let next = self
            .family
            .parked
            .get_mut(&pid)
            .ok_or("target process disappeared")?;
        self.machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .transfer_frame_mailbox_to(
                next.ext.get_mut::<Graphics<Snapshot>>(),
                &crate::graphics::guest_memory(&mut self.machine.cpu),
            )?;
        self.family.foreground = pid;
        self.select_process(pid);
        Ok(())
    }

    fn create_child(&mut self, spec: Spawn) -> Result<Created, Error> {
        let live = self.family.records.len() - self.family.completed.len();
        if live >= processes::MAX_LIVE_PROCESSES
            || self.family.records.len() >= processes::MAX_PROCESSES
        {
            return Err(Error::new(8, "process family limit reached"));
        }
        if !self.select_process(spec.parent)
            || self
                .machine
                .sys
                .process_identity
                .completion
                .exit_code()
                .is_some()
        {
            return Err(Error::new(87, "parent process has exited"));
        }
        if self.machine.sys.clock.is_virtual() {
            // Machine's virtual run loop advances time locally. Until a family
            // virtual scheduler exists, never give children inconsistent time.
            return Err(Error::new(
                50,
                "child processes require the runtime real-time scheduler",
            ));
        }
        let pid = (self.family.records.len() as u32 + 1) * 0x10000;
        let cpu = self
            .machine
            .cpu
            .new_process()
            .map_err(|e| Error::new(50, e))?;
        let mut config = self.machine.ext.get::<Win32>().cfg.clone();
        config.os = spec.config;
        let mut registry = api_registry(self.family.codec);
        if let Some(parent) = self.machine.sys.thunks.resolver_mut::<adv_abi::Registry>() {
            registry.set_mode(parent.mode());
        }
        let mut child = Machine::new(cpu, registry, self.machine.sys.clock.clone())
            .map_err(|e| Error::new(8, e.to_string()))?;
        child
            .sys
            .set_process_identity(pid)
            .map_err(|e| Error::new(8, e))?;
        child.inherit_policy(&self.machine);
        for inherited in spec.inherited {
            let object = match inherited.object {
                processes::InheritedObject::Process(identity, access) => {
                    adv_machine::Object::Process { identity, access }
                }
                processes::InheritedObject::Thread(identity, access) => {
                    adv_machine::Object::RemoteThread { identity, access }
                }
            };
            if !child.sys.objects.insert_handle_at(inherited.handle, object)
                || !child
                    .sys
                    .objects
                    .set_handle_flags(inherited.handle, 3, inherited.flags)
            {
                return Err(Error::new(8, "cannot install inherited handle"));
            }
        }
        child.ext.insert(self.family.services.clone());
        child.ext.insert(Parameters {
            command_line: spec.command_line,
            environment: spec.environment,
        });
        let exe_path = config.os.exe_path();
        Win32::install(&mut child, config).map_err(|e| Error::new(8, e.to_string()))?;
        child
            .ext
            .get_mut::<Modules>()
            .inherit_system_image_layout(self.machine.ext.get::<Modules>())
            .map_err(|e| Error::new(8, e))?;
        child
            .ext
            .insert(adv_dx::dinput::InputProvider(crate::boot::input_snapshot));
        child
            .ext
            .get_mut::<Win32>()
            .vfs
            .inherit_optical(&self.machine.ext.get::<Win32>().vfs);
        {
            let parent = self.machine.ext.get::<Win32>();
            let win32 = child.ext.get_mut::<Win32>();
            win32.vfs.cwd = spec.directory;
            win32.vfs.sections = parent.vfs.sections.clone();
            win32.user.host_keys = parent.user.host_keys.clone();
        }
        crate::vorbis::install(&mut child, self.family.codec).map_err(|e| Error::new(8, e))?;
        let (win32, modules) = child.ext.get2_mut::<Win32, Modules>();
        let prepared = Loader::new(&mut child.cpu, &mut child.sys, modules, &win32.vfs)
            .prepare_process(&exe_path)
            .map_err(|e| Error::new(193, e.to_string()))?;
        let exe = prepared.module;
        Win32::prepare_process_infrastructure(&mut child).map_err(|e| Error::new(8, e))?;
        crate::crt::install(&mut child).map_err(|e| Error::new(8, e))?;
        install_graphics(&mut child, self.family.width, self.family.height)
            .map_err(|e| Error::new(8, e))?;
        #[cfg(feature = "gpu")]
        {
            adv_dx::d3d9::install(&mut child, crate::graphics::d3d9_provider())
                .map_err(|e| Error::new(8, e))?;
            adv_dx::d3d9::fork_host_state(&self.machine.ext, &mut child.ext)
                .map_err(|e| Error::new(8, e))?;
        }
        let sound = self.machine.ext.get::<adv_dx::dsound::Sound>();
        let enabled = sound.enabled;
        let digests = sound.capture_digests;
        adv_dx::dsound::install_with_audio(&mut child, sound.audio.clone())
            .map_err(|e| Error::new(8, e))?;
        let sound = child.ext.get_mut::<adv_dx::dsound::Sound>();
        sound.enabled = enabled;
        sound.capture_digests = digests;
        adv_dx::dshow::install(&mut child);
        adv_dx::dxdiag::install(&mut child);
        let main = child
            .start_process(exe)
            .map_err(|e| Error::new(8, e.to_string()))?;
        let entry = adv_os::modules(&child.ext).get(exe).entry;
        child
            .visit_context(Some(main), |env| {
                env.cpu.set_reg(Reg::Eax, entry);
                env.cpu.set_reg(Reg::Ebx, adv_machine::layout::PEB_ADDR);
            })
            .map_err(|e| Error::new(8, e))?;
        if spec.suspended {
            child.sys.suspend_context(main);
        }
        let parent_control = self.machine.ext.get::<RunControl>();
        let control = child.ext.get_mut::<RunControl>();
        control.frame_limit = parent_control.frame_limit;
        control.frame_timing = parent_control.frame_timing;
        let created = Created {
            process: child.sys.process_identity.clone(),
            thread: child.sys.thread_identity(main).unwrap(),
            publication: Rc::new(Cell::new(processes::Publication::Pending)),
        };
        self.family
            .publication
            .insert(pid, created.publication.clone());
        self.family
            .services
            .register_child(&created.process, spec.parent);
        self.family.records.push(ProcessStatus {
            pid,
            parent: Some(spec.parent),
            image: exe_path,
            exit_code: None,
            failure: None,
            missing_apis: Vec::new(),
            acceptance_eligible: true,
            structural_mismatches: 0,
            structural_warnings: Vec::new(),
            first_cpu_fault: None,
            latest_cpu_fault: None,
        });
        self.family.prepared.insert(pid, prepared);
        // Local timed-wait acceleration is unsound while another address space
        // can run. Family-wide acceleration is deliberately disabled here.
        self.machine.set_time_skip_source(None);
        child.set_time_skip_source(None);
        for sibling in self.family.parked.values_mut() {
            sibling.set_time_skip_source(None);
        }
        self.family.parked.insert(pid, child);
        Ok(created)
    }

    fn service_requests(&mut self) {
        let caller = self.machine.sys.pid;
        while let Some(request) = self.family.services.pop() {
            match request {
                Request::Spawn { spec, reply } => {
                    *reply.borrow_mut() = Some(self.create_child(*spec));
                }
                Request::Visit { pid, context, task } => {
                    if self.family.completed.contains(&pid)
                        || !self.select_process(pid)
                        || self
                            .machine
                            .sys
                            .process_identity
                            .completion
                            .exit_code()
                            .is_some()
                    {
                        task(Err(Error::new(87, "process has exited")));
                    } else {
                        let mut task = Some(task);
                        if let Err(error) = self
                            .machine
                            .visit_context(context, |env| task.take().unwrap()(Ok(env)))
                            && let Some(task) = task
                        {
                            task(Err(Error::new(87, error)));
                        }
                    }
                }
            }
        }
        self.select_process(caller);
    }

    fn initialize_selected(&mut self) -> Result<(), String> {
        let pid = self.machine.sys.pid;
        if !self.machine.sys.contexts.iter().any(|c| c.is_ready()) {
            return Ok(());
        }
        if let Some(prepared) = self.family.prepared.remove(&pid) {
            let (win32, modules) = self.machine.ext.get2_mut::<Win32, Modules>();
            let mut loader = Loader::new(
                &mut self.machine.cpu,
                &mut self.machine.sys,
                modules,
                &win32.vfs,
            );
            loader.finish_process(prepared).map_err(|e| e.to_string())?;
            let inits = std::mem::take(&mut loader.inits);
            drop(loader);
            // Infrastructure precedes image initialization. The machine runs the
            // separate readiness queue last, then resumes the requested context.
            self.machine.sys.process_init.extend(inits);
            // A resumed executable owns the session's display/input from here.
            self.set_foreground(pid)?;
        }
        Ok(())
    }

    fn record_exit(&mut self, code: u32, failure: Option<String>) {
        let pid = self.machine.sys.pid;
        if !self.family.completed.insert(pid) {
            return;
        }
        self.machine.sys.terminate_process_stopped(code);
        self.family.prepared.remove(&pid);
        self.family.publication.remove(&pid);
        let missing = self.selected_missing_apis();
        let eligible = self.selected_api_acceptance_eligible();
        let (structural_mismatches, structural_warnings) = self.process_structural_diagnostics(pid);
        let record = self
            .family
            .records
            .iter_mut()
            .find(|p| p.pid == pid)
            .unwrap();
        record.exit_code = Some(code);
        record.failure = failure;
        record.missing_apis = missing;
        record.acceptance_eligible = eligible;
        record.structural_mismatches = structural_mismatches;
        record.structural_warnings = structural_warnings;
        record.first_cpu_fault = self.machine.sys.first_cpu_fault.clone();
        record.latest_cpu_fault = self.machine.sys.latest_cpu_fault.clone();
        #[cfg(feature = "gpu")]
        adv_dx::d3d9::cancel_pending(&mut self.machine);
        adv_dx::dsound::shutdown(&mut self.machine.ext);
    }

    fn reap(&mut self) {
        let completed: Vec<_> = self
            .family
            .parked
            .keys()
            .copied()
            .filter(|pid| *pid != self.family.foreground && self.family.completed.contains(pid))
            .collect();
        for pid in completed {
            if adv_dx::dsound::shutdown(&mut self.family.parked.get_mut(&pid).unwrap().ext) {
                self.family.parked.remove(&pid);
            }
        }
    }

    pub(crate) fn step_family(&mut self, slice: Option<Duration>) -> SliceResult {
        // Until a child is created, retain Machine's original scheduling and
        // lease boundaries. In particular, step() is unsliced and step_for()
        // receives its full requested budget, not the family fairness quantum.
        if self.family.records.len() == 1 && self.family.completed.is_empty() {
            let outcome = match self.machine.sys.process_identity.completion.exit_code() {
                Some(code) => SliceResult::Complete(RunResult::Exited(code)),
                None => match slice {
                    Some(slice) => self.machine.run_slice(slice),
                    None => SliceResult::Complete(self.machine.run()),
                },
            };
            let outcome = self.finish_step(outcome);
            match &outcome {
                SliceResult::Complete(RunResult::HostRequest) => {
                    self.service_requests();
                    return SliceResult::Yielded;
                }
                SliceResult::Complete(RunResult::Exited(code)) => self.record_exit(*code, None),
                SliceResult::Complete(RunResult::Failed(error)) => {
                    let code = self.machine.sys.guest_failure_code.unwrap_or(0xc000_00bb);
                    self.record_exit(code, Some(error.message.clone()));
                    self.family.first_failure.get_or_insert(error.clone());
                }
                _ => {}
            }
            return outcome;
        }
        let slice = slice.unwrap_or(Duration::from_millis(10));
        let started = Instant::now();
        let mut visited = BTreeSet::new();
        let mut any_yielded = false;
        let mut deadline: Option<u64> = None;
        loop {
            self.service_requests();
            self.reap();
            let live: Vec<_> = self
                .family
                .records
                .iter()
                .filter(|p| !self.family.completed.contains(&p.pid))
                .map(|p| p.pid)
                .collect();
            if live.is_empty() {
                self.select_process(self.family.foreground);
                if let Some(error) = &self.family.first_failure {
                    return SliceResult::Complete(RunResult::Failed(error.clone()));
                }
                return SliceResult::Complete(RunResult::Exited(
                    self.family
                        .records
                        .iter()
                        .filter_map(|p| p.exit_code)
                        .find(|code| *code != 0)
                        .unwrap_or(0),
                ));
            }
            // An exiting foreground yields to the oldest surviving child even
            // if it is still suspended; a live parent waiting on a child keeps
            // receiving scheduling turns in the background.
            if self.family.completed.contains(&self.family.foreground)
                && let Some(pid) = self
                    .family
                    .records
                    .iter()
                    .find(|p| !self.family.completed.contains(&p.pid))
                    .map(|p| p.pid)
                && let Err(message) = self.set_foreground(pid)
            {
                return SliceResult::Complete(RunResult::Failed(MachineError {
                    ctx: self.machine.sys.current,
                    pc: self.machine.cpu.reg(Reg::Eip),
                    message,
                    recent: self.machine.sys.recent_calls(),
                }));
            }
            let next = live
                .iter()
                .copied()
                .find(|p| *p > self.family.last && !visited.contains(p))
                .or_else(|| live.iter().copied().find(|p| !visited.contains(p)));
            let Some(pid) = next else {
                // A parent can publish a child after that child's pending ticket
                // was visited in this round, then immediately wait for it.
                // Give newly published runnable work a turn before host sleep.
                let published_ready =
                    self.family.publication.iter().any(|(pid, ticket)| {
                        ticket.get() == processes::Publication::Published
                            && self.family.parked.get(pid).is_some_and(|child| {
                                child.sys.contexts.iter().any(|c| c.is_ready())
                            })
                    });
                self.select_process(self.family.foreground);
                if any_yielded || published_ready {
                    self.machine.sys.clock.begin_work();
                    return SliceResult::Yielded;
                }
                self.machine.sys.clock.begin_wait(deadline);
                return SliceResult::Complete(RunResult::Idle {
                    next_deadline: deadline,
                });
            };
            self.family.last = pid;
            visited.insert(pid);
            self.select_process(pid);
            self.machine.sys.clock.begin_work();
            if let Some(code) = self.machine.sys.process_identity.completion.exit_code() {
                self.record_exit(code, None);
                any_yielded = true;
                continue;
            }
            if let Some(publication) = self.family.publication.get(&pid).map(|ticket| ticket.get())
            {
                if publication == processes::Publication::Cancelled {
                    self.record_exit(995, None);
                    any_yielded = true;
                    continue;
                }
                if publication == processes::Publication::Pending {
                    continue;
                }
            }
            self.family.publication.remove(&pid);
            if let Err(message) = self.initialize_selected() {
                self.record_exit(0xc000_0142, Some(message.clone()));
                any_yielded = true;
                self.family
                    .first_failure
                    .get_or_insert_with(|| MachineError {
                        ctx: self.machine.sys.current,
                        pc: self.machine.cpu.reg(Reg::Eip),
                        message: format!("process {pid}: {message}"),
                        recent: self.machine.sys.recent_calls(),
                    });
                continue;
            }
            let outcome =
                if let Some(code) = self.machine.sys.process_identity.completion.exit_code() {
                    SliceResult::Complete(RunResult::Exited(code))
                } else {
                    let remaining = slice
                        .saturating_sub(started.elapsed())
                        .max(Duration::from_micros(1));
                    let outcome = self
                        .machine
                        .run_slice(remaining.min(Duration::from_millis(2)));
                    self.finish_step(outcome)
                };
            match outcome {
                SliceResult::Complete(RunResult::HostRequest) => {
                    self.service_requests();
                    any_yielded = true;
                }
                SliceResult::Complete(RunResult::Exited(code)) => {
                    self.record_exit(code, None);
                    // Earlier peers may now have satisfied process/thread waits.
                    any_yielded = true;
                }
                SliceResult::Complete(RunResult::Failed(error)) => {
                    // A guest exception retains its native status. A missing
                    // implementation or host-device error is NOT_SUPPORTED.
                    let code = self.machine.sys.guest_failure_code.unwrap_or(0xc000_00bb);
                    self.record_exit(code, Some(error.message.clone()));
                    any_yielded = true;
                    self.family.first_failure.get_or_insert(error);
                    continue;
                }
                SliceResult::Complete(RunResult::Idle { next_deadline }) => {
                    if let Some(at) = next_deadline {
                        deadline = Some(deadline.map_or(at, |old| old.min(at)));
                    }
                }
                SliceResult::Yielded => {
                    any_yielded = true;
                }
            }
            if started.elapsed() >= slice && any_yielded {
                self.select_process(self.family.foreground);
                self.machine.sys.clock.begin_work();
                return SliceResult::Yielded;
            }
        }
    }
}

#[cfg(test)]
mod tests;
