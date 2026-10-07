//! Process, thread, environment, version, and error-reporting functions.

use adv_abi::{Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{Env, Object, Outcome, Wait, process::ThreadIdentity};
use adv_os::{Heaps, processes};

use super::K32;
use crate::util::{cstr, err, out_u32, put_cstr, set_last_error, wstr, zero};
use crate::{STD_ERROR, STD_INPUT, STD_OUTPUT, Win32, stdcall};
mod parameters;
mod processor;
#[cfg(test)]
mod tests;
mod thread_context;
pub(crate) use parameters::install as install_parameters;

#[win32(kernel32, name = "GetConsoleWindow", level = state)]
fn get_console_window(_env: &mut Env<'_>) -> Handle {
    // Guest standard streams do not allocate a console window. Query that
    // absence without borrowing a host console HWND or changing LastError.
    Handle(0)
}

fn exit_process(env: &mut Env<'_>) -> Outcome {
    super::module::exit_process(env, env.arg(0))
}

fn terminate_process(env: &mut Env<'_>) -> Outcome {
    let code = env.arg(1);
    let identity = match super::child_process::process_target(env, env.arg(0), 1) {
        Ok(identity) => identity,
        Err(error) => return api_error(env, error, 0),
    };
    if identity.completion.exit_code().is_some() {
        return api_error(
            env,
            processes::Error::new(err::ACCESS_DENIED, "process has exited"),
            0,
        );
    }
    if identity.pid == env.sys.pid {
        return Outcome::ExitProcess(code);
    }
    processes::visit(
        env,
        identity.pid,
        None,
        move |target| {
            target.sys.terminate_process_stopped(code);
            Ok(1)
        },
        |env, result| api_result(env, result, 0),
    )
}

fn api_error(env: &mut Env<'_>, error: processes::Error, failure: u32) -> Outcome {
    set_last_error(env, error.code);
    Outcome::Return(failure)
}

fn api_result(env: &mut Env<'_>, result: Result<u32, processes::Error>, failure: u32) -> Outcome {
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => api_error(env, error, failure),
    }
}

fn open_process(env: &mut Env<'_>) -> Outcome {
    let (mut access, pid) = (env.arg(0), env.arg(2));
    let identity = if pid == env.sys.pid {
        Some(env.sys.process_identity.clone())
    } else {
        env.ext
            .try_get::<processes::Services>()
            .and_then(|services| services.find(pid))
    };
    let Some(identity) = identity else {
        return api_error(
            env,
            processes::Error::new(err::INVALID_PARAMETER, "unknown process id"),
            0,
        );
    };
    if access & 0x1000_0000 != 0 {
        access = 0x1f_ffff;
    } // GENERIC_ALL
    // Query-information grants the limited query operations too.
    if access & 0x400 != 0 {
        access |= 0x1000;
    }
    let handle = env.sys.objects.insert(Object::Process { identity, access });
    if handle == 0 {
        return api_error(
            env,
            processes::Error::new(err::NOT_ENOUGH_MEMORY, "process handle table full"),
            0,
        );
    }
    Outcome::Return(handle)
}

fn get_exit_code_process(env: &mut Env<'_>) -> Outcome {
    let identity = match super::child_process::process_target(env, env.arg(0), 0x1000) {
        Ok(identity) => identity,
        Err(error) => return api_error(env, error, 0),
    };
    write_exit_code(
        env,
        env.arg(1),
        identity.completion.exit_code().unwrap_or(259),
    )
}

fn write_exit_code(env: &mut Env<'_>, out: u32, code: u32) -> Outcome {
    if out == 0 {
        return api_error(
            env,
            processes::Error::new(err::INVALID_PARAMETER, "null exit-code output"),
            0,
        );
    }
    if adv_abi::guest::check_range(env.cpu, out, 4, Prot::WRITE).is_err()
        || env.cpu.write_u32(out, code).is_err()
    {
        return api_error(
            env,
            processes::Error::new(998, "unwritable exit-code output"),
            0,
        );
    }
    Outcome::Return(1)
}

fn thread_target(
    env: &Env<'_>,
    handle: u32,
    access: u32,
) -> Result<ThreadIdentity, processes::Error> {
    match env.sys.objects.get(handle) {
        _ if handle == u32::MAX - 1 => env.sys.thread_identity(env.context),
        Some(Object::Thread { ctx }) => env.sys.thread_identity(*ctx),
        Some(Object::RemoteThread {
            identity,
            access: granted,
        }) => {
            let permitted = if access == 0x40 {
                granted & (0x40 | 0x800) != 0
            } else {
                granted & access == access
            };
            if !permitted {
                return Err(processes::Error::new(
                    err::ACCESS_DENIED,
                    "thread handle access denied",
                ));
            }
            return Ok(identity.clone());
        }
        _ => None,
    }
    .ok_or_else(|| processes::Error::new(err::INVALID_HANDLE, "invalid thread handle"))
}

fn flush_instruction_cache(env: &mut Env<'_>) -> Outcome {
    // Memory writes invalidate translated blocks when they are committed; no
    // additional host instruction-cache operation is needed for a guest range.
    api_result(
        env,
        super::child_process::process_target(env, env.arg(0), 0).map(|_| 1),
        0,
    )
}

pub(crate) fn command_line(env: &mut Env<'_>, wide: bool) -> u32 {
    let supplied = env
        .ext
        .try_get::<adv_os::processes::Parameters>()
        .map(|p| p.command_line.clone());
    let (w, heaps) = env.ext.get2_mut::<Win32, Heaps>();
    let key = if wide { "cmdline_w" } else { "cmdline_a" };
    if let Some(&pointer) = w.statics.get(key) {
        return pointer;
    }
    let line = supplied.unwrap_or_else(|| format!("\"{}\"", w.exe_path()));
    let bytes = if wide {
        line.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut bytes = w.codec.encode(&line);
        bytes.push(0);
        // Reserve enough room for a later explicitly selected code page while
        // retaining the address stored in each CRT data cell.
        bytes.resize(line.encode_utf16().count() * 4 + 1, 0);
        bytes
    };
    w.static_bytes(key, env.cpu, env.sys, heaps, &bytes)
}

pub(crate) fn configure_codepage(
    env: &mut Env<'_>,
    codepage: u32,
) -> Result<(), adv_cpu::backend::MemFault> {
    let w = env.ext.get::<Win32>();
    if w.cfg.os.codepage == codepage {
        return Ok(());
    }
    let codec = adv_os::Codec::new(codepage);
    if let Some(&pointer) = w.statics.get("cmdline_a") {
        let line = env
            .ext
            .try_get::<adv_os::processes::Parameters>()
            .map(|p| p.command_line.clone())
            .unwrap_or_else(|| format!("\"{}\"", w.exe_path()));
        let mut bytes = codec.encode(&line);
        bytes.push(0);
        let length = bytes.len() as u32;
        if env
            .ext
            .get_mut::<Heaps>()
            .size(0, pointer)
            .is_none_or(|size| size < length)
        {
            return Err(adv_cpu::backend::MemFault {
                addr: pointer,
                len: length,
            });
        }
        adv_abi::guest::check_range(env.cpu, pointer, length, adv_cpu::backend::Prot::WRITE)?;
        env.cpu.write(pointer, &bytes)?;
    }
    let w = env.ext.get_mut::<Win32>();
    w.cfg.os.codepage = codepage;
    w.codec = codec;
    Ok(())
}

fn command_line_a(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(command_line(env, false))
}
fn command_line_w(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(command_line(env, true))
}

pub(super) fn environment(w: &Win32) -> Vec<String> {
    vec![
        format!("=C:={}", w.vfs.cwd),
        "ALLUSERSPROFILE=C:\\ProgramData".into(),
        format!(
            "APPDATA=C:\\Users\\{}\\AppData\\Roaming",
            w.cfg.os.user_name
        ),
        "COMPUTERNAME=PAL-HLE".into(),
        format!(
            "LOCALAPPDATA=C:\\Users\\{}\\AppData\\Local",
            w.cfg.os.user_name
        ),
        "OS=Windows_NT".into(),
        "PATH=C:\\Windows\\system32;C:\\Windows".into(),
        "SystemDrive=C:".into(),
        "SystemRoot=C:\\Windows".into(),
        format!(
            "TEMP=C:\\Users\\{}\\AppData\\Local\\Temp",
            w.cfg.os.user_name
        ),
        format!(
            "TMP=C:\\Users\\{}\\AppData\\Local\\Temp",
            w.cfg.os.user_name
        ),
        format!("USERNAME={}", w.cfg.os.user_name),
        format!("USERPROFILE=C:\\Users\\{}", w.cfg.os.user_name),
        "windir=C:\\Windows".into(),
    ]
}

/// STARTUPINFO: cb = 68, everything else zero.
fn get_startup_info(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    zero(env.cpu, p, 68);
    let _ = env.cpu.write_u32(p, 68);
    Outcome::Return(0)
}

fn get_std_handle(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(match env.arg(0) as i32 {
        -10 => STD_INPUT,
        -11 => STD_OUTPUT,
        -12 => STD_ERROR,
        _ => u32::MAX,
    })
}

fn is_processor_feature_present(env: &mut Env<'_>) -> Outcome {
    let bit = match env.arg(0) {
        2 => 8,   // CMPXCHG8B
        3 => 23,  // MMX
        6 => 25,  // SSE
        8 => 4,   // RDTSC
        10 => 26, // SSE2
        _ => return Outcome::Return(0),
    };
    Outcome::Return(u32::from(env.cpu.cpuid_features() & (1 << bit) != 0))
}

fn set_unhandled_exception_filter(env: &mut Env<'_>) -> Outcome {
    let f = env.arg(0);
    let w = env.ext.get_mut::<Win32>();
    Outcome::Return(std::mem::replace(&mut w.unhandled_filter, f))
}

fn unhandled_exception_filter(env: &mut Env<'_>) -> Outcome {
    let filter = env.ext.get::<Win32>().unhandled_filter;
    if filter == 0 {
        return Outcome::Return(0); // EXCEPTION_CONTINUE_SEARCH, without a guest filter.
    }
    let pointers = env.arg(0);
    // The caller owns EXCEPTION_POINTERS and CONTEXT. A CRT frame filter may
    // pass the dispatcher's records here, then resume their edited context when
    // the registered top-level filter returns EXCEPTION_CONTINUE_EXECUTION.
    Outcome::call(filter, [pointers], |_, result| {
        Outcome::Return(result.value())
    })
    .unwindable()
}

const EXCEPTION_UNWINDING: u32 = 2;
const EXCEPTION_EXIT_UNWIND: u32 = 4;
const STATUS_UNWIND: u32 = 0xc000_0027;
/// Guest scratch for an unwind: EXCEPTION_RECORD, CONTEXT, dispatcher context.
const RECORD: u32 = 0;
const CONTEXT: u32 = 0x50;
const DISPATCHER: u32 = 0x320;
const HANDLER_FRAME: u32 = 0x330;
const SCRATCH: u32 = HANDLER_FRAME + super::exception::HANDLER_FRAME_SIZE;

struct Unwind {
    target: u32,
    value: u32,
    record: u32,
    scratch: u32,
    /// EBX, ESI, EDI, EBP at entry, which the resumed context restores.
    saved: [u32; 4],
    _phase: std::rc::Rc<super::exception::phase::Phase>,
}

const SAVED: [Reg; 4] = [Reg::Ebx, Reg::Esi, Reg::Edi, Reg::Ebp];

/// RtlUnwind(target_frame, target_ip, record, return_value): call each SEH handler on the
/// `fs:[0]` chain above `target_frame` with an unwinding record, unlinking each as it
/// returns, then resume after the call with EAX = `return_value` and the callee-saved
/// registers of the entry. Windows resumes at `target_ip` with the entry context; the x86
/// MSVC callers (`_global_unwind2`, `longjmp`, `_UnwindNestedFrames`) pass the label right
/// after their call, so any other target is refused.
fn rtl_unwind(env: &mut Env<'_>) -> Outcome {
    let (target, ip, record, value) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let ret = env.caller();
    if ip != 0 && ip != ret {
        return Outcome::Fail(format!(
            "RtlUnwind to {ip:#010x} from {ret:#010x}: only the return address is supported"
        ));
    }
    let code = if record == 0 {
        STATUS_UNWIND
    } else {
        match env.cpu.read_u32(record) {
            Ok(code) => code,
            Err(error) => {
                return Outcome::Fail(format!("RtlUnwind: unreadable exception code: {error}"));
            }
        }
    };
    let heaps = env.ext.get_mut::<Heaps>();
    let Some(scratch) = heaps.alloc(env.cpu, &mut env.sys.vm, 0, SCRATCH, true) else {
        return Outcome::Fail("RtlUnwind: out of memory".into());
    };
    let flags = EXCEPTION_UNWINDING
        | if target == 0 {
            EXCEPTION_EXIT_UNWIND
        } else {
            0
        };
    let record = if record == 0 {
        let r = scratch + RECORD;
        for (off, v) in [(0, STATUS_UNWIND), (4, flags), (0xc, ret)] {
            let _ = env.cpu.write_u32(r + off, v);
        }
        r
    } else {
        let old = env.cpu.read_u32(record + 4).unwrap_or(0);
        let _ = env.cpu.write_u32(record + 4, old | flags);
        record
    };
    let saved = SAVED.map(|r| env.cpu.reg(r));
    // CONTEXT_CONTROL | CONTEXT_INTEGER: the context the handlers see is the resumed one.
    let c = scratch + CONTEXT;
    let esp = env.cpu.reg(Reg::Esp) + 4 + 16;
    let regs = [
        (0x00, 0x0001_0003),
        (0x9c, saved[2]),
        (0xa0, saved[1]),
        (0xa4, saved[0]),
        (0xb0, value),
        (0xb4, saved[3]),
        (0xb8, ret),
        (0xc4, esp),
    ];
    for (off, v) in regs {
        let _ = env.cpu.write_u32(c + off, v);
    }
    let active = super::exception::phase::enter(env, super::exception::phase::Phase::Unwind, code);
    unwind_step(
        env,
        Unwind {
            target,
            value,
            record,
            scratch,
            saved,
            _phase: active,
        },
    )
}

fn unwind_step(env: &mut Env<'_>, u: Unwind) -> Outcome {
    let teb = env.ctx().teb;
    let frame = env.cpu.read_u32(teb).unwrap_or(u32::MAX);
    if frame == u.target || frame == u32::MAX {
        for (r, v) in SAVED.into_iter().zip(u.saved) {
            env.cpu.set_reg(r, v);
        }
        let heaps = env.ext.get_mut::<Heaps>();
        heaps.free(env.cpu, &mut env.sys.vm, 0, u.scratch);
        return Outcome::Return(u.value);
    }
    if u.target != 0 && frame > u.target {
        return Outcome::Fail(format!(
            "RtlUnwind: frame {frame:#010x} is past target {:#010x}",
            u.target
        ));
    }
    let Ok(handler) = env.cpu.read_u32(frame + 4) else {
        return Outcome::Fail(format!("RtlUnwind: unreadable registration {frame:#010x}"));
    };
    let args = [u.record, frame, u.scratch + CONTEXT, u.scratch + DISPATCHER];
    let at = u.scratch + HANDLER_FRAME;
    if let Err(error) = super::exception::handler_frame(env, at, args, handler) {
        return Outcome::Fail(format!("RtlUnwind: handler frame write failed: {error}"));
    }
    Outcome::call(handler, args, move |env, r| {
        if r.value() != 1 {
            log::warn!(
                "RtlUnwind: handler {handler:#010x} returned {} (not ExceptionContinueSearch)",
                r.value()
            );
        }
        let next = env.cpu.read_u32(frame).unwrap_or(u32::MAX);
        let _ = env.cpu.write_u32(env.ctx().teb, next);
        unwind_step(env, u)
    })
}

fn output_debug_string_a(env: &mut Env<'_>) -> Outcome {
    let s = env
        .ext
        .get::<Win32>()
        .codec
        .decode(&cstr(env.cpu, env.arg(0)));
    log::info!("[guest] {}", s.trim_end());
    Outcome::Return(0)
}

fn output_debug_string_w(env: &mut Env<'_>) -> Outcome {
    log::info!(
        "[guest] {}",
        String::from_utf16_lossy(&wstr(env.cpu, env.arg(0))).trim_end()
    );
    Outcome::Return(0)
}

fn get_last_error(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(crate::util::last_error(env))
}

fn set_last_error_(env: &mut Env<'_>) -> Outcome {
    let e = env.arg(0);
    set_last_error(env, e);
    Outcome::Return(0)
}

fn create_thread(env: &mut Env<'_>) -> Outcome {
    let inherit = match super::handle::security_inherit(env, env.arg(0)) {
        Ok(inherit) => inherit,
        Err(code) => {
            return api_error(
                env,
                processes::Error::new(code, "invalid thread security attributes"),
                0,
            );
        }
    };
    let (stack, start, param, flags, tid_out) =
        (env.arg(1), env.arg(2), env.arg(3), env.arg(4), env.arg(5));
    if flags & !(4 | 0x10000) != 0 {
        return api_error(
            env,
            processes::Error::new(err::INVALID_PARAMETER, "unsupported thread creation flags"),
            0,
        );
    }
    if tid_out != 0 && adv_abi::guest::check_range(env.cpu, tid_out, 4, Prot::WRITE).is_err() {
        return api_error(
            env,
            processes::Error::new(998, "unwritable thread-id output"),
            0,
        );
    }
    let name = format!("T{}", env.sys.contexts.len());
    match super::module::spawn_thread(
        env,
        &name,
        start,
        param,
        if stack == 0 { 0x10_0000 } else { stack },
    ) {
        Ok(id) => {
            if flags & 4 != 0 {
                env.sys.suspend_context(id);
            }
            let c = env.sys.context(id);
            let (tid, h) = (c.tid, c.thread_handle);
            super::handle::set_inherit(env, h, inherit);
            log::info!(
                "CreateThread: context {id} ({name}) at {start:#010x}({param:#x}) flags={flags:#x}"
            );
            out_u32(env.cpu, tid_out, tid);
            Outcome::Return(h)
        }
        Err(error) => api_error(
            env,
            processes::Error::new(err::NOT_ENOUGH_MEMORY, format!("CreateThread: {error}")),
            0,
        ),
    }
}

fn suspend_thread(env: &mut Env<'_>) -> Outcome {
    change_suspend_count(env, true)
}

fn resume_thread(env: &mut Env<'_>) -> Outcome {
    change_suspend_count(env, false)
}

fn change_suspend_count(env: &mut Env<'_>, suspend: bool) -> Outcome {
    change_suspend_front(env, env.arg(0), suspend, false, 0)
}

pub(crate) fn native_resume_thread(env: &mut Env<'_>, handle: u32, previous: u32) -> Outcome {
    change_suspend_front(env, handle, false, true, previous)
}

fn suspend_error(env: &mut Env<'_>, error: processes::Error, native: bool) -> Outcome {
    if !native {
        return api_error(env, error, u32::MAX);
    }
    Outcome::Return(match error.code {
        5 => 0xc0000022,
        6 => 0xc0000008,
        8 => 0xc0000017,
        50 => {
            adv_abi::warn_unavailable(env, "ntdll.dll!NtResumeThread", &error.message);
            0xc00000bb
        }
        _ => 0xc0000001,
    })
}
fn resume_result(env: &mut Env<'_>, previous: u32, count: u32) -> Outcome {
    if previous != 0
        && (adv_abi::guest::check_range(env.cpu, previous, 4, Prot::WRITE).is_err()
            || env.cpu.write_u32(previous, count).is_err())
    {
        return Outcome::Return(0xc0000005);
    }
    Outcome::Return(0)
}
fn change_suspend_front(
    env: &mut Env<'_>,
    handle: u32,
    suspend: bool,
    native: bool,
    previous: u32,
) -> Outcome {
    if native
        && previous != 0
        && adv_abi::guest::check_range(env.cpu, previous, 4, Prot::WRITE).is_err()
    {
        return Outcome::Return(0xc0000005);
    }
    let identity = match thread_target(env, handle, 2) {
        Ok(identity) => identity,
        Err(error) => return suspend_error(env, error, native),
    };
    if native && identity.completion.exit_code().is_some() {
        // The completed thread remains a valid handle; there is no execution to
        // reschedule. Native x86 publishes a zero previous count in this state.
        return resume_result(env, previous, 0);
    }
    let completion = identity.completion.clone();
    let pid = identity.process.pid;
    let id = identity.context;
    processes::visit(
        env,
        pid,
        None,
        move |target| {
            let context = target.sys.contexts.get(id as usize).ok_or_else(|| {
                processes::Error::new(err::INVALID_HANDLE, "thread context no longer exists")
            })?;
            if context.is_done() {
                if native {
                    return Ok(0);
                }
                return Err(processes::Error::new(
                    err::INVALID_HANDLE,
                    "thread has exited",
                ));
            }
            if suspend && context.suspend_count == 127 {
                return Err(processes::Error::new(156, "maximum suspend count reached"));
            }
            let previous = if suspend {
                target.sys.suspend_context(id)
            } else {
                target.sys.resume_context(id)
            };
            previous.ok_or_else(|| {
                processes::Error::new(err::INVALID_HANDLE, "thread cannot be suspended or resumed")
            })
        },
        move |env, result| {
            if native {
                return match result {
                    Ok(count) => resume_result(env, previous, count),
                    Err(error) if error.code == 6 && completion.exit_code().is_some() => {
                        resume_result(env, previous, 0)
                    }
                    Err(error) => suspend_error(env, error, true),
                };
            }
            if suspend && pid == env.sys.pid && id == env.context {
                match result {
                    Ok(previous) => Outcome::block_then_return(Wait::Yield, previous),
                    Err(error) => api_error(env, error, u32::MAX),
                }
            } else {
                api_result(env, result, u32::MAX)
            }
        },
    )
}

fn terminate_thread(env: &mut Env<'_>) -> Outcome {
    let (h, code) = (env.arg(0), env.arg(1));
    let identity = match thread_target(env, h, 1) {
        Ok(identity) => identity,
        Err(error) => return api_error(env, error, 0),
    };
    if identity.completion.exit_code().is_some() {
        return Outcome::Return(1);
    }
    if identity.process.pid == env.sys.pid {
        if identity.context == env.context {
            return Outcome::ExitThread(code);
        }
        env.sys.terminate_context(identity.context, code);
        return Outcome::Return(1);
    }
    processes::visit(
        env,
        identity.process.pid,
        None,
        move |target| {
            target.sys.terminate_context_stopped(identity.context, code);
            Ok(1)
        },
        |env, result| api_result(env, result, 0),
    )
}

fn get_exit_code_thread(env: &mut Env<'_>) -> Outcome {
    let (h, out) = (env.arg(0), env.arg(1));
    let identity = match thread_target(env, h, 0x40) {
        Ok(identity) => identity,
        Err(error) => return api_error(env, error, 0),
    };
    write_exit_code(env, out, identity.completion.exit_code().unwrap_or(259))
}

fn get_thread_context(env: &mut Env<'_>) -> Outcome {
    thread_context_api(env, false)
}
fn set_thread_context(env: &mut Env<'_>) -> Outcome {
    thread_context_api(env, true)
}

fn thread_context_api(env: &mut Env<'_>, set: bool) -> Outcome {
    thread_context_front(env, env.arg(0), env.arg(1), set, ContextFront::Win32)
}

pub(crate) fn native_thread_context(
    env: &mut Env<'_>,
    handle: u32,
    pointer: u32,
    set: bool,
) -> Outcome {
    thread_context_front(
        env,
        handle,
        pointer,
        set,
        ContextFront::Native(if set {
            "ntdll.dll!NtSetContextThread"
        } else {
            "ntdll.dll!NtGetContextThread"
        }),
    )
}

#[derive(Clone, Copy)]
enum ContextFront {
    Win32,
    Native(&'static str),
}
impl ContextFront {
    fn api(self) -> &'static str {
        match self {
            Self::Native(api) => api,
            Self::Win32 => "kernel32.dll!GetThreadContext",
        }
    }
    fn success(self) -> Outcome {
        Outcome::Return(if matches!(self, Self::Native(_)) {
            0
        } else {
            1
        })
    }
    fn error(self, env: &mut Env<'_>, error: processes::Error) -> Outcome {
        if matches!(self, Self::Win32) {
            return api_error(env, error, 0);
        }
        let status = match error.code {
            5 => 0xc0000022,
            6 => 0xc0000008,
            8 => 0xc0000017,
            87 => 0xc000000d,
            998 => 0xc0000005,
            50 => {
                adv_abi::warn_unavailable(env, self.api(), &error.message);
                0xc00000bb
            }
            _ => 0xc0000001,
        };
        Outcome::Return(status)
    }
}

fn thread_context_front(
    env: &mut Env<'_>,
    handle: u32,
    pointer: u32,
    set: bool,
    front: ContextFront,
) -> Outcome {
    let identity = match thread_target(env, handle, if set { 0x10 } else { 8 }) {
        Ok(identity) => identity,
        Err(error) => return front.error(env, error),
    };
    if identity.process.pid == env.sys.pid
        && env
            .sys
            .contexts
            .get(identity.context as usize)
            .is_none_or(|context| context.suspend_count == 0 || context.is_done())
    {
        return front.error(
            env,
            processes::Error::new(
                err::INVALID_PARAMETER,
                "thread CONTEXT requires a suspended thread",
            ),
        );
    }
    let protection = if set { Prot::READ } else { Prot::RW };
    if pointer == 0
        || adv_abi::guest::check_range(env.cpu, pointer, thread_context::SIZE as u32, protection)
            .is_err()
    {
        return front.error(
            env,
            processes::Error::new(998, "inaccessible thread CONTEXT"),
        );
    }
    let mut bytes = [0; thread_context::SIZE];
    if env.cpu.read(pointer, &mut bytes).is_err() {
        return front.error(env, processes::Error::new(998, "unreadable thread CONTEXT"));
    }
    if let Err(error) = thread_context::flags(&bytes) {
        if matches!(front, ContextFront::Native(_)) {
            adv_abi::warn_unavailable(env, front.api(), &error.message);
            return Outcome::Return(0xc00000bb);
        }
        return front.error(env, error);
    }
    processes::visit(
        env,
        identity.process.pid,
        Some(identity.context),
        move |target| {
            if target.ctx().suspend_count == 0 {
                return Err(processes::Error::new(
                    err::INVALID_PARAMETER,
                    "thread CONTEXT requires a suspended thread",
                ));
            }
            let mut state = target
                .cpu
                .capture_architecture()
                .map_err(|error| processes::Error::new(50, error.to_string()))?;
            if set {
                thread_context::restore(&bytes, &mut state)?;
                target.cpu.restore_architecture(&state).map_err(|error| {
                    processes::Error::new(err::INVALID_PARAMETER, error.to_string())
                })?;
                Ok(None)
            } else {
                thread_context::capture(&state, bytes).map(Some)
            }
        },
        move |env, result| match result {
            Ok(Some(bytes)) => {
                if adv_abi::guest::check_range(
                    env.cpu,
                    pointer,
                    thread_context::SIZE as u32,
                    Prot::WRITE,
                )
                .is_err()
                    || env.cpu.write(pointer, &bytes).is_err()
                {
                    return front
                        .error(env, processes::Error::new(998, "unwritable thread CONTEXT"));
                }
                front.success()
            }
            Ok(None) => front.success(),
            Err(error) => front.error(env, error),
        },
    )
}

/// MEMORYSTATUS / MEMORYSTATUSEX with 2 GiB physical, 2 GiB virtual.
fn global_memory_status(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let v = [
        32u32,
        30,
        0x8000_0000,
        0x6000_0000,
        0xffff_ffff,
        0xc000_0000,
        0x7ffe_0000,
        0x7000_0000,
    ];
    for (i, x) in v.iter().enumerate() {
        let _ = env.cpu.write_u32(p + 4 * i as u32, *x);
    }
    Outcome::Return(0)
}

fn global_memory_status_ex(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    zero(env.cpu, p + 4, 60);
    let _ = env.cpu.write_u32(p + 4, 30);
    for (i, v) in [
        0x8000_0000u64,
        0x6000_0000,
        0xffff_ffff,
        0xc000_0000,
        0x7ffe_0000,
        0x7000_0000,
    ]
    .iter()
    .enumerate()
    {
        let _ = env.cpu.write_u32(p + 8 + 8 * i as u32, *v as u32);
        let _ = env.cpu.write_u32(p + 12 + 8 * i as u32, (*v >> 32) as u32);
    }
    Outcome::Return(1)
}

fn get_computer_name_a(env: &mut Env<'_>) -> Outcome {
    let (buf, size_p) = (env.arg(0), env.arg(1));
    let cap = env.cpu.read_u32(size_p).unwrap_or(0);
    match put_cstr(env.cpu, buf, cap, b"PAL-HLE") {
        Some(n) => {
            out_u32(env.cpu, size_p, n);
            Outcome::Return(1)
        }
        None => {
            out_u32(env.cpu, size_p, 8);
            set_last_error(env, 111); // ERROR_BUFFER_OVERFLOW
            Outcome::Return(0)
        }
    }
}

pub(super) fn register(reg: &mut Registry) {
    reg.register(get_console_window_ABI);
    // A 32-bit Windows: not under WOW64.
    stdcall(reg, K32, "IsWow64Process", 2, |e| {
        let p = e.arg(1);
        out_u32(e.cpu, p, 0);
        Outcome::Return(1)
    });
    // There is no console: handlers never fire and reads fail.
    stdcall(reg, K32, "SetConsoleCtrlHandler", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "ReadConsoleW", 5, |e| {
        let p = e.arg(3);
        out_u32(e.cpu, p, 0);
        set_last_error(e, err::INVALID_HANDLE);
        Outcome::Return(0)
    });
    stdcall(reg, K32, "GetCurrentProcessId", 0, |e| {
        Outcome::Return(e.sys.pid)
    });
    stdcall(reg, K32, "GetCurrentThreadId", 0, |e| {
        Outcome::Return(e.ctx().tid)
    });
    stdcall(reg, K32, "GetCurrentProcess", 0, |_| {
        Outcome::Return(u32::MAX)
    });
    stdcall(reg, K32, "OpenProcess", 3, open_process);
    stdcall(reg, K32, "GetCurrentThread", 0, |_| {
        Outcome::Return(u32::MAX - 1)
    });
    stdcall(reg, K32, "ExitProcess", 1, exit_process);
    stdcall(reg, K32, "TerminateProcess", 2, terminate_process);
    stdcall(reg, K32, "ExitThread", 1, |e| {
        super::module::exit_thread(e, e.arg(0))
    });
    stdcall(reg, K32, "TerminateThread", 2, terminate_thread);
    stdcall(reg, K32, "CreateThread", 6, create_thread);
    stdcall(reg, K32, "SuspendThread", 1, suspend_thread);
    stdcall(reg, K32, "ResumeThread", 1, resume_thread);
    stdcall(reg, K32, "GetExitCodeThread", 2, get_exit_code_thread);
    stdcall(reg, K32, "GetExitCodeProcess", 2, get_exit_code_process);
    stdcall(reg, K32, "GetThreadContext", 2, get_thread_context);
    stdcall(reg, K32, "SetThreadContext", 2, set_thread_context);
    stdcall(
        reg,
        K32,
        "FlushInstructionCache",
        3,
        flush_instruction_cache,
    );
    stdcall(reg, K32, "SetThreadPriority", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetThreadPriority", 1, |_| Outcome::Return(0));
    stdcall(reg, K32, "SetPriorityClass", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetPriorityClass", 1, |_| Outcome::Return(0x20));
    processor::register(reg);
    stdcall(reg, K32, "GetCommandLineA", 0, command_line_a);
    stdcall(reg, K32, "GetCommandLineW", 0, command_line_w);
    stdcall(reg, K32, "GetStartupInfoA", 1, get_startup_info);
    stdcall(reg, K32, "GetStartupInfoW", 1, get_startup_info);
    stdcall(reg, K32, "GetStdHandle", 1, get_std_handle);
    stdcall(reg, K32, "SetStdHandle", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetHandleCount", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "GetConsoleMode", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "SetConsoleMode", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetConsoleCP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "GetConsoleOutputCP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "IsDebuggerPresent", 0, |_| Outcome::Return(0));
    stdcall(reg, K32, "CheckRemoteDebuggerPresent", 2, |e| {
        if let Err(error) = super::child_process::process_target(e, e.arg(0), 0x1000) {
            return api_error(e, error, 0);
        }
        let out = e.arg(1);
        if out == 0 {
            set_last_error(e, err::INVALID_PARAMETER);
            return Outcome::Return(0);
        }
        if adv_abi::guest::check_range(e.cpu, out, 4, adv_cpu::backend::Prot::WRITE).is_err()
            || e.cpu.write_u32(out, 0).is_err()
        {
            set_last_error(e, 998); // ERROR_NOACCESS
            return Outcome::Return(0);
        }
        Outcome::Return(1)
    });
    stdcall(
        reg,
        K32,
        "IsProcessorFeaturePresent",
        1,
        is_processor_feature_present,
    );
    stdcall(
        reg,
        K32,
        "SetUnhandledExceptionFilter",
        1,
        set_unhandled_exception_filter,
    );
    stdcall(
        reg,
        K32,
        "UnhandledExceptionFilter",
        1,
        unhandled_exception_filter,
    );
    stdcall(reg, K32, "RtlUnwind", 4, rtl_unwind);
    stdcall(reg, K32, "SetErrorMode", 1, |_| Outcome::Return(0));
    stdcall(reg, K32, "OutputDebugStringA", 1, output_debug_string_a);
    stdcall(reg, K32, "OutputDebugStringW", 1, output_debug_string_w);
    stdcall(reg, K32, "EncodePointer", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "DecodePointer", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "GetLastError", 0, get_last_error);
    stdcall(reg, K32, "SetLastError", 1, set_last_error_);
    stdcall(reg, K32, "GlobalMemoryStatus", 1, global_memory_status);
    stdcall(reg, K32, "GlobalMemoryStatusEx", 1, global_memory_status_ex);
    stdcall(reg, K32, "GetComputerNameA", 2, get_computer_name_a);
}
