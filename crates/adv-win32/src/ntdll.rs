//! NT compatibility entry points over the existing virtual process, VFS and VM.
mod allocation;
mod directory;
mod loader;
mod loader_lock;
mod memory;
mod path;
mod procedure;
mod registry;
mod strings;
mod thread;

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

use crate::stdcall;
mod file;
mod heap;
mod open;
mod section;

const NTDLL: &str = "ntdll.dll";
const STATUS_SUCCESS: u32 = 0;

pub(super) fn current_process(env: &Env<'_>, handle: u32) -> bool {
    handle == u32::MAX
        || matches!(env.sys.objects.get(handle), Some(adv_machine::Object::Process { identity, .. }) if identity.pid == env.sys.pid)
        || env
            .sys
            .objects
            .host::<crate::kernel32::ProcessHandle>(handle)
            .is_some()
}

#[derive(Default)]
struct ThreadDebugState {
    hidden: std::collections::BTreeSet<adv_machine::CtxId>,
}

fn query_system_information(env: &mut Env<'_>) -> Outcome {
    let (class, out, size, returned) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if class != 35 {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtQuerySystemInformation",
            "system information class is not modeled",
        );
        return Outcome::Return(0xc000_0002);
    }
    if returned != 0 {
        if adv_abi::guest::check_range(env.cpu, returned, 4, adv_cpu::backend::Prot::WRITE).is_err()
            || env.cpu.write_u32(returned, 2).is_err()
        {
            return Outcome::Return(0xc000_0005);
        }
    }
    if size < 2 {
        return Outcome::Return(0xc000_0004);
    }
    // SYSTEM_KERNEL_DEBUGGER_INFORMATION: enabled=false, not-present=true.
    if adv_abi::guest::check_range(env.cpu, out, 2, adv_cpu::backend::Prot::WRITE).is_err()
        || env.cpu.write(out, &[0, 1]).is_err()
    {
        return Outcome::Return(0xc000_0005);
    }
    Outcome::Return(STATUS_SUCCESS)
}

fn set_information_thread(env: &mut Env<'_>) -> Outcome {
    if env.arg(1) != 17 {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtSetInformationThread",
            "thread information class is not modeled",
        );
        return Outcome::Return(0xc000_0002);
    }
    if env.arg(3) != 0 {
        return Outcome::Return(0xc000_0004);
    }
    let target = match env.sys.objects.get(env.arg(0)) {
        Some(adv_machine::Object::Thread { ctx }) => *ctx,
        _ if env.arg(0) == u32::MAX - 1 => env.context,
        _ => return Outcome::Return(0xc000_0008),
    };
    env.ext
        .get_or_default::<ThreadDebugState>()
        .hidden
        .insert(target);
    Outcome::Return(STATUS_SUCCESS)
}

/// The virtual process has no guest debugger. Keep the debug port, debug
/// object, and debug flags consistent with the Win32 debugger queries.
fn query_information_process(env: &mut Env<'_>) -> Outcome {
    let (process, class, out, size, returned) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if !matches!(class, 7 | 30 | 31) {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtQueryInformationProcess",
            &format!("process information class {class} is not modeled"),
        );
        return Outcome::Return(0xc000_0002); // STATUS_NOT_IMPLEMENTED
    }
    if size != 4 {
        return Outcome::Return(0xc000_0004); // STATUS_INFO_LENGTH_MISMATCH
    }
    if !current_process(env, process) {
        return Outcome::Return(0xc000_0008); // STATUS_INVALID_HANDLE
    }
    if returned != 0
        && adv_abi::guest::check_range(env.cpu, returned, 4, adv_cpu::backend::Prot::WRITE).is_err()
    {
        return Outcome::Return(0xc000_0005);
    }
    if class != 30 {
        if adv_abi::guest::check_range(env.cpu, out, 4, adv_cpu::backend::Prot::WRITE).is_err()
            || env.cpu.write_u32(out, u32::from(class == 31)).is_err()
        {
            return Outcome::Return(0xc000_0005);
        }
    }
    if returned != 0 && env.cpu.write_u32(returned, 4).is_err() {
        return Outcome::Return(0xc000_0005);
    }
    // No debug object: native leaves the handle buffer untouched on this failure.
    Outcome::Return(if class == 30 {
        0xc000_0353
    } else {
        STATUS_SUCCESS
    })
}

/// RtlFormatCurrentUserKeyPath(PUNICODE_STRING CurrentUserKeyPath) -> NTSTATUS
fn rtl_format_current_user_key_path(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(0);
    if out == 0 {
        return Outcome::Return(0xc000_000d); // STATUS_INVALID_PARAMETER
    }
    let path = format!(
        r"\REGISTRY\USER\{}",
        adv_os::identity::current_user_sid_string()
    );
    let units: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let byte_len = ((units.len() - 1) * 2) as u16;
    let max_byte_len = (units.len() * 2) as u16;

    let heap = env.ext.get::<Heaps>().process_heap;
    let alloc = match env.ext.get_mut::<Heaps>().alloc(
        env.cpu,
        &mut env.sys.vm,
        heap,
        max_byte_len as u32,
        false,
    ) {
        Some(addr) => addr,
        None => return Outcome::Return(0xc000_0017), // STATUS_NO_MEMORY
    };

    crate::util::write_units(env.cpu, alloc, &units);
    let _ = env.cpu.write(out, &byte_len.to_le_bytes());
    let _ = env.cpu.write(out + 2, &max_byte_len.to_le_bytes());
    let _ = env.cpu.write_u32(out + 4, alloc);
    Outcome::Return(STATUS_SUCCESS)
}

/// RtlFreeUnicodeString(PUNICODE_STRING UnicodeString) -> VOID
fn rtl_free_unicode_string(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    if p != 0 {
        if let Ok(buf) = env.cpu.read_u32(p + 4) {
            if buf != 0 {
                let heap = env.ext.get::<Heaps>().process_heap;
                let _ = env
                    .ext
                    .get_mut::<Heaps>()
                    .free(env.cpu, &mut env.sys.vm, heap, buf);
                let _ = env.cpu.write_u32(p + 4, 0);
            }
        }
        let _ = env.cpu.write(p, &0u16.to_le_bytes());
        let _ = env.cpu.write(p + 2, &0u16.to_le_bytes());
    }
    Outcome::Return(0)
}

/// RtlInitUnicodeString(PUNICODE_STRING DestinationString, PCWSTR SourceString) -> VOID
fn rtl_init_unicode_string(env: &mut Env<'_>) -> Outcome {
    let (dst, src) = (env.arg(0), env.arg(1));
    if dst != 0 {
        if src == 0 {
            let _ = env.cpu.write(dst, &0u16.to_le_bytes());
            let _ = env.cpu.write(dst + 2, &0u16.to_le_bytes());
            let _ = env.cpu.write_u32(dst + 4, 0);
        } else {
            let s = crate::util::wstr(env.cpu, src);
            let byte_len = (s.len() * 2) as u16;
            let max_byte_len = ((s.len() + 1) * 2) as u16;
            let _ = env.cpu.write(dst, &byte_len.to_le_bytes());
            let _ = env.cpu.write(dst + 2, &max_byte_len.to_le_bytes());
            let _ = env.cpu.write_u32(dst + 4, src);
        }
    }
    Outcome::Return(0)
}

pub fn register(reg: &mut Registry) {
    heap::register(reg);
    path::register(reg);
    directory::register(reg);
    loader_lock::register(reg);
    loader::register(reg);
    strings::register(reg);
    procedure::register(reg);
    thread::register(reg);
    allocation::register(reg);
    memory::register(reg);
    registry::register(reg);
    stdcall(
        reg,
        NTDLL,
        "NtQueryInformationProcess",
        5,
        query_information_process,
    );
    stdcall(
        reg,
        NTDLL,
        "ZwQueryInformationProcess",
        5,
        query_information_process,
    );
    stdcall(
        reg,
        NTDLL,
        "NtSetInformationThread",
        4,
        set_information_thread,
    );
    stdcall(
        reg,
        NTDLL,
        "ZwSetInformationThread",
        4,
        set_information_thread,
    );
    stdcall(
        reg,
        NTDLL,
        "NtQuerySystemInformation",
        4,
        query_system_information,
    );
    stdcall(
        reg,
        NTDLL,
        "ZwQuerySystemInformation",
        4,
        query_system_information,
    );
    file::register(reg);
    open::register(reg);
    section::register(reg);
    stdcall(
        reg,
        NTDLL,
        "RtlFormatCurrentUserKeyPath",
        1,
        rtl_format_current_user_key_path,
    );
    stdcall(
        reg,
        NTDLL,
        "RtlFreeUnicodeString",
        1,
        rtl_free_unicode_string,
    );
    stdcall(
        reg,
        NTDLL,
        "RtlInitUnicodeString",
        2,
        rtl_init_unicode_string,
    );
}
