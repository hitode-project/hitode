//! NT section calls share kernel32's backing objects, views and page coherence.
use crate::kernel32::file_mapping;
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

fn status(error: u32) -> u32 {
    match error {
        6 => 0xc0000008,
        8 => 0xc0000017,
        5 => 0xc0000022,
        487 => 0xc0000018,
        1132 => 0xc0000220,
        50 => 0xc0000002,
        _ => 0xc000000d,
    }
}
fn core(env: &mut Env<'_>, f: impl FnOnce(&mut Env<'_>) -> Outcome) -> Result<u32, u32> {
    let last = crate::util::last_error(env);
    let result = f(env);
    let error = crate::util::last_error(env);
    crate::util::set_last_error(env, last);
    match result {
        Outcome::Return(v) if v != 0 => Ok(v),
        _ => Err(status(error)),
    }
}
fn read64(env: &Env<'_>, pointer: u32) -> Result<u64, u32> {
    check_range(env.cpu, pointer, 8, Prot::READ).map_err(|_| 0xc0000005u32)?;
    Ok(
        u64::from(env.cpu.read_u32(pointer).map_err(|_| 0xc0000005u32)?)
            | (u64::from(env.cpu.read_u32(pointer + 4).map_err(|_| 0xc0000005u32)?) << 32),
    )
}
fn create(env: &mut Env<'_>) -> Outcome {
    let (out, access, attrs, maximum, protection, flags, file) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
        env.arg(6),
    );
    let result = (|| -> Result<(), u32> {
        check_range(env.cpu, out, 4, Prot::WRITE).map_err(|_| 0xc0000005u32)?;
        if attrs != 0 {
            check_range(env.cpu, attrs, 24, Prot::READ).map_err(|_| 0xc0000005u32)?;
            if env.cpu.read_u32(attrs).unwrap_or(0) != 24 {
                return Err(0xc000000d);
            }
            if env.cpu.read_u32(attrs + 4).unwrap_or(1) != 0
                || env.cpu.read_u32(attrs + 8).unwrap_or(1) != 0
            {
                adv_abi::warn_unavailable(
                    env,
                    "ntdll.dll!NtCreateSection",
                    "named NT sections are not modeled",
                );
                return Err(0xc0000002);
            }
        }
        if flags != 0x08000000 || !matches!(protection, 2 | 4 | 8) {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtCreateSection",
                "only committed data sections are modeled",
            );
            return Err(0xc0000002);
        }
        let maximum = if maximum == 0 {
            0
        } else {
            read64(env, maximum)?
        };
        if maximum > i64::MAX as u64 {
            return Err(0xc000000d);
        }
        let handle = core(env, |env| {
            file_mapping::create_section(
                env,
                if file == 0 { u32::MAX } else { file },
                0,
                protection,
                maximum,
                None,
            )
        })?;
        let rights = access & 6
            | if access & (0x10000000 | 0x80000000) != 0 {
                4
            } else {
                0
            }
            | if access & (0x10000000 | 0x40000000) != 0 {
                2
            } else {
                0
            };
        file_mapping::restrict_section_access(env, handle, rights);
        if env.cpu.write_u32(out, handle).is_err() {
            env.sys.objects.close(handle);
            return Err(0xc0000005);
        }
        Ok(())
    })();
    Outcome::Return(result.err().unwrap_or(0))
}
fn map(env: &mut Env<'_>) -> Outcome {
    let (section, process, base, zero, commit, offset, size, inherit, allocation, protection) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
        env.arg(6),
        env.arg(7),
        env.arg(8),
        env.arg(9),
    );
    let result = (|| -> Result<(), u32> {
        if !super::current_process(env, process) {
            return Err(0xc0000008);
        }
        for p in [base, size] {
            check_range(env.cpu, p, 4, Prot::RW).map_err(|_| 0xc0000005u32)?;
        }
        if zero != 0 || commit != 0 || allocation != 0 || !matches!(inherit, 1 | 2) {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtMapViewOfSection",
                "view allocation options are not modeled",
            );
            return Err(0xc0000002);
        }
        let access = match protection {
            2 => 4,
            4 => 2,
            8 => 1,
            _ => return Err(0xc0000045),
        };
        let offset = if offset == 0 {
            0
        } else {
            check_range(env.cpu, offset, 8, Prot::RW).map_err(|_| 0xc0000005u32)?;
            read64(env, offset)?
        };
        let requested = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        let address = core(env, |env| {
            file_mapping::map_section(env, section, access, offset, u64::from(count), requested)
        })?;
        let (_, length) = file_mapping::view_extent(env, address).ok_or(0xc000000du32)?;
        env.cpu
            .write_u32(base, address)
            .map_err(|_| 0xc0000005u32)?;
        env.cpu.write_u32(size, length).map_err(|_| 0xc0000005u32)?;
        Ok(())
    })();
    Outcome::Return(result.err().unwrap_or(0))
}
fn unmap(env: &mut Env<'_>) -> Outcome {
    if !super::current_process(env, env.arg(0)) {
        return Outcome::Return(0xc0000008);
    }
    let Some((base, _)) = file_mapping::view_extent(env, env.arg(1)) else {
        return Outcome::Return(0xc0000019);
    };
    Outcome::Return(
        core(env, |env| file_mapping::unmap_section(env, base))
            .err()
            .unwrap_or(0),
    )
}
fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    Outcome::Return(super::file::close(env, adv_abi::Handle(handle)) as u32)
}
fn vm_process(env: &mut Env<'_>, handle: u32, api: &str) -> Result<(), u32> {
    if let Some(adv_machine::Object::Process { identity, access }) = env.sys.objects.get(handle) {
        if access & 8 == 0 {
            return Err(0xc0000022); // STATUS_ACCESS_DENIED
        }
        if identity.pid != env.sys.pid {
            adv_abi::warn_unavailable(env, api, "remote NT virtual memory is not modeled");
            return Err(0xc0000002);
        }
    }
    if !super::current_process(env, handle) {
        return Err(0xc0000008);
    }
    Ok(())
}
#[win32(ntdll, name = "NtAllocateVirtualMemory", level = implement)]
fn allocate(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    zero: u32,
    size: GuestPtr<u32>,
    flags: u32,
    protection: u32,
) -> i32 {
    let (process, base, size) = (process.0, base.addr(), size.addr());
    let result = (|| -> Result<(), u32> {
        vm_process(env, process, "ntdll.dll!NtAllocateVirtualMemory")?;
        for pointer in [base, size] {
            check_range(env.cpu, pointer, 4, Prot::RW).map_err(|_| 0xc0000005u32)?;
        }
        if zero != 0 {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtAllocateVirtualMemory",
                "ZeroBits allocation constraints are not modeled",
            );
            return Err(0xc0000002);
        }
        // Private allocations cannot use copy-on-write protection. Valid
        // modifiers requiring additional semantics remain an explicit gap.
        if protection & 0x700 != 0 && matches!(protection & !0x700, 2 | 4 | 0x10 | 0x20 | 0x40) {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtAllocateVirtualMemory",
                "page protection modifiers are not modeled",
            );
            return Err(0xc0000002);
        }
        if !matches!(protection, 1 | 2 | 4 | 0x10 | 0x20 | 0x40) {
            return Err(0xc0000045); // STATUS_INVALID_PAGE_PROTECTION
        }
        let address = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        let (page, length) =
            crate::kernel32::memory_span(address, count).map_err(|error| status(error.code))?;
        let allocated = crate::kernel32::allocate_memory(env, address, count, flags, protection)
            .map_err(|error| status(error.code))?;
        // Explicit reservations include the bytes between the granule-rounded
        // base and the requested first page. Commits round only to pages.
        let length = if address == 0 {
            length
        } else {
            page + length - allocated
        };
        env.cpu
            .write_u32(base, allocated)
            .map_err(|_| 0xc0000005u32)?;
        env.cpu.write_u32(size, length).map_err(|_| 0xc0000005u32)?;
        Ok(())
    })();
    result.err().unwrap_or(0) as i32
}
#[win32(ntdll, name = "ZwAllocateVirtualMemory", level = implement)]
fn zw_allocate(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    zero: u32,
    size: GuestPtr<u32>,
    flags: u32,
    protection: u32,
) -> i32 {
    allocate(env, process, base, zero, size, flags, protection)
}
#[win32(ntdll, name = "NtFreeVirtualMemory", level = implement)]
fn free(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    size: GuestPtr<u32>,
    flags: u32,
) -> i32 {
    let (process, base, size) = (process.0, base.addr(), size.addr());
    let result = (|| -> Result<(), u32> {
        vm_process(env, process, "ntdll.dll!NtFreeVirtualMemory")?;
        for pointer in [base, size] {
            check_range(env.cpu, pointer, 4, Prot::RW).map_err(|_| 0xc0000005u32)?;
        }
        if !matches!(flags, 0x4000 | 0x8000) {
            return Err(0xc00000f2); // STATUS_INVALID_PARAMETER_4
        }
        let address = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        let (owner, extent) =
            crate::kernel32::allocation_extent(env, address).ok_or(0xc00000a0u32)?; // STATUS_MEMORY_NOT_ALLOCATED
        let (page, length) = if count == 0 {
            let page = address & !4095;
            if page != owner {
                return Err(0xc000009f); // STATUS_FREE_VM_NOT_AT_BASE
            }
            (page, extent)
        } else {
            crate::kernel32::memory_span(address, count).map_err(|error| status(error.code))?
        };
        if flags == 0x8000 && count != 0 {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtFreeVirtualMemory",
                "partial reservation release is not modeled",
            );
            return Err(0xc0000002);
        }
        if u64::from(page) + u64::from(length) > u64::from(owner) + u64::from(extent) {
            return Err(0xc000001a); // STATUS_UNABLE_TO_FREE_VM
        }
        crate::kernel32::free_memory(env, page, if count == 0 { 0 } else { length }, flags)
            .map_err(|error| status(error.code))?;
        env.cpu.write_u32(base, page).map_err(|_| 0xc0000005u32)?;
        env.cpu.write_u32(size, length).map_err(|_| 0xc0000005u32)?;
        Ok(())
    })();
    result.err().unwrap_or(0) as i32
}
#[win32(ntdll, name = "ZwFreeVirtualMemory", level = implement)]
fn zw_free(
    env: &mut Env<'_>,
    process: Handle,
    base: GuestPtr<u32>,
    size: GuestPtr<u32>,
    flags: u32,
) -> i32 {
    free(env, process, base, size, flags)
}
fn protect(env: &mut Env<'_>) -> Outcome {
    let (process, base, size, protection, old) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let result = (|| -> Result<(), u32> {
        if !super::current_process(env, process) {
            return Err(0xc0000008);
        }
        for p in [base, size] {
            check_range(env.cpu, p, 4, Prot::RW).map_err(|_| 0xc0000005u32)?;
        }
        check_range(env.cpu, old, 4, Prot::WRITE).map_err(|_| 0xc0000005u32)?;
        let address = env.cpu.read_u32(base).map_err(|_| 0xc0000005u32)?;
        let count = env.cpu.read_u32(size).map_err(|_| 0xc0000005u32)?;
        if count == 0 {
            return Err(0xc000000d);
        }
        let page = address & !4095;
        let end = (u64::from(address) + u64::from(count) + 4095) & !4095;
        let length = end - u64::from(page);
        if end > 0x100000000 || length > u64::from(u32::MAX) {
            return Err(0xc000000d);
        }
        core(env, |env| {
            crate::kernel32::protect_memory(env, address, count, protection, old)
        })?;
        env.cpu.write_u32(base, page).map_err(|_| 0xc0000005u32)?;
        env.cpu
            .write_u32(size, length as u32)
            .map_err(|_| 0xc0000005u32)?;
        Ok(())
    })();
    Outcome::Return(result.err().unwrap_or(0))
}
pub(super) fn register(reg: &mut Registry) {
    reg.register(allocate_ABI);
    reg.register(zw_allocate_ABI);
    reg.register(free_ABI);
    reg.register(zw_free_ABI);
    for name in ["NtCreateSection", "ZwCreateSection"] {
        crate::stdcall(reg, "ntdll.dll", name, 7, create);
    }
    for name in ["NtMapViewOfSection", "ZwMapViewOfSection"] {
        crate::stdcall(reg, "ntdll.dll", name, 10, map);
    }
    for name in ["NtUnmapViewOfSection", "ZwUnmapViewOfSection"] {
        crate::stdcall(reg, "ntdll.dll", name, 2, unmap);
    }
    // ZwClose is registered by the typed file front; both names use its core.
    crate::stdcall(reg, "ntdll.dll", "NtClose", 1, close);
    for name in ["NtProtectVirtualMemory", "ZwProtectVirtualMemory"] {
        crate::stdcall(reg, "ntdll.dll", name, 5, protect);
    }
}
