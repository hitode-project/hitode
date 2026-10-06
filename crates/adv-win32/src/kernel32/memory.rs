//! Heap, Local/Global, and Virtual memory functions.

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::layout::{GRANULE, PAGE};
use adv_machine::{Env, Outcome};

use super::K32;
use crate::stdcall;
use crate::util::{err, set_last_error};
use adv_os::Heaps;
mod global;
mod locks;

const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;

/// Highest application address in the existing flat x86 Win32 process profile.
/// GetSystemInfo and VirtualQuery must describe the same address domain.
pub(super) const MAX_APPLICATION_ADDRESS: u32 = 0x7ffe_ffff;

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
    let heaps = env.ext.get_mut::<Heaps>();
    match heaps.create(env.cpu, &mut env.sys.vm) {
        Ok(h) => Outcome::Return(h),
        Err(_) => Outcome::Return(0),
    }
}

fn heap_destroy(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    let heaps = env.ext.get_mut::<Heaps>();
    Outcome::Return(u32::from(heaps.destroy(env.cpu, &mut env.sys.vm, h)))
}

fn alloc(env: &mut Env<'_>, heap: u32, size: u32, zero: bool) -> u32 {
    let heaps = env.ext.get_mut::<Heaps>();
    let p = heaps
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
    if let Some(block) = global_block(env, p) {
        if block.movable || (heap != 0 && heap != env.ext.get::<Heaps>().process_heap) {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return false;
        }
        return global::release(env, block.handle, false) == 0;
    }
    let heaps = env.ext.get_mut::<Heaps>();
    let heap = if heap == 0 {
        heaps.owner(p).unwrap_or(0)
    } else {
        heap
    };
    let ok = heaps.free(env.cpu, &mut env.sys.vm, heap, p);
    if !ok {
        log::warn!(
            "free of unknown block {p:#x} (heap {heap:#x}) from {:#010x}",
            env.caller()
        );
    }
    ok
}
fn global_block(env: &Env<'_>, pointer: u32) -> Option<adv_os::global::Block> {
    let globals = env.ext.try_get::<adv_os::global::Globals>()?;
    globals
        .get(
            &env.sys.objects,
            globals.from_pointer(&env.sys.objects, pointer)?,
        )
        .cloned()
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
    if let Some(block) = global_block(env, p) {
        if block.movable || (h != 0 && h != env.ext.get::<Heaps>().process_heap) {
            set_last_error(env, if block.clipboard { 5 } else { 87 });
            return Outcome::Return(0);
        }
        return Outcome::Return(global::resize(
            env,
            block.handle,
            size,
            if flags & HEAP_REALLOC_IN_PLACE_ONLY != 0 {
                0
            } else {
                2
            } | if flags & HEAP_ZERO_MEMORY != 0 {
                0x40
            } else {
                0
            },
            false,
        ));
    }
    let heaps = env.ext.get_mut::<Heaps>();
    let zero = flags & HEAP_ZERO_MEMORY != 0;
    let in_place = flags & HEAP_REALLOC_IN_PLACE_ONLY != 0;
    let r = heaps
        .realloc(env.cpu, &mut env.sys.vm, h, p, size, zero, in_place)
        .unwrap_or(0);
    if r == 0 {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
    }
    Outcome::Return(r)
}

fn heap_size(env: &mut Env<'_>) -> Outcome {
    let (h, p) = (env.arg(0), env.arg(2));
    Outcome::Return(env.ext.get_mut::<Heaps>().size(h, p).unwrap_or(u32::MAX))
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
    let (addr, size, ty) = (env.arg(0), env.arg(1), env.arg(2));
    if adv_os::try_modules(env.ext)
        .and_then(|modules| modules.data_file_containing(addr))
        .is_some()
    {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
    if super::file_mapping::contains(env, addr) {
        set_last_error(env, 487);
        return Outcome::Return(0);
    }
    if ty & 0x8000 != 0 {
        // MEM_RELEASE
        let allocation = env.sys.vm.region(addr);
        let released = env.sys.vm.free(env.cpu, addr).is_ok();
        if !released && allocation.is_some_and(|length| env.cpu.has_locked_pages(addr, length)) {
            adv_abi::warn_unavailable(
                env,
                "kernel32.dll!VirtualFree",
                "MEM_RELEASE could not retire physical page pins; the allocation remains reserved",
            );
            set_last_error(env, 50);
        }
        return Outcome::Return(u32::from(released));
    }
    if ty & 0x4000 != 0 {
        let size = if size == 0 {
            find_region(env, addr).map_or(0, |(_, length)| length)
        } else {
            size
        };
        if let Some((page, length)) = page_span(addr, size)
            && env.cpu.has_locked_pages(page, length)
        {
            adv_abi::warn_unavailable(
                env,
                "kernel32.dll!VirtualFree",
                "MEM_DECOMMIT of pinned pages is unavailable in the current keep-mapped VM profile; unlock or release the allocation instead",
            );
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
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
    if let Some(module) =
        adv_os::try_modules(env.ext).and_then(|modules| modules.data_file_containing(page))
    {
        return Some(
            module
                .data_file
                .as_ref()?
                .protection(env.cpu, module.base, page),
        );
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
    const MEM_MAPPED: u32 = 0x4_0000;
    const MEM_IMAGE: u32 = 0x100_0000;
    let (addr, out, len) = (env.arg(0), env.arg(1), env.arg(2));
    if addr > MAX_APPLICATION_ADDRESS {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(0);
    }
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
    let modules = adv_os::try_modules(env.ext);
    let image = modules
        .and_then(|modules| modules.containing(addr).map(|id| modules.get(id)))
        .filter(|m| m.size != 0);
    let data_file = modules.and_then(|modules| modules.data_file_containing(addr));
    let allocation = image
        .map(|m| (m.base, m.size))
        .or_else(|| data_file.map(|m| (m.base, m.size)))
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
                    if data_file.is_some() { 2 } else { protection },
                    (end - u64::from(begin)) as u32,
                    MEM_COMMIT,
                    protection,
                    if data_file.is_some() {
                        MEM_MAPPED
                    } else if image.is_some() {
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
    protect_memory(env, addr, size, protection, old)
}

pub(crate) fn protect_memory(
    env: &mut Env<'_>,
    addr: u32,
    size: u32,
    protection: u32,
    old: u32,
) -> Outcome {
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
    if let Some(raw) =
        adv_os::try_modules(env.ext).and_then(|modules| modules.data_file_containing(page))
    {
        if u64::from(page) + u64::from(len) > u64::from(raw.base) + u64::from(raw.size) {
            set_last_error(env, 487);
            return Outcome::Return(0);
        }
        if !matches!(protection, 1 | 2 | 8) {
            set_last_error(env, err::INVALID_PARAMETER);
            return Outcome::Return(0);
        }
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
    if let Some(id) = adv_os::try_modules(env.ext).and_then(|modules| {
        modules
            .data_file_containing(page)
            .and_then(|module| modules.by_handle(module.handle()))
    }) {
        let module = adv_os::modules_mut(env.ext).get_mut(id);
        if let Some(raw) = &mut module.data_file {
            let begin = ((page - module.base) / PAGE) as usize;
            raw.protections[begin..begin + (len / PAGE) as usize].fill(protection);
        }
    }
    Outcome::Return(1)
}

fn read_process_memory(env: &mut Env<'_>) -> Outcome {
    let (_process, base, buf, size, out_read) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
    );
    let mut data = vec![0u8; size as usize];
    if env.cpu.read(base, &mut data).is_err() || env.cpu.write(buf, &data).is_err() {
        set_last_error(env, 299);
        return Outcome::Return(0);
    }
    if out_read != 0 {
        let _ = env.cpu.write_u32(out_read, size);
    }
    Outcome::Return(1)
}

fn write_process_memory(env: &mut Env<'_>) -> Outcome {
    let (_process, base, buf, size, out_written) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
    );
    let mut data = vec![0u8; size as usize];
    if env.cpu.read(buf, &mut data).is_err() {
        set_last_error(env, 299);
        return Outcome::Return(0);
    }
    if env.cpu.write(base, &data).is_err() {
        let Some((page, len)) = page_span(base, size) else {
            set_last_error(env, 299);
            return Outcome::Return(0);
        };
        let _ = env.cpu.protect(page, len, Prot::RW);
        if env.cpu.write(base, &data).is_err() {
            set_last_error(env, 299);
            return Outcome::Return(0);
        }
    }
    if out_written != 0 {
        let _ = env.cpu.write_u32(out_written, size);
    }
    Outcome::Return(1)
}

pub(super) fn register(reg: &mut Registry) {
    global::register(reg);
    locks::register(reg);
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
        Outcome::Return(e.ext.get::<Heaps>().process_heap)
    });
    stdcall(reg, K32, "VirtualAlloc", 4, virtual_alloc);
    stdcall(reg, K32, "VirtualFree", 3, virtual_free);
    stdcall(reg, K32, "VirtualProtect", 4, virtual_protect);
    stdcall(reg, K32, "VirtualQuery", 3, virtual_query);
    stdcall(reg, K32, "ReadProcessMemory", 5, read_process_memory);
    stdcall(reg, K32, "WriteProcessMemory", 5, write_process_memory);
    // Guest writes/protection changes already invalidate translated code in both
    // executors. There is no separate guest instruction cache to drain.
    stdcall(reg, K32, "FlushInstructionCache", 3, |_| Outcome::Return(1));
    stdcall(reg, K32, "IsBadReadPtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadWritePtr", 2, |_| Outcome::Return(0));
    stdcall(reg, K32, "IsBadCodePtr", 1, |_| Outcome::Return(0));
}
