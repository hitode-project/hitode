//! Process, thread, environment, version, and error-reporting functions.

use adv_machine::{Env, Outcome, Registry};
use pal_cpu::{GuestExt, Reg};

use super::K32;
use crate::util::{cstr, err, out_u32, put_cstr, set_last_error, wstr, zero};
use crate::{STD_ERROR, STD_INPUT, STD_OUTPUT, Win32, stdcall};

fn exit_process(env: &mut Env<'_>) -> Outcome {
    Outcome::ExitProcess(env.arg(0))
}

fn terminate_process(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) == u32::MAX {
        return Outcome::ExitProcess(env.arg(1));
    }
    Outcome::Fail(format!(
        "TerminateProcess({:#x}) of another process",
        env.arg(0)
    ))
}

fn command_line_a(env: &mut Env<'_>) -> Outcome {
    let w = env.ext.get_mut::<Win32>();
    let line = format!("\"{}\"", w.exe_path());
    let mut bytes = w.codec.encode(&line);
    bytes.push(0);
    Outcome::Return(w.static_bytes("cmdline_a", env.cpu, env.sys, &bytes))
}

fn command_line_w(env: &mut Env<'_>) -> Outcome {
    let w = env.ext.get_mut::<Win32>();
    let line = format!("\"{}\"", w.exe_path());
    let bytes: Vec<u8> = line
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    Outcome::Return(w.static_bytes("cmdline_w", env.cpu, env.sys, &bytes))
}

pub(super) fn environment(w: &Win32) -> Vec<String> {
    vec![
        "=C:=C:\\Game".into(),
        "ALLUSERSPROFILE=C:\\ProgramData".into(),
        format!("APPDATA=C:\\Users\\{}\\AppData\\Roaming", w.cfg.user_name),
        "COMPUTERNAME=PAL-HLE".into(),
        format!(
            "LOCALAPPDATA=C:\\Users\\{}\\AppData\\Local",
            w.cfg.user_name
        ),
        "OS=Windows_NT".into(),
        "PATH=C:\\Windows\\system32;C:\\Windows".into(),
        "SystemDrive=C:".into(),
        "SystemRoot=C:\\Windows".into(),
        format!("TEMP=C:\\Users\\{}\\AppData\\Local\\Temp", w.cfg.user_name),
        format!("TMP=C:\\Users\\{}\\AppData\\Local\\Temp", w.cfg.user_name),
        format!("USERNAME={}", w.cfg.user_name),
        format!("USERPROFILE=C:\\Users\\{}", w.cfg.user_name),
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

fn raise_exception(env: &mut Env<'_>) -> Outcome {
    let (code, flags, n, args) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let a: Vec<String> = (0..n.min(15))
        .map(|i| format!("{:#x}", env.cpu.read_u32(args + 4 * i).unwrap_or(0)))
        .collect();
    Outcome::Fail(format!(
        "RaiseException(code {code:#010x}, flags {flags:#x}, args [{}]) from {:#010x}: SEH dispatch not implemented",
        a.join(", "),
        env.caller()
    ))
}

const EXCEPTION_UNWINDING: u32 = 2;
const EXCEPTION_EXIT_UNWIND: u32 = 4;
const STATUS_UNWIND: u32 = 0xc000_0027;
/// Guest scratch for an unwind: EXCEPTION_RECORD, CONTEXT, dispatcher context.
const RECORD: u32 = 0;
const CONTEXT: u32 = 0x50;
const DISPATCHER: u32 = 0x320;
const SCRATCH: u32 = 0x330;

struct Unwind {
    target: u32,
    value: u32,
    record: u32,
    scratch: u32,
    /// EBX, ESI, EDI, EBP at entry, which the resumed context restores.
    saved: [u32; 4],
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
    let w = env.ext.get_mut::<Win32>();
    let Some(scratch) = w.heaps.alloc(env.cpu, &mut env.sys.vm, 0, SCRATCH, true) else {
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
    unwind_step(
        env,
        Unwind {
            target,
            value,
            record,
            scratch,
            saved,
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
        let w = env.ext.get_mut::<Win32>();
        w.heaps.free(env.cpu, &mut env.sys.vm, 0, u.scratch);
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
    let (stack, start, param, flags, tid_out) =
        (env.arg(1), env.arg(2), env.arg(3), env.arg(4), env.arg(5));
    if flags & 4 != 0 {
        return Outcome::Fail("CreateThread(CREATE_SUSPENDED) not implemented".into());
    }
    let name = format!("T{}", env.sys.contexts.len());
    match env.sys.spawn(
        env.cpu,
        &name,
        start,
        &[param],
        if stack == 0 { 0x10_0000 } else { stack },
    ) {
        Ok(id) => {
            let c = env.sys.context(id);
            let (tid, h) = (c.tid, c.thread_handle);
            log::info!("CreateThread: context {id} ({name}) at {start:#010x}({param:#x})");
            out_u32(env.cpu, tid_out, tid);
            Outcome::Return(h)
        }
        Err(e) => Outcome::Fail(format!("CreateThread: {e}")),
    }
}

fn get_exit_code_thread(env: &mut Env<'_>) -> Outcome {
    let (h, out) = (env.arg(0), env.arg(1));
    let code = match env.sys.objects.get(h) {
        Some(adv_machine::Object::Thread { ctx }) => {
            env.sys.context(*ctx).exit_code().unwrap_or(259)
        } // STILL_ACTIVE
        _ => {
            set_last_error(env, err::INVALID_HANDLE);
            return Outcome::Return(0);
        }
    };
    out_u32(env.cpu, out, code);
    Outcome::Return(1)
}

/// SYSTEM_INFO for one x86 CPU with 4 KiB pages and 64 KiB granularity.
fn get_system_info(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    zero(env.cpu, p, 36);
    for (off, v) in [
        (4, 0x1000),
        (8, 0x1_0000),
        (12, 0x7ffe_ffff),
        (16, 1),
        (20, 1),
        (24, 586),
        (28, 0x1_0000),
    ] {
        let _ = env.cpu.write_u32(p + off, v);
    }
    let _ = env.cpu.write(p + 32, &[6, 0, 0x2a, 0x3a]); // wProcessorLevel 6, revision
    Outcome::Return(0)
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
    stdcall(reg, K32, "GetCurrentThread", 0, |_| {
        Outcome::Return(u32::MAX - 1)
    });
    stdcall(reg, K32, "ExitProcess", 1, exit_process);
    stdcall(reg, K32, "TerminateProcess", 2, terminate_process);
    stdcall(reg, K32, "ExitThread", 1, |e| Outcome::ExitThread(e.arg(0)));
    stdcall(reg, K32, "CreateThread", 6, create_thread);
    stdcall(reg, K32, "GetExitCodeThread", 2, get_exit_code_thread);
    stdcall(reg, K32, "SetThreadPriority", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetThreadPriority", 1, |_| Outcome::Return(0));
    stdcall(reg, K32, "SetPriorityClass", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "GetPriorityClass", 1, |_| Outcome::Return(0x20));
    stdcall(reg, K32, "SetThreadAffinityMask", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetProcessAffinityMask", 2, |_| {
        Outcome::Return(1)
    });
    stdcall(reg, K32, "GetCommandLineA", 0, command_line_a);
    stdcall(reg, K32, "GetCommandLineW", 0, command_line_w);
    stdcall(reg, K32, "GetStartupInfoA", 1, get_startup_info);
    stdcall(reg, K32, "GetStartupInfoW", 1, get_startup_info);
    stdcall(reg, K32, "GetStdHandle", 1, get_std_handle);
    stdcall(reg, K32, "SetStdHandle", 2, |_| Outcome::Return(1));
    stdcall(reg, K32, "SetHandleCount", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "GetConsoleMode", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "GetConsoleCP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "GetConsoleOutputCP", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().codec.acp)
    });
    stdcall(reg, K32, "IsDebuggerPresent", 0, |_| Outcome::Return(0));
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
    stdcall(reg, K32, "UnhandledExceptionFilter", 1, |_| {
        Outcome::Return(0)
    });
    stdcall(reg, K32, "RaiseException", 4, raise_exception);
    stdcall(reg, K32, "RtlUnwind", 4, rtl_unwind);
    stdcall(reg, K32, "SetErrorMode", 1, |_| Outcome::Return(0));
    stdcall(reg, K32, "OutputDebugStringA", 1, output_debug_string_a);
    stdcall(reg, K32, "OutputDebugStringW", 1, output_debug_string_w);
    stdcall(reg, K32, "EncodePointer", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "DecodePointer", 1, |e| Outcome::Return(e.arg(0)));
    stdcall(reg, K32, "GetLastError", 0, get_last_error);
    stdcall(reg, K32, "SetLastError", 1, set_last_error_);
    stdcall(reg, K32, "GetSystemInfo", 1, get_system_info);
    stdcall(reg, K32, "GlobalMemoryStatus", 1, global_memory_status);
    stdcall(reg, K32, "GlobalMemoryStatusEx", 1, global_memory_status_ex);
    stdcall(reg, K32, "GetComputerNameA", 2, get_computer_name_a);
}
