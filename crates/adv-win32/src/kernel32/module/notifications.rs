//! Win32 module notification policy, composed exclusively with Machine outcomes.
use super::*;
use adv_cpu::backend::Backend;
use adv_cpu::backend::{MemFault, Reg};
use adv_machine::CtxId;
use adv_machine::Machine;
use adv_machine::outcome::DeferredContinuation;

pub(super) const INTERNAL: &str = "<win32>";
pub(super) const THREAD_START: &str = "thread_start";

mod shutdown;
#[cfg(test)]
mod tests;
pub(crate) use shutdown::shutdown_process;

pub(crate) fn install_startup<B: Backend>(machine: &mut Machine<B>) -> Result<(), MemFault> {
    machine.sys.process_return = Some(exit_process);
    // Installing API providers does not start a process. Prepare resident images
    // at the initial-context boundary, including a process created suspended.
    machine.sys.process_prepare = Some(|cpu, sys, ext| {
        let (win32, modules) = ext.get2_mut::<Win32, Modules>();
        resident_system_modules(cpu, sys, win32, modules).map_err(|_| MemFault { addr: 0, len: 0 })
    });
    if machine
        .sys
        .thunks
        .resolver()
        .host_fn(INTERNAL, "startup_enter")
        .is_none()
    {
        return Ok(());
    }
    let enter = machine
        .sys
        .thunks
        .resolve(&mut machine.cpu, INTERNAL, "startup_enter")?;
    let leave = machine
        .sys
        .thunks
        .resolve(&mut machine.cpu, INTERNAL, "startup_leave")?;
    machine.sys.process_init.push_front(InitCall {
        target: enter,
        args: vec![],
        what: "Win32 startup gate acquire".into(),
        failure_exit: Some(0xc0000017),
        begin: None,
    });
    machine.sys.process_ready.push(InitCall {
        target: leave,
        args: vec![],
        what: "Win32 startup gate release".into(),
        failure_exit: Some(0xc0000142),
        begin: None,
    });
    Ok(())
}

pub(super) fn startup_enter(env: &mut Env<'_>) -> Outcome {
    let Some(handle) = loader_mutex(env) else {
        return Outcome::Return(0);
    };
    if env.sys.try_acquire_objects(&[handle], false).is_some() {
        return startup_system_modules(env, handle);
    }
    Outcome::block(
        Wait::Objects {
            handles: vec![handle],
            all: false,
            deadline: None,
            notification: None,
        },
        move |env, _| startup_system_modules(env, handle),
    )
}

/// Win32 process infrastructure is resident before any guest initializer,
/// even when the executable has no import descriptor naming these DLLs.
/// Keep this in the personality startup gate, not GetModuleHandle: queries
/// must never implicitly load arbitrary modules or change reference counts.
fn startup_system_modules(env: &mut Env<'_>, handle: u32) -> Outcome {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    let result = resident_system_modules(env.cpu, env.sys, win32, modules);
    match result {
        Ok(()) => Outcome::Return(1),
        Err(error) => release_lock(env, handle, Outcome::Fail(error)),
    }
}

pub(crate) fn prepare_system_modules<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
) -> Result<(), String> {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    resident_system_modules(&mut machine.cpu, &mut machine.sys, win32, modules)
}

pub(crate) fn prepare_system_env(env: &mut Env<'_>) -> Result<(), MemFault> {
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    resident_system_modules(env.cpu, env.sys, win32, modules)
        .map_err(|_| MemFault { addr: 0, len: 0 })
}

fn resident_system_modules(
    cpu: &mut dyn adv_cpu::backend::Guest,
    sys: &mut adv_machine::Sys,
    win32: &Win32,
    modules: &mut Modules,
) -> Result<(), String> {
    for name in ["ntdll.dll", "kernel32.dll"] {
        if modules.by_name(name).is_some() {
            continue;
        }
        let mut loader = Loader::new(cpu, sys, modules, &win32.vfs);
        loader
            .load(&format!("C:\\Windows\\System32\\{name}"))
            .map_err(|error| format!("Win32 startup {name}: {error}"))?;
        // Synthetic system images have no guest TLS/DllMain callbacks.
        debug_assert!(loader.inits.is_empty());
    }
    let id = modules.by_name("ntdll.dll").expect("resident ntdll");
    let module = modules.get(id);
    if let Some(adv_pe::ExportTarget::Rva(rva)) = module.exports_by_name.get("LdrInitializeThunk") {
        sys.set_process_start_entry(module.base + rva);
    }
    Ok(())
}

fn system_entry(env: &Env<'_>, module: &str, name: &str) -> Result<u32, MemFault> {
    let modules = adv_os::modules(env.ext);
    let id = modules
        .by_name(module)
        .ok_or(MemFault { addr: 0, len: 0 })?;
    let image = modules.get(id);
    match image.exports_by_name.get(name) {
        Some(adv_pe::ExportTarget::Rva(rva)) => Ok(image.base + rva),
        _ => Err(MemFault {
            addr: image.base,
            len: image.size,
        }),
    }
}

pub(super) fn register_start_entries(registry: &mut Registry) {
    stdcall(registry, "ntdll.dll", "LdrInitializeThunk", 0, |env| {
        let entry = env.cpu.reg(Reg::Eax);
        adv_machine::process_entry_at(env, entry)
    });
    stdcall(
        registry,
        "ntdll.dll",
        "RtlUserThreadStart",
        2,
        user_thread_start,
    );
    stdcall(
        registry,
        "kernel32.dll",
        "BaseThreadInitThunk",
        3,
        base_thread_start,
    );
    stdcall(registry, INTERNAL, "thread_exception_handler", 4, |_| {
        // A real terminal registration participates in dispatch and unwind.
        // Existing personality policy decides uncaught exceptions at chain end.
        Outcome::Return(1) // ExceptionContinueSearch
    });
}

fn user_thread_start(env: &mut Env<'_>) -> Outcome {
    // The suspended x86 initial image exposes the actual routine and argument.
    // Consume the requested registers, including a caller's SetThreadContext edits.
    let (start, param) = (env.cpu.reg(Reg::Eax), env.cpu.reg(Reg::Ebx));
    let target = match system_entry(env, "kernel32.dll", "BaseThreadInitThunk") {
        Ok(target) => target,
        Err(error) => return Outcome::Fail(format!("thread startup image entry: {error}")),
    };
    Outcome::call(target, [0, start, param], |env, returned| {
        exit_thread(env, returned.value())
    })
}

fn base_thread_start(env: &mut Env<'_>) -> Outcome {
    thread_start_at(env, env.arg(1), env.arg(2))
}

pub(super) fn startup_leave(env: &mut Env<'_>) -> Outcome {
    let Some(handle) = env.ext.get::<Win32>().loader_lock else {
        return Outcome::Fail("missing startup loader lock".into());
    };
    release_lock(env, handle, Outcome::Return(1))
}

struct Notification {
    module: ModuleId,
    target: u32,
    reason: u32,
    reserved: u32,
    dllmain: bool,
}

fn notifications(env: &Env<'_>, reason: u32) -> VecDeque<Notification> {
    let modules = adv_os::modules(env.ext);
    let mut order = modules.initialization_order();
    if reason != 2 {
        order.reverse();
    }
    let mut calls = VecDeque::new();
    for id in order {
        let module = modules.get(id);
        if !module.lifetime.attach_started || module.lifetime.unloading {
            continue;
        }
        for &target in &module.lifetime.callbacks {
            calls.push_back(Notification {
                module: id,
                target,
                reason,
                reserved: 0,
                dllmain: false,
            });
        }
        if module.is_dll && module.entry != 0 && (reason == 0 || module.lifetime.thread_calls) {
            calls.push_back(Notification {
                module: id,
                target: module.entry,
                reason,
                reserved: u32::from(reason == 0),
                dllmain: true,
            });
        }
    }
    calls
}

fn run(
    env: &mut Env<'_>,
    mut calls: VecDeque<Notification>,
    then: DeferredContinuation,
) -> Outcome {
    while let Some(call) = calls.pop_front() {
        // A prior callback can unload another module or disable notifications.
        // Never reinterpret a retired identity as a later image at the same base.
        let Some(module) = adv_os::modules(env.ext).try_get(call.module) else {
            continue;
        };
        if call.reason != 0 && call.dllmain && !module.lifetime.thread_calls {
            continue;
        }
        return Outcome::call(
            call.target,
            vec![module.base, call.reason, call.reserved],
            move |env, _| run(env, calls, then),
        );
    }
    then(env)
}

/// Terminal APIs must never turn a loader-lock allocation failure into a return.
/// With no callbacks, cleanup itself needs no loader mutex. If we must wait for
/// the lock, collect again after acquisition so intervening module changes are
/// observed; `run` also revalidates each identity before invoking guest code.
fn terminal_notifications(env: &mut Env<'_>, reason: u32, then: DeferredContinuation) -> Outcome {
    if notifications(env, reason).is_empty() {
        return then(env);
    }
    if loader_mutex(env).is_none() {
        return Outcome::Fail("terminal DLL/TLS notification loader lock allocation failed".into());
    }
    locked(env, move |env| {
        let calls = notifications(env, reason);
        run(env, calls, then)
    })
}

pub(crate) fn spawn_thread(
    env: &mut Env<'_>,
    name: &str,
    start: u32,
    param: u32,
    stack: u32,
) -> Result<CtxId, MemFault> {
    if env.ext.try_get::<Modules>().is_none() {
        // Embedders may exercise internal workers without a Win32 process.
        let wrapper = env.sys.thunks.resolve(env.cpu, INTERNAL, THREAD_START)?;
        return env
            .sys
            .spawn(env.cpu, name, wrapper, &[start, param], stack);
    }
    let (win32, modules) = env.ext.get2_mut::<Win32, Modules>();
    resident_system_modules(env.cpu, env.sys, win32, modules)
        .map_err(|_| MemFault { addr: 0, len: 0 })?;
    let wrapper = system_entry(env, "ntdll.dll", "RtlUserThreadStart")?;
    let id = env
        .sys
        .spawn(env.cpu, name, wrapper, &[start, param], stack)?;
    env.sys
        .set_initial_registers(id, &[(Reg::Eax, start), (Reg::Ebx, param)]);
    Ok(id)
}

pub(super) fn thread_start(env: &mut Env<'_>) -> Outcome {
    let (start, param) = (env.arg(0), env.arg(1));
    thread_start_at(env, start, param)
}

fn thread_start_at(env: &mut Env<'_>, start: u32, param: u32) -> Outcome {
    // Acquire only around notifications; the thread procedure itself must run
    // after releasing the loader lock or thread joins would deadlock.
    let locked_start = locked(env, |env| {
        let calls = notifications(env, 2);
        run(env, calls, Box::new(|_| Outcome::Return(0)))
    });
    after(
        env,
        locked_start,
        Box::new(move |env| {
            super::super::process::apc::drain(
                env,
                Box::new(move |_| {
                    Outcome::call(start, vec![param], |env, returned| {
                        exit_thread(env, returned.value())
                    })
                }),
            )
        }),
    )
}

pub(super) fn after(env: &mut Env<'_>, outcome: Outcome, then: DeferredContinuation) -> Outcome {
    match outcome {
        Outcome::Call {
            target,
            args,
            then: resume,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            stack,
            unwindable,
            then: Box::new(move |env, value| {
                let next = resume(env, value);
                after(env, next, then)
            }),
        },
        Outcome::Block {
            wait,
            then: resume,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, value| {
                let next = resume(env, value);
                after(env, next, then)
            }),
        },
        Outcome::Defer { then: resume } => Outcome::defer(move |env| {
            let next = resume(env);
            after(env, next, then)
        }),
        Outcome::Return(_) => then(env),
        terminal => terminal,
    }
}

pub(crate) fn exit_thread(env: &mut Env<'_>, code: u32) -> Outcome {
    super::super::process::apc::retire(env, env.context);
    if !env
        .ext
        .get_mut::<Win32>()
        .exiting_threads
        .insert(env.context)
    {
        return Outcome::ExitThread(code);
    }
    // FLS cleanup precedes DLL_THREAD_DETACH, while the registering module's
    // dependencies are still alive. Native authored DLL probes verify the order.
    super::super::sync::detach_fls(
        env,
        Box::new(move |env| {
            terminal_notifications(
                env,
                3,
                Box::new(move |env| {
                    crate::user32::clipboard::exit_thread(env, move |env| {
                        if let Err(error) = crate::msvcrt::detach_thread(env) {
                            return Outcome::Fail(error);
                        }
                        crate::user32::message::sent::exit_thread(env);
                        crate::user32::hooks::exit_thread(env);
                        env.ext
                            .get_mut::<Win32>()
                            .exiting_threads
                            .remove(&env.context);
                        if env
                            .sys
                            .contexts
                            .iter()
                            .all(|context| context.id == env.context || context.is_done())
                        {
                            exit_process(env, code)
                        } else {
                            Outcome::ExitThread(code)
                        }
                    })
                }),
            )
        }),
    )
}

pub(crate) fn exit_process(env: &mut Env<'_>, code: u32) -> Outcome {
    if let Some(out) = crate::gdi32::raster::front::flush_software(
        env,
        adv_device::surface::FlushReason::Immediate,
    ) {
        return out;
    }
    super::super::process::apc::retire(env, env.context);
    if env.sys.process_identity.completion.exit_status().is_some() {
        return Outcome::ExitProcess(code);
    }
    if std::mem::replace(&mut env.ext.get_mut::<Win32>().process_exiting, true) {
        return Outcome::ExitProcess(code);
    }
    // DLL detach callbacks must not race a native lease belonging to a thread
    // that Windows has already terminated as part of process shutdown.
    env.cpu.quiesce_memory();
    crate::user32::hooks::exit_process(env);
    env.sys.terminate_other_contexts(code);
    super::super::process::apc::retire_completed(env);
    let shutdown = shutdown_process(env);
    after(env, shutdown, Box::new(move |_| Outcome::ExitProcess(code)))
}
