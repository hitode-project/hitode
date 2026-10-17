//! NT compatibility entry points over the existing virtual process, VFS and VM.
mod allocation;
mod directory;
mod loader;
mod loader_lock;
mod memory;
pub(crate) mod path;
mod peb_lock;
pub(crate) use peb_lock::install as install_peb_lock;
mod procedure;
mod registry;
mod shutdown;
mod strings;
mod termination;
mod thread;
mod thread_open;
mod virtual_query;

use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_os::Heaps;

use crate::stdcall;
pub(crate) mod file;
mod heap;
pub(crate) use heap::{allocate_shared as heap_allocate, reallocate_shared as heap_reallocate};
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
            &format!("system information class {class} is not modeled"),
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
/// ProcessBasicInformation (0) reports the process PEB address, PID, and exit code.
fn query_information_process(env: &mut Env<'_>) -> Outcome {
    let (process, class, out, size, returned) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if !matches!(class, 0 | 7 | 30 | 31) {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtQueryInformationProcess",
            &format!("process information class {class} is not modeled"),
        );
        return Outcome::Return(0xc000_0002); // STATUS_NOT_IMPLEMENTED
    }
    if class == 0 {
        if size < 24 {
            return Outcome::Return(0xc000_0004); // STATUS_INFO_LENGTH_MISMATCH
        }
        let target = if current_process(env, process) {
            env.sys.process_identity.clone()
        } else {
            match crate::kernel32::child_process::process_target(
                env, process, 0x0400, // PROCESS_QUERY_INFORMATION
            ) {
                Ok(target) => target,
                Err(error) => {
                    let status = match error.code {
                        5 => 0xc000_0022, // STATUS_ACCESS_DENIED
                        6 => 0xc000_0008, // STATUS_INVALID_HANDLE
                        _ => 0xc000_0008,
                    };
                    return Outcome::Return(status);
                }
            }
        };
        let pid = target.pid;
        let parent_pid = env
            .ext
            .try_get::<adv_os::processes::Services>()
            .map(|services| {
                services
                    .live_processes()
                    .iter()
                    .find(|(p, _)| *p == pid)
                    .map(|(_, parent)| *parent)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let exit_status = target.completion.exit_code().map_or(0x0000_0103u32, |c| c); // STATUS_PENDING or exit code
        if returned != 0 {
            if adv_abi::guest::check_range(env.cpu, returned, 4, adv_cpu::backend::Prot::WRITE)
                .is_err()
                || env.cpu.write_u32(returned, 24).is_err()
            {
                return Outcome::Return(0xc000_0005);
            }
        }
        if adv_abi::guest::check_range(env.cpu, out, 24, adv_cpu::backend::Prot::WRITE).is_err() {
            return Outcome::Return(0xc000_0005);
        }
        let mut pbi = [0u8; 24];
        pbi[0..4].copy_from_slice(&exit_status.to_le_bytes());
        pbi[4..8].copy_from_slice(&adv_machine::layout::PEB_ADDR.to_le_bytes());
        pbi[8..12].copy_from_slice(&1u32.to_le_bytes()); // AffinityMask
        pbi[12..16].copy_from_slice(&8i32.to_le_bytes()); // BasePriority (NORMAL_PRIORITY_CLASS)
        pbi[16..20].copy_from_slice(&pid.to_le_bytes());
        pbi[20..24].copy_from_slice(&parent_pid.to_le_bytes());
        if env.cpu.write(out, &pbi).is_err() {
            return Outcome::Return(0xc000_0005);
        }
        return Outcome::Return(STATUS_SUCCESS);
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
    peb_lock::register(reg);
    loader::register(reg);
    shutdown::register(reg);
    strings::register(reg);
    procedure::register(reg);
    thread::register(reg);
    thread_open::register(reg);
    termination::register(reg);
    allocation::register(reg);
    memory::register(reg);
    virtual_query::register(reg);
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
    // Register before process installation: discovery and non-Win7 profiles
    // must see the same implementations, without depending on numeric veneers.
    for export in adv_abi::system_exports::exports(NTDLL) {
        let Some(suffix) = export.name.strip_prefix("Zw") else {
            continue;
        };
        let target = format!("Nt{suffix}");
        if reg.is_implemented(NTDLL, export.name) || !reg.is_implemented(NTDLL, &target) {
            continue;
        }
        let target = adv_abi::system_exports::lookup(NTDLL, &target)
            .expect("native service has a fixed public export")
            .name;
        reg.register_export_alias(NTDLL, export.name, target)
            .expect("user-mode Nt/Zw declarations share the complete ABI");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Prot, Reg},
    };
    use adv_machine::{
        Clock, Machine,
        layout::{PAGE, PARK_ADDR},
    };

    const DATA: u32 = 0x410000;

    fn test_machine() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let cpu = JitCpu::new(PARK_ADDR, false).unwrap();
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        machine.sys.set_process_identity(0x1234).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/nt-query-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "test.exe", &root),
        )
        .unwrap();
        machine.spawn("main", PARK_ADDR, &[], 65536).unwrap();
        machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
        machine
    }

    #[test]
    // spec: PERS-PROFILE-1
    fn query_information_process_basic_information() {
        let mut m = test_machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let stack = 0x5a0000;
        env.cpu.map(stack, PAGE, Prot::RW).unwrap();
        env.cpu.set_reg(Reg::Esp, stack);
        env.cpu.write_u32(stack + 4, u32::MAX).unwrap();
        env.cpu.write_u32(stack + 8, 0).unwrap();
        env.cpu.write_u32(stack + 12, DATA).unwrap();
        env.cpu.write_u32(stack + 16, 24).unwrap();
        env.cpu.write_u32(stack + 20, DATA + 32).unwrap();
        let Outcome::Return(res) = query_information_process(&mut env) else {
            panic!("expected return");
        };
        assert_eq!(res, 0);
        assert_eq!(env.cpu.read_u32(DATA + 32).unwrap(), 24);
        assert_eq!(env.cpu.read_u32(DATA).unwrap(), 0x0000_0103);
        assert_eq!(
            env.cpu.read_u32(DATA + 4).unwrap(),
            adv_machine::layout::PEB_ADDR
        );
        assert_eq!(env.cpu.read_u32(DATA + 8).unwrap(), 1);
        assert_eq!(env.cpu.read_u32(DATA + 12).unwrap() as i32, 8);
        assert_eq!(env.cpu.read_u32(DATA + 16).unwrap(), 0x1234);

        env.cpu.write_u32(stack + 16, 20).unwrap();
        let Outcome::Return(res) = query_information_process(&mut env) else {
            panic!("expected return");
        };
        assert_eq!(res, 0xc000_0004);

        env.cpu.write_u32(stack + 4, 12345).unwrap();
        env.cpu.write_u32(stack + 16, 24).unwrap();
        let Outcome::Return(res) = query_information_process(&mut env) else {
            panic!("expected return");
        };
        assert_eq!(res, 0xc000_0008);
    }
}
