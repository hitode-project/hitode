//! Heap, Local/Global, and Virtual memory functions.

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::layout::{GRANULE, PAGE};
use adv_machine::{Env, Outcome};

use super::K32;
use crate::util::{err, set_last_error};
use crate::{Win32, stdcall};

const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;
const LMEM_ZEROINIT: u32 = 0x40;

fn page_span(addr: u32, size: u32) -> Option<(u32, u32)> {
    if size == 0 {
        return None;
    }
    let page = addr & !(PAGE - 1);
    let end = (u64::from(addr) + u64::from(size)).next_multiple_of(u64::from(PAGE));
    if end > 1u64 << 32 {
        return None;
    }
    Some((page, u32::try_from(end - u64::from(page)).ok()?))
}

fn heap_create(env: &mut Env<'_>) -> Outcome {
    let w = env.ext.get_mut::<Win32>();
    match w.heaps.create(env.cpu, &mut env.sys.vm) {
        Ok(h) => Outcome::Return(h),
        Err(_) => Outcome::Return(0),
    }
}

fn heap_destroy(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    let w = env.ext.get_mut::<Win32>();
    Outcome::Return(u32::from(w.heaps.destroy(env.cpu, &mut env.sys.vm, h)))
}

fn alloc(env: &mut Env<'_>, heap: u32, size: u32, zero: bool) -> u32 {
    let w = env.ext.get_mut::<Win32>();
    let p = w
        .heaps
        .alloc(env.cpu, &mut env.sys.vm, heap, size, zero)
        .unwrap_or(0);
    if p == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    p
}

fn free(env: &mut Env<'_>, heap: u32, p: u32) -> bool {
    if p == 0 {
        return true;
    }
    let w = env.ext.get_mut::<Win32>();
    let heap = if heap == 0 {
        w.heaps.owner(p).unwrap_or(0)
    } else {
        heap
    };
    let ok = w.heaps.free(env.cpu, &mut env.sys.vm, heap, p);
    if !ok {
        log::warn!(
            "free of unknown block {p:#x} (heap {heap:#x}) from {:#010x}",
            env.caller()
        );
    }
    ok
}

fn heap_alloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, size) = (env.arg(0), env.arg(1), env.arg(2));
    Outcome::Return(alloc(env, h, size, flags & HEAP_ZERO_MEMORY != 0))
}

fn heap_free(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(2));
    Outcome::Return(u32::from(free(env, h, p)))
}

fn heap_realloc(env: &mut Env<'_>) -> Outcome {
    let (h, flags, p, size) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let w = env.ext.get_mut::<Win32>();
    let zero = flags & HEAP_ZERO_MEMORY != 0;
    let in_place = flags & HEAP_REALLOC_IN_PLACE_ONLY != 0;
    let r = w
        .heaps
        .realloc(env.cpu, &mut env.sys.vm, h, p, size, zero, in_place)
        .unwrap_or(0);
    if r == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    Outcome::Return(r)
}

fn heap_size(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(2));
    Outcome::Return(
        env.ext
            .get_mut::<Win32>()
            .heaps
            .size(h, p)
            .unwrap_or(u32::MAX),
    )
}

fn local_alloc(env: &mut Env<'_>) -> Outcome {
    let (flags, size) = (env.arg(0), env.arg(1));
    Outcome::Return(alloc(env, 0, size, flags & LMEM_ZEROINIT != 0))
}

/// LocalFree/GlobalFree: NULL on success.
fn local_free(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    Outcome::Return(if free(env, 0, p) { 0 } else { p })
}

fn local_realloc(env: &mut Env<'_>) -> Outcome {
    let (p, size, flags) = (env.arg(0), env.arg(1), env.arg(2));
    let w = env.ext.get_mut::<Win32>();
    let heap = w.heaps.owner(p).unwrap_or(0);
    Outcome::Return(
        w.heaps
            .realloc(
                env.cpu,
                &mut env.sys.vm,
                heap,
                p,
                size,
                flags & LMEM_ZEROINIT != 0,
                false,
            )
            .unwrap_or(0),
    )
}

fn local_size(env: &mut Env<'_>) -> Outcome {
    let p = env.arg(0);
    let w = env.ext.get_mut::<Win32>();
    let heap = w.heaps.owner(p).unwrap_or(0);
    Outcome::Return(w.heaps.size(heap, p).unwrap_or(0))
}

/// VirtualAlloc: regions are reserved and committed in one go (the host maps them fully).
fn virtual_alloc(env: &mut Env<'_>) -> Outcome {
    let (addr, size, _ty, protection) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(prot) = decode_protection(protection) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    let Some((page, span)) = page_span(addr, size) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    if addr != 0 {
        // Committing inside a region we handed out earlier.
        let base = addr & !(GRANULE - 1);
        if let Some((start, len)) = find_region(env, addr)
            && u64::from(addr) + u64::from(size) <= u64::from(start) + u64::from(len)
            && env.cpu.protect(page, span, prot).is_ok()
        {
            return Outcome::Return(addr);
        }
        let end = (u64::from(addr) + u64::from(size)).next_multiple_of(u64::from(GRANULE));
        if let Ok(len) = u32::try_from(end - u64::from(base))
            && end <= 1u64 << 32
            && env.sys.vm.reserve_at(base, len)
        {
            let len = env.sys.vm.region(base).unwrap_or(0);
            if env.cpu.map(base, len, prot).is_ok() {
                return Outcome::Return(addr);
            }
        }
        set_last_error(env, 487); // ERROR_INVALID_ADDRESS
        return Outcome::Return(0);
    }
    if size > u32::MAX - (GRANULE - 1) {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return Outcome::Return(0);
    }
    match env.sys.vm.alloc(env.cpu, size, prot) {
        Ok(p) => Outcome::Return(p),
        Err(_) => {
            set_last_error(env, err::NOT_ENOUGH_MEMORY);
            Outcome::Return(0)
        }
    }
}

fn find_region(env: &Env<'_>, addr: u32) -> Option<(u32, u32)> {
    env.sys.vm.containing(addr)
}

fn virtual_free(env: &mut Env<'_>) -> Outcome {
    let (addr, _size, ty) = (env.arg(0), env.arg(1), env.arg(2));
    if super::file_mapping::contains(env, addr) {
        set_last_error(env, 487);
        return Outcome::Return(0);
    }
    if ty & 0x8000 != 0 {
        // MEM_RELEASE
        return Outcome::Return(u32::from(env.sys.vm.free(env.cpu, addr).is_ok()));
    }
    Outcome::Return(1) // MEM_DECOMMIT: keep it mapped
}

/// PAGE_* values supported by the backend's read/write/execute permissions.
fn decode_protection(value: u32) -> Option<Prot> {
    Some(match value {
        0x01 => Prot::default(),
        0x02 => Prot::READ,
        0x04 | 0x08 => Prot::RW,
        0x10 => Prot::EXEC,
        0x20 => Prot::RX,
        0x40 | 0x80 => Prot::RWX,
        _ => return None,
    })
}

/// Query the executor's actual committed pages, including loader sections and thunks.
fn page_protection(env: &Env<'_>, page: u32) -> Option<u32> {
    if !env.cpu.accessible(page, PAGE, Prot::default()) {
        return None;
    }
    let read = env.cpu.accessible(page, PAGE, Prot::READ);
    let write = env.cpu.accessible(page, PAGE, Prot::WRITE);
    let exec = env.cpu.accessible(page, PAGE, Prot::EXEC);
    Some(match (read, write, exec) {
        (_, true, true) => 0x40,
        (true, false, true) => 0x20,
        (false, false, true) => 0x10,
        (_, true, false) => 0x04,
        (true, false, false) => 0x02,
        (false, false, false) => 0x01,
    })
}

/// VirtualQuery(addr, MEMORY_BASIC_INFORMATION*, len). Adjacent pages are grouped only
/// while their actual protection and allocation match.
fn virtual_query(env: &mut Env<'_>) -> Outcome {
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_FREE: u32 = 0x1_0000;
    const MEM_PRIVATE: u32 = 0x2_0000;
    const MEM_IMAGE: u32 = 0x100_0000;
    let (addr, out, len) = (env.arg(0), env.arg(1), env.arg(2));
    if len < 28 {
        set_last_error(env, 24); // ERROR_BAD_LENGTH
        return Outcome::Return(0);
    }
    // Host Guest::write bypasses guest page permissions. Validate the whole output
    // before writing any field, so protected or cross-page outputs fail atomically.
    if !env.cpu.accessible(out, 28, Prot::WRITE) {
        set_last_error(env, 998); // ERROR_NOACCESS
        return Outcome::Return(0);
    }
    let page = addr & !0xfff;
    let image = adv_os::modules(env.ext)
        .containing(addr)
        .map(|id| adv_os::modules(env.ext).get(id))
        .filter(|m| m.size != 0);
    let allocation = image
        .map(|m| (m.base, m.size))
        .or_else(|| env.sys.vm.containing(addr));
    let info = if let Some(info) = super::file_mapping::query(env, addr) {
        info
    } else {
        match page_protection(env, page) {
            Some(protection) => {
                let (base, size) = allocation.unwrap_or((page, PAGE));
                let limit = u64::from(base) + u64::from(size);
                let mut begin = page;
                while begin >= base.saturating_add(PAGE)
                    && page_protection(env, begin - PAGE) == Some(protection)
                {
                    begin -= PAGE;
                }
                let mut end = u64::from(page) + u64::from(PAGE);
                while end < limit
                    && end < 1u64 << 32
                    && page_protection(env, end as u32) == Some(protection)
                {
                    end += u64::from(PAGE);
                }
                [
                    begin,
                    base,
                    protection,
                    (end - u64::from(begin)) as u32,
                    MEM_COMMIT,
                    protection,
                    if image.is_some() {
                        MEM_IMAGE
                    } else {
                        MEM_PRIVATE
                    },
                ]
            }
            None => [page, 0, 0, PAGE, MEM_FREE, 0x01, 0],
        }
    };
    for (i, v) in info.into_iter().enumerate() {
        if env.cpu.write_u32(out + 4 * i as u32, v).is_err() {
            set_last_error(env, 998); // ERROR_NOACCESS
            return Outcome::Return(0);
        }
    }
    Outcome::Return(28)
}

fn virtual_protect(env: &mut Env<'_>) -> Outcome {
    let (addr, size, protection, old) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(prot) = decode_protection(protection) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    let Some((page, len)) = page_span(addr, size) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    };
    if old == 0 || !env.cpu.accessible(old, 4, Prot::WRITE) {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    if let Some(result) = super::file_mapping::protect(env, page, len, protection) {
        return match result {
            Ok(previous) => match env.cpu.write_u32(old, previous) {
                Ok(()) => Outcome::Return(1),
                Err(_) => {
                    set_last_error(env, 998);
                    Outcome::Return(0)
                }
            },
            Err(error) => {
                set_last_error(env, error);
                Outcome::Return(0)
            }
        };
    }
    let Some(previous) = page_protection(env, page) else {
        set_last_error(env, 487); // ERROR_INVALID_ADDRESS
        return Outcome::Return(0);
    };
    if env.sys.vm.containing(addr).is_none_or(|(base, size)| {
        u64::from(page) + u64::from(len) > u64::from(base) + u64::from(size)
    }) {
        set_last_error(env, 487);
        return Outcome::Return(0);
    }
    if env.cpu.protect(page, len, prot).is_err() || env.cpu.write_u32(old, previous).is_err() {
        set_last_error(env, 487);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "HeapCreate", 3, heap_create);
    stdcall(reg, K32, "HeapDestroy", 1, heap_destroy);
    stdcall(reg, K32, "HeapAlloc", 3, heap_alloc);
    stdcall(reg, K32, "HeapFree", 3, heap_free);
    stdcall(reg, K32, "HeapReAlloc", 4, heap_realloc);
    stdcall(reg, K32, "HeapSize", 3, heap_size);
    stdcall(reg, K32, "HeapSetInformation", 4, |_| Outcome::Return(1));
    stdcall(reg, K32, "HeapValidate", 3, |_| Outcome::Return(1));
    stdcall(reg, K32, "HeapCompact", 2, |_| Outcome::Return(0x10_0000));
    stdcall(reg, K32, "GetProcessHeap", 0, |e| {
        Outcome::Return(e.ext.get::<Win32>().heaps.process_heap)
    });
    for p in ["Local", "Global"] {
        stdcall(reg, K32, &format!("{p}Alloc"), 2, local_alloc);
        stdcall(reg, K32, &format!("{p}Free"), 1, local_free);
        stdcall(reg, K32, &format!("{p}ReAlloc"), 3, local_realloc);
        stdcall(reg, K32, &format!("{p}Size"), 1, local_size);
        stdcall(reg, K32, &format!("{p}Lock"), 1, |e| {
            Outcome::Return(e.arg(0))
        });
        stdcall(reg, K32, &format!("{p}Unlock"), 1, |_| Outcome::Return(1));
        stdcall(reg, K32, &format!("{p}Handle"), 1, |e| {
            Outcome::Return(e.arg(0))
        });
    }
    stdcall(reg, K32, "VirtualAlloc", 4, virtual_alloc);
    stdcall(reg, K32, "VirtualFree", 3, virtual_free);
    stdcall(reg, K32, "VirtualProtect", 4, virtual_protect);
    stdcall(reg, K32, "VirtualQuery", 3, virtual_query);
    // Guest writes/protection changes already invalidate translated code in both
    // executors. There is no separate guest instruction cache to drain.
    stdcall(reg, K32, "FlushInstructionCache", 3, |_| Outcome::Return(1));
    stdcall(reg, K32, "IsBadReadPtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadWritePtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadCodePtr", 1, |_| Outcome::Return(0));
}
