//! Virtual memory operations on one process, and the ABI/service boundary for
//! remote operations. No remote request carries a borrowed guest pointer.
use std::collections::BTreeMap;

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{DYN_END, DYN_START, GRANULE, PAGE};
use adv_machine::{Env, Outcome};
use adv_os::processes::{self, Error};

use super::super::{K32, child_process, file_mapping};
use super::MAX_APPLICATION_ADDRESS;
use crate::{
    stdcall,
    util::{err, set_last_error},
};

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_PRIVATE: u32 = 0x2_0000;
const MEM_TOP_DOWN: u32 = 0x10_0000;
const INVALID_ADDRESS: u32 = 487;
const NOACCESS: u32 = 998;
const PARTIAL_COPY: u32 = 299;
const VM_OPERATION: u32 = 8;
const VM_READ: u32 = 16;
const VM_WRITE: u32 = 32;
const QUERY_INFORMATION: u32 = 0x400;
/// Bound owned request/reply payloads independently of guest-supplied lengths.
const MAX_TRANSFER: u32 = 64 * 1024 * 1024;

#[derive(Default)]
struct Allocations(BTreeMap<u32, Allocation>);
struct Allocation {
    protection: u32,
    /// Exact PAGE_* values for committed private pages.
    pages: BTreeMap<u32, u32>,
}

fn error(code: u32) -> Error {
    Error::new(code, "guest virtual memory operation failed")
}
fn finish(env: &mut Env<'_>, result: Result<u32, Error>) -> Outcome {
    match result {
        Ok(value) => Outcome::Return(value),
        Err(error) => {
            set_last_error(env, error.code);
            Outcome::Return(0)
        }
    }
}
fn state<'a>(env: &'a mut Env<'_>) -> &'a mut Allocations {
    if env.ext.try_get::<Allocations>().is_none() {
        env.ext.insert(Allocations::default());
    }
    env.ext.get_mut::<Allocations>()
}
pub(crate) fn span(addr: u32, size: u32) -> Result<(u32, u32), Error> {
    if size == 0 {
        return Err(error(err::INVALID_PARAMETER));
    }
    let page = addr & !(PAGE - 1);
    let end = (u64::from(addr) + u64::from(size)).next_multiple_of(u64::from(PAGE));
    if end > u64::from(MAX_APPLICATION_ADDRESS) + 1 {
        return Err(error(INVALID_ADDRESS));
    }
    let len = u32::try_from(end - u64::from(page)).map_err(|_| error(INVALID_ADDRESS))?;
    Ok((page, len))
}
/// Backend protection for a page protection value under the process DEP policy.
pub(crate) fn page_prot(env: &Env<'_>, value: u32) -> Option<Prot> {
    decode(env, value).ok()
}
/// Without DEP (an image lacking NX_COMPAT) readable pages also execute.
fn decode(env: &Env<'_>, value: u32) -> Result<Prot, Error> {
    let prot = decode_value(value)?;
    let dep_disabled = adv_os::try_modules(env.ext).is_some_and(|m| m.dep_disabled());
    Ok(if dep_disabled && prot != Prot::default() {
        prot | Prot::EXEC
    } else {
        prot
    })
}
fn decode_value(value: u32) -> Result<Prot, Error> {
    Ok(match value {
        1 => Prot::default(),
        2 => Prot::READ,
        4 | 8 => Prot::RW,
        0x10 => Prot::EXEC,
        0x20 => Prot::RX,
        0x40 | 0x80 => Prot::RWX,
        _ => return Err(error(err::INVALID_PARAMETER)),
    })
}
fn output(env: &Env<'_>, address: u32, len: u32) -> Result<(), Error> {
    if address == 0 || !env.cpu.accessible(address, len, Prot::WRITE) {
        Err(error(NOACCESS))
    } else {
        Ok(())
    }
}
fn optional_output(env: &Env<'_>, address: u32) -> Result<(), Error> {
    if address == 0 {
        Ok(())
    } else {
        output(env, address, 4)
    }
}
fn count(env: &mut Env<'_>, address: u32, value: u32) -> Result<(), Error> {
    optional_output(env, address)?;
    if address != 0 {
        env.cpu
            .write_u32(address, value)
            .map_err(|_| error(NOACCESS))?;
    }
    Ok(())
}

/// Windows aligns reservation bases to a granule, but their extent to a page.
/// Walk reserved boundaries rather than probing every page in the address space.
fn reserve_any(env: &mut Env<'_>, len: u32, top_down: bool) -> Option<u32> {
    let mut cursor = DYN_START;
    let mut candidate = None;
    while cursor < DYN_END {
        if let Some((base, size)) = env.sys.vm.containing(cursor) {
            let end = u64::from(base) + u64::from(size);
            if end >= u64::from(DYN_END) {
                break;
            }
            cursor = u32::try_from(end.next_multiple_of(u64::from(GRANULE))).ok()?;
            continue;
        }
        let end = env
            .sys
            .vm
            .next_region(cursor)
            .unwrap_or(DYN_END)
            .min(DYN_END);
        if end.checked_sub(cursor).is_some_and(|gap| gap >= len) {
            candidate = Some(if top_down {
                (end - len) & !(GRANULE - 1)
            } else {
                cursor
            });
            if !top_down {
                break;
            }
        }
        if end == DYN_END {
            break;
        }
        cursor = end;
    }
    let base = candidate?;
    env.sys.vm.reserve_at(base, len).then_some(base)
}

pub(crate) fn allocate(
    env: &mut Env<'_>,
    addr: u32,
    size: u32,
    ty: u32,
    protection: u32,
) -> Result<u32, Error> {
    let prot = decode(env, protection)?;
    if matches!(protection, 8 | 0x80) {
        return Err(error(err::INVALID_PARAMETER));
    }
    if ty & !(MEM_COMMIT | MEM_RESERVE | MEM_TOP_DOWN) != 0 || ty & (MEM_COMMIT | MEM_RESERVE) == 0
    {
        return Err(error(err::INVALID_PARAMETER));
    }
    let (page, len) = span(addr, size)?;
    let reserve = ty & MEM_RESERVE != 0 || addr == 0;
    let (base, page, len) = if reserve {
        let base = if addr == 0 {
            reserve_any(env, len, ty & MEM_TOP_DOWN != 0)
                .ok_or_else(|| error(err::NOT_ENOUGH_MEMORY))?
        } else {
            let base = addr & !(GRANULE - 1);
            if base < GRANULE {
                return Err(error(INVALID_ADDRESS));
            }
            let extent = page
                .checked_add(len)
                .and_then(|end| end.checked_sub(base))
                .ok_or_else(|| error(INVALID_ADDRESS))?;
            if !env.sys.vm.reserve_at(base, extent) {
                return Err(error(INVALID_ADDRESS));
            }
            base
        };
        let extent = env.sys.vm.region(base).unwrap();
        state(env).0.insert(
            base,
            Allocation {
                protection,
                pages: BTreeMap::new(),
            },
        );
        (base, base, extent)
    } else {
        let (base, extent) = env
            .sys
            .vm
            .containing(page)
            .ok_or_else(|| error(INVALID_ADDRESS))?;
        if u64::from(page) + u64::from(len) > u64::from(base) + u64::from(extent)
            || env
                .ext
                .try_get::<Allocations>()
                .is_none_or(|s| !s.0.contains_key(&base))
        {
            return Err(error(INVALID_ADDRESS));
        }
        (base, page, len)
    };
    if ty & MEM_COMMIT != 0 {
        let previous: Vec<_> = (page..page + len)
            .step_by(PAGE as usize)
            .filter_map(|p| page_protection(env, p).map(|protection| (p, protection)))
            .collect();
        let mut mapped = Vec::new();
        let end = page + len;
        let mut cursor = page;
        while cursor < end {
            if env.cpu.accessible(cursor, PAGE, Prot::default()) {
                cursor += PAGE;
                continue;
            }
            let start = cursor;
            cursor += PAGE;
            while cursor < end && !env.cpu.accessible(cursor, PAGE, Prot::default()) {
                cursor += PAGE;
            }
            if env.cpu.map(start, cursor - start, prot).is_err() {
                for (start, len) in mapped {
                    let _ = env.cpu.unmap(start, len);
                }
                if reserve {
                    let _ = env.sys.vm.free(env.cpu, base);
                    state(env).0.remove(&base);
                }
                return Err(error(err::NOT_ENOUGH_MEMORY));
            }
            mapped.push((start, cursor - start));
        }
        // Recommit preserves existing contents and applies the requested
        // protection to all pages, matching VirtualAlloc on Windows.
        if env.cpu.protect(page, len, prot).is_err() {
            for (p, protection) in previous {
                let _ = env.cpu.protect(p, PAGE, decode(env, protection).unwrap());
            }
            for (start, len) in mapped {
                let _ = env.cpu.unmap(start, len);
            }
            if reserve {
                let _ = env.sys.vm.free(env.cpu, base);
                state(env).0.remove(&base);
            }
            return Err(error(INVALID_ADDRESS));
        }
        let allocation = state(env).0.get_mut(&base).unwrap();
        for p in (page..page + len).step_by(PAGE as usize) {
            allocation.pages.insert(p, protection);
        }
    }
    Ok(if reserve { base } else { page })
}

pub(crate) fn allocation_extent(env: &Env<'_>, addr: u32) -> Option<(u32, u32)> {
    let (base, extent) = env.sys.vm.containing(addr)?;
    env.ext
        .try_get::<Allocations>()?
        .0
        .contains_key(&base)
        .then_some((base, extent))
}

pub(crate) fn release(env: &mut Env<'_>, addr: u32, size: u32, ty: u32) -> Result<u32, Error> {
    if !matches!(ty, MEM_RELEASE | MEM_DECOMMIT) {
        return Err(error(err::INVALID_PARAMETER));
    }
    // LoadLibraryEx data-file mappings belong to the loader. Windows rejects
    // VirtualFree on these views with ERROR_INVALID_PARAMETER, rather than the
    // ERROR_INVALID_ADDRESS used for ordinary non-VirtualAlloc regions.
    if adv_os::try_modules(env.ext)
        .and_then(|modules| modules.data_file_containing(addr))
        .is_some()
    {
        return Err(error(err::INVALID_PARAMETER));
    }
    // Only VirtualAlloc reservations can be released/decommitted here. Modules,
    // section views, stacks and heaps retain their own ownership and lifetime.
    let (base, extent) = env
        .sys
        .vm
        .containing(addr)
        .ok_or_else(|| error(INVALID_ADDRESS))?;
    if env
        .ext
        .try_get::<Allocations>()
        .is_none_or(|s| !s.0.contains_key(&base))
    {
        return Err(error(INVALID_ADDRESS));
    }
    if ty == MEM_RELEASE {
        if size != 0 || addr != base {
            return Err(error(err::INVALID_PARAMETER));
        }
        env.sys
            .vm
            .free(env.cpu, base)
            .map_err(|_| error(INVALID_ADDRESS))?;
        state(env).0.remove(&base);
    } else {
        let (page, len) = if size == 0 {
            if addr != base {
                return Err(error(INVALID_ADDRESS));
            }
            (base, extent)
        } else {
            span(addr, size)?
        };
        if u64::from(page) + u64::from(len) > u64::from(base) + u64::from(extent) {
            return Err(error(INVALID_ADDRESS));
        }
        for p in (page..page + len).step_by(PAGE as usize) {
            if env.cpu.accessible(p, PAGE, Prot::default()) {
                env.cpu.unmap(p, PAGE).map_err(|_| error(INVALID_ADDRESS))?;
                state(env).0.get_mut(&base).unwrap().pages.remove(&p);
            }
        }
    }
    Ok(1)
}

fn page_protection(env: &Env<'_>, page: u32) -> Option<u32> {
    if !env.cpu.accessible(page, PAGE, Prot::default()) {
        return None;
    }
    if let Some(raw) = adv_os::try_modules(env.ext).and_then(|m| m.data_file_containing(page)) {
        return Some(raw.data_file.as_ref()?.protection(env.cpu, raw.base, page));
    }
    if let Some(protection) =
        adv_os::try_modules(env.ext).and_then(|modules| modules.image_protection(env.cpu, page))
    {
        return Some(protection);
    }
    if let Some(value) = env
        .ext
        .try_get::<Allocations>()
        .and_then(|s| s.0.range(..=page).next_back())
        .and_then(|(_, a)| a.pages.get(&page))
    {
        return Some(*value);
    }
    Some(
        match (
            env.cpu.accessible(page, PAGE, Prot::READ),
            env.cpu.accessible(page, PAGE, Prot::WRITE),
            env.cpu.accessible(page, PAGE, Prot::EXEC),
        ) {
            (_, true, true) => 0x40,
            (true, false, true) => 0x20,
            (false, false, true) => 0x10,
            (_, true, false) => 4,
            (true, false, false) => 2,
            (false, false, false) => 1,
        },
    )
}

pub(crate) fn query(env: &mut Env<'_>, addr: u32) -> Result<[u32; 7], Error> {
    if addr > MAX_APPLICATION_ADDRESS {
        return Err(error(err::INVALID_PARAMETER));
    }
    let page = addr & !(PAGE - 1);
    if let Some(mut info) = file_mapping::query(env, addr) {
        // Queries describe the run beginning at the queried page.
        info[3] -= page - info[0];
        info[0] = page;
        return Ok(info);
    }
    let modules = adv_os::try_modules(env.ext);
    let image = modules
        .and_then(|m| m.containing(addr).map(|id| m.get(id)))
        .filter(|m| m.size != 0);
    let raw = modules.and_then(|m| m.data_file_containing(addr));
    let allocation = image
        .map(|m| (m.base, m.size))
        .or_else(|| raw.map(|m| (m.base, m.size)))
        .or_else(|| env.sys.vm.containing(addr));
    let protection = page_protection(env, page);
    let limit = (MAX_APPLICATION_ADDRESS + 1).min(
        allocation
            .map(|(b, s)| b.saturating_add(s))
            .unwrap_or_else(|| {
                env.sys
                    .vm
                    .next_region(page)
                    .unwrap_or(MAX_APPLICATION_ADDRESS + 1)
            }),
    );
    if allocation.is_none() && protection.is_none() {
        // Direct backend mappings, including PAGE_NOACCESS and lazy bindings,
        // occupy the gap even when they have no VM reservation metadata.
        let end = env
            .cpu
            .next_mapped_page(page, u64::from(limit))
            .map_err(|_| error(NOACCESS))?
            .unwrap_or(limit);
        // Windows reports free address space as PAGE_NOACCESS.
        return Ok([page, 0, 0, end - page, MEM_FREE, 1, 0]);
    }
    let (base, _) = allocation.unwrap_or((page, PAGE));
    let limit = if allocation.is_none() {
        page + PAGE
    } else {
        limit
    };
    let mut end = if protection.is_none() {
        env.cpu
            .next_mapped_page(page, u64::from(limit))
            .map_err(|_| error(NOACCESS))?
            .unwrap_or(limit)
    } else {
        page + PAGE
    };
    while end < limit && page_protection(env, end) == protection {
        end += PAGE;
    }
    let allocation_protection = env
        .ext
        .try_get::<Allocations>()
        .and_then(|s| s.0.get(&base))
        .map(|a| a.protection)
        .unwrap_or(if raw.is_some() {
            2
        } else if image.is_some() {
            // SEC_IMAGE views are created PAGE_EXECUTE_WRITECOPY; sections
            // narrow the page protections, not the allocation's.
            0x80
        } else {
            protection.unwrap_or(0)
        });
    Ok([
        page,
        base,
        allocation_protection,
        end.min(limit) - page,
        if protection.is_some() {
            MEM_COMMIT
        } else {
            MEM_RESERVE
        },
        protection.unwrap_or(0),
        if raw.is_some() {
            0x4_0000
        } else if image.is_some() {
            0x100_0000
        } else {
            MEM_PRIVATE
        },
    ])
}

fn protect(env: &mut Env<'_>, addr: u32, size: u32, protection: u32) -> Result<u32, Error> {
    let prot = decode(env, protection)?;
    let (page, len) = span(addr, size)?;
    if !env.cpu.accessible(page, len, Prot::default()) {
        return Err(error(INVALID_ADDRESS));
    }
    if let Some(result) = file_mapping::protect(env, page, len, protection) {
        return result.map_err(error);
    }
    if matches!(protection, 8 | 0x80)
        && adv_os::try_modules(env.ext).is_none_or(|modules| {
            modules.containing(page).is_none() && modules.data_file_containing(page).is_none()
        })
    {
        return Err(error(err::INVALID_PARAMETER));
    }
    let (base, extent) = env
        .sys
        .vm
        .containing(page)
        .ok_or_else(|| error(INVALID_ADDRESS))?;
    if u64::from(page) + u64::from(len) > u64::from(base) + u64::from(extent) {
        return Err(error(INVALID_ADDRESS));
    }
    let raw = adv_os::try_modules(env.ext).and_then(|m| m.data_file_containing(page));
    if let Some(raw) = raw {
        if u64::from(page) + u64::from(len) > u64::from(raw.base) + u64::from(raw.size) {
            return Err(error(INVALID_ADDRESS));
        }
        if !matches!(protection, 1 | 2 | 8) {
            return Err(error(err::INVALID_PARAMETER));
        }
    }
    let previous = page_protection(env, page).ok_or_else(|| error(INVALID_ADDRESS))?;
    env.cpu
        .protect(page, len, prot)
        .map_err(|_| error(INVALID_ADDRESS))?;
    if let Some(modules) = env.ext.try_get_mut::<adv_os::Modules>() {
        modules.protect_image_pages(page, len, protection);
    }
    if let Some(allocation) = state(env).0.get_mut(&base) {
        for p in (page..page + len).step_by(PAGE as usize) {
            allocation.pages.insert(p, protection);
        }
    }
    if let Some(id) = adv_os::try_modules(env.ext).and_then(|m| {
        m.data_file_containing(page)
            .and_then(|raw| m.by_handle(raw.handle()))
    }) {
        let module = adv_os::modules_mut(env.ext).get_mut(id);
        if let Some(raw) = &mut module.data_file {
            let begin = ((page - module.base) / PAGE) as usize;
            raw.protections[begin..begin + (len / PAGE) as usize].fill(protection);
        }
    }
    Ok(previous)
}

fn virtual_alloc(env: &mut Env<'_>) -> Outcome {
    let (a, s, t, p) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if let Some(outcome) = super::native::allocate(env, u32::MAX, a, s, t, p) {
        return outcome;
    }
    let result = allocate(env, a, s, t, p);
    finish(env, result)
}
fn virtual_alloc_ex(env: &mut Env<'_>) -> Outcome {
    let (h, a, s, t, p) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if let Some(outcome) = super::native::allocate(env, h, a, s, t, p) {
        return outcome;
    }
    let target = match child_process::process_target(env, h, VM_OPERATION) {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |e| allocate(e, a, s, t, p),
        finish,
    )
}
fn virtual_free(env: &mut Env<'_>) -> Outcome {
    let (a, s, t) = (env.arg(0), env.arg(1), env.arg(2));
    if let Some(outcome) = super::native::free(env, u32::MAX, a, s, t) {
        return outcome;
    }
    let result = release(env, a, s, t);
    finish(env, result)
}
fn virtual_free_ex(env: &mut Env<'_>) -> Outcome {
    let (h, a, s, t) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if let Some(outcome) = super::native::free(env, h, a, s, t) {
        return outcome;
    }
    let target = match child_process::process_target(env, h, VM_OPERATION) {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(env, target.pid, None, move |e| release(e, a, s, t), finish)
}
pub(crate) fn protect_memory(env: &mut Env<'_>, a: u32, s: u32, p: u32, out: u32) -> Outcome {
    if let Err(e) = output(env, out, 4) {
        return finish(env, Err(e));
    }
    let result = match protect(env, a, s, p) {
        Ok(previous) => env
            .cpu
            .write_u32(out, previous)
            .map(|()| 1)
            .map_err(|_| error(NOACCESS)),
        Err(error) => {
            // Native Windows publishes PAGE_NOACCESS for a rejected private
            // COPY request; memory contents and page protections stay intact.
            if error.code == err::INVALID_PARAMETER && matches!(p, 8 | 0x80) {
                let _ = env.cpu.write_u32(out, 1);
            }
            Err(error)
        }
    };
    finish(env, result)
}
fn virtual_protect(env: &mut Env<'_>) -> Outcome {
    if let Some(outcome) = super::native::protect(
        env,
        u32::MAX,
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
    ) {
        return outcome;
    }
    protect_memory(env, env.arg(0), env.arg(1), env.arg(2), env.arg(3))
}
fn virtual_protect_ex(env: &mut Env<'_>) -> Outcome {
    let (h, a, s, p, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if let Some(outcome) = super::native::protect(env, h, a, s, p, out) {
        return outcome;
    }
    let target = match child_process::process_target(env, h, VM_OPERATION).and_then(|t| {
        output(env, out, 4)?;
        Ok(t)
    }) {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |e| protect(e, a, s, p),
        move |e, result| {
            let result = result.and_then(|previous| {
                output(e, out, 4)?;
                e.cpu
                    .write_u32(out, previous)
                    .map_err(|_| error(NOACCESS))?;
                Ok(1)
            });
            finish(e, result)
        },
    )
}
fn write_info(env: &mut Env<'_>, out: u32, info: [u32; 7]) -> Result<u32, Error> {
    output(env, out, 28)?;
    let data: Vec<u8> = info.into_iter().flat_map(u32::to_le_bytes).collect();
    env.cpu.write(out, &data).map_err(|_| error(NOACCESS))?;
    Ok(28)
}
fn query_output(env: &Env<'_>, out: u32, len: u32) -> Result<(), Error> {
    if len < 28 {
        return Err(error(24));
    }
    output(env, out, 28)
}
fn virtual_query(env: &mut Env<'_>) -> Outcome {
    let (a, out, len) = (env.arg(0), env.arg(1), env.arg(2));
    if let Some(outcome) = super::native::query(env, u32::MAX, a, out, len) {
        return outcome;
    }
    let result = query_output(env, out, len)
        .and_then(|()| query(env, a))
        .and_then(|info| write_info(env, out, info));
    finish(env, result)
}
fn virtual_query_ex(env: &mut Env<'_>) -> Outcome {
    let (h, a, out, len) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if let Some(outcome) = super::native::query(env, h, a, out, len) {
        return outcome;
    }
    let target = match child_process::process_target(env, h, QUERY_INFORMATION).and_then(|t| {
        query_output(env, out, len)?;
        Ok(t)
    }) {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |e| query(e, a),
        move |e, result| {
            let result = result.and_then(|info| write_info(e, out, info));
            finish(e, result)
        },
    )
}

fn transfer_range(address: u32, size: u32) -> Result<(), Error> {
    if size > MAX_TRANSFER {
        return Err(error(err::NOT_ENOUGH_MEMORY));
    }
    if u64::from(address) + u64::from(size) > 1u64 << 32 {
        return Err(error(PARTIAL_COPY));
    }
    Ok(())
}
fn owned_buffer(size: u32) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size as usize)
        .map_err(|_| error(err::NOT_ENOUGH_MEMORY))?;
    bytes.resize(size as usize, 0);
    Ok(bytes)
}
fn read_target(cpu: &dyn Guest, address: u32, size: u32) -> Result<Vec<u8>, Error> {
    if !cpu.accessible(address, size, Prot::READ) {
        return Err(error(PARTIAL_COPY));
    }
    let mut bytes = owned_buffer(size)?;
    if size != 0 {
        cpu.read(address, &mut bytes)
            .map_err(|_| error(PARTIAL_COPY))?;
    }
    Ok(bytes)
}
fn read_process_memory(env: &mut Env<'_>) -> Outcome {
    let (h, a, buf, size, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if let Some(outcome) =
        super::native::transfer(env, "NtReadVirtualMemory", [h, a, buf, size, out])
    {
        return outcome;
    }
    let prepare = child_process::process_target(env, h, VM_READ).and_then(|target| {
        optional_output(env, out)?;
        let prepared = (|| {
            transfer_range(a, size)?;
            transfer_range(buf, size)?;
            if size != 0 {
                output(env, buf, size)?;
            }
            // A local count pointer may overlap the requested source. Capture
            // those bytes before writing any count, matching native Windows.
            Ok(if target.pid == env.sys.pid {
                Some(read_target(env.cpu, a, size))
            } else {
                None
            })
        })();
        count(env, out, 0)?;
        prepared.map(|snapshot| (target, snapshot))
    });
    let (target, snapshot) = match prepare {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |e| snapshot.unwrap_or_else(|| read_target(e.cpu, a, size)),
        move |e, result| {
            let result = result.and_then(|bytes| {
                if size != 0 {
                    output(e, buf, size)?;
                }
                optional_output(e, out)?;
                if !bytes.is_empty() {
                    e.cpu.write(buf, &bytes).map_err(|_| error(NOACCESS))?;
                }
                count(e, out, bytes.len() as u32)?;
                if bytes.len() != size as usize {
                    return Err(error(PARTIAL_COPY));
                }
                Ok(1)
            });
            finish(e, result)
        },
    )
}
/// Windows can report the accessible prefix on failure without changing any
/// target bytes. Preflight the complete transfer before performing the copy.
fn write_target(env: &mut Env<'_>, address: u32, bytes: &[u8], debugger: bool) -> (u32, u32) {
    let size = bytes.len() as u32;
    let mut checked = 0;
    let mut temporary = Vec::new();
    while checked < size {
        let at = address + checked;
        let page = at & !(PAGE - 1);
        let len = (PAGE - (at & (PAGE - 1))).min(size - checked);
        if env.cpu.accessible(at, len, Prot::WRITE) {
            // Writable pages need no debugger protection override.
        } else if debugger && env.cpu.accessible(at, len, Prot::EXEC) {
            temporary.push((
                page,
                decode(env, page_protection(env, page).unwrap()).unwrap(),
            ));
        } else {
            let code = if checked == 0 && env.cpu.accessible(at, len, Prot::default()) {
                NOACCESS
            } else {
                PARTIAL_COPY
            };
            return (checked, code);
        }
        checked += len;
    }
    // Executable pages temporarily accept debugger writes, then retain their
    // original guest permissions. Guest::write also invalidates translated code.
    let mut changed = Vec::new();
    for (page, previous) in temporary {
        if env.cpu.protect(page, PAGE, Prot::RWX).is_err() {
            for (page, previous) in changed {
                let _ = env.cpu.protect(page, PAGE, previous);
            }
            return (0, INVALID_ADDRESS);
        }
        changed.push((page, previous));
    }
    let written = size == 0 || env.cpu.write(address, bytes).is_ok();
    let mut restored = true;
    for (page, previous) in changed {
        if env.cpu.protect(page, PAGE, previous).is_err() {
            restored = false;
        }
    }
    if !restored {
        return (if written { size } else { 0 }, INVALID_ADDRESS);
    }
    if written {
        (size, 0)
    } else {
        (0, PARTIAL_COPY)
    }
}

fn native_transfer_count(env: &mut Env<'_>, pointer: u32, count: u32) {
    // Windows preserves the transfer status even if this optional output is
    // inaccessible. Capture source bytes before publishing an aliased count.
    if pointer != 0 && adv_abi::guest::check_range(env.cpu, pointer, 4, Prot::WRITE).is_ok() {
        let _ = env.cpu.write_u32(pointer, count);
    }
}

/// Native transfers keep page permissions and best-effort the optional count.
/// Unlike the Win32 debugger frontend, no execute-page write override is used.
pub(crate) fn native_write_memory(
    env: &mut Env<'_>,
    handle: u32,
    address: u32,
    buffer: u32,
    size: u32,
    copied: u32,
) -> Outcome {
    let fail = |env: &mut Env<'_>, status| {
        native_transfer_count(env, copied, 0);
        Outcome::Return(status)
    };
    if size == 0 {
        return fail(env, 0);
    }
    if size > MAX_TRANSFER {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtWriteVirtualMemory",
            "transfers beyond the bounded owned-payload limit are unavailable",
        );
        return fail(env, 0xc00000bb);
    }
    let target = match child_process::process_target(env, handle, VM_WRITE) {
        Ok(target) => target,
        Err(error) => {
            return fail(
                env,
                if error.code == 5 {
                    0xc0000022
                } else {
                    0xc0000008
                },
            );
        }
    };
    if transfer_range(address, size).is_err() || transfer_range(buffer, size).is_err() {
        return fail(env, 0xc0000005);
    }
    let bytes = match read_target(env.cpu, buffer, size) {
        Ok(bytes) => bytes,
        Err(error) => {
            return fail(
                env,
                if error.code == 8 {
                    0xc0000017
                } else {
                    0x8000000d
                },
            );
        }
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |remote| Ok(write_target(remote, address, &bytes, false)),
        move |caller, result| {
            let (count, status) = match result {
                Ok((count, 0)) => (count, 0),
                Ok((count, _)) => (count, 0x8000000d),
                Err(error) => (
                    0,
                    match error.code {
                        5 => 0xc0000022,
                        6 => 0xc0000008,
                        8 => 0xc0000017,
                        50 => {
                            adv_abi::warn_unavailable(
                                caller,
                                "ntdll.dll!NtWriteVirtualMemory",
                                &error.message,
                            );
                            0xc00000bb
                        }
                        _ => 0xc0000001,
                    },
                ),
            };
            native_transfer_count(caller, copied, count);
            Outcome::Return(status)
        },
    )
}

/// Native reads own the stopped target's bytes before publishing caller output.
pub(crate) fn native_read_memory(
    env: &mut Env<'_>,
    handle: u32,
    address: u32,
    buffer: u32,
    size: u32,
    copied: u32,
) -> Outcome {
    let fail = |env: &mut Env<'_>, status| {
        native_transfer_count(env, copied, 0);
        Outcome::Return(status)
    };
    if size == 0 {
        return fail(env, 0);
    }
    if size > MAX_TRANSFER {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtReadVirtualMemory",
            "transfers beyond the bounded owned-payload limit are unavailable",
        );
        return fail(env, 0xc00000bb);
    }
    let target = match child_process::process_target(env, handle, VM_READ) {
        Ok(target) => target,
        Err(error) => {
            return fail(
                env,
                if error.code == 5 {
                    0xc0000022
                } else {
                    0xc0000008
                },
            );
        }
    };
    if transfer_range(address, size).is_err() || transfer_range(buffer, size).is_err() {
        return fail(env, 0xc0000005);
    }
    if !env.cpu.accessible(buffer, size, Prot::WRITE) {
        return fail(env, 0x8000000d);
    }
    processes::visit(
        env,
        target.pid,
        None,
        move |remote| read_target(remote.cpu, address, size),
        move |caller, result| {
            let status = match result {
                Ok(bytes) => {
                    if !caller.cpu.accessible(buffer, size, Prot::WRITE)
                        || caller.cpu.write(buffer, &bytes).is_err()
                    {
                        native_transfer_count(caller, copied, 0);
                        return Outcome::Return(0x8000000d);
                    }
                    native_transfer_count(caller, copied, size);
                    return Outcome::Return(0);
                }
                Err(error) => match error.code {
                    5 => 0xc0000022,
                    6 => 0xc0000008,
                    8 => 0xc0000017,
                    50 => {
                        adv_abi::warn_unavailable(
                            caller,
                            "ntdll.dll!NtReadVirtualMemory",
                            &error.message,
                        );
                        0xc00000bb
                    }
                    _ => 0x8000000d,
                },
            };
            native_transfer_count(caller, copied, 0);
            Outcome::Return(status)
        },
    )
}

fn write_process_memory(env: &mut Env<'_>) -> Outcome {
    let (h, a, buf, size, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if let Some(outcome) =
        super::native::transfer(env, "NtWriteVirtualMemory", [h, a, buf, size, out])
    {
        return outcome;
    }
    let prepare =
        child_process::process_target(env, h, VM_WRITE | VM_OPERATION).and_then(|target| {
            optional_output(env, out)?;
            let prepared = (|| {
                transfer_range(a, size)?;
                transfer_range(buf, size)?;
                if !env.cpu.accessible(buf, size, Prot::READ) {
                    return Err(error(NOACCESS));
                }
                let mut bytes = owned_buffer(size)?;
                if size != 0 {
                    env.cpu.read(buf, &mut bytes).map_err(|_| error(NOACCESS))?;
                }
                Ok(bytes)
            })();
            count(env, out, 0)?;
            prepared.map(|bytes| (target, bytes))
        });
    let (target, bytes) = match prepare {
        Ok(t) => t,
        Err(e) => return finish(env, Err(e)),
    };
    processes::visit(
        env,
        target.pid,
        None,
        move |e| Ok(write_target(e, a, &bytes, true)),
        move |e, result| {
            let result = result.and_then(|(written, code)| {
                count(e, out, written)?;
                if code != 0 {
                    return Err(error(code));
                }
                Ok(1)
            });
            finish(e, result)
        },
    )
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "VirtualAlloc", 4, virtual_alloc);
    stdcall(reg, K32, "VirtualAllocEx", 5, virtual_alloc_ex);
    stdcall(reg, K32, "VirtualFree", 3, virtual_free);
    stdcall(reg, K32, "VirtualFreeEx", 4, virtual_free_ex);
    stdcall(reg, K32, "VirtualProtect", 4, virtual_protect);
    stdcall(reg, K32, "VirtualProtectEx", 5, virtual_protect_ex);
    stdcall(reg, K32, "VirtualQuery", 3, virtual_query);
    stdcall(reg, K32, "VirtualQueryEx", 4, virtual_query_ex);
    stdcall(reg, K32, "ReadProcessMemory", 5, read_process_memory);
    stdcall(reg, K32, "WriteProcessMemory", 5, write_process_memory);
}

#[cfg(test)]
mod tests;
