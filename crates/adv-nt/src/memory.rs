//! Virtual memory: the NT allocation, protection and query services.
//!
//! Each NT allocation keeps the state of its pages, as Wine's `virtual.c`
//! does: reserved pages are not mapped in the guest, committed pages are
//! mapped with their protection. Address space comes from the machine's
//! reservations, which also hold what the machine allocates itself; such a
//! range reads as a reserved private region.

use std::collections::BTreeMap;

use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{DYN_END, DYN_START, GRANULE, PAGE};
use adv_machine::vm::align_up;
use adv_machine::{Env, Outcome, Vm};

use crate::status::{self, Status};
use crate::unixlib;

pub const MEM_COMMIT: u32 = 0x1000;
pub const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_RESET: u32 = 0x8_0000;
const MEM_TOP_DOWN: u32 = 0x10_0000;
const MEM_WRITE_WATCH: u32 = 0x20_0000;
const MEM_RESERVE_PLACEHOLDER: u32 = 0x4_0000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x4000;
const MEM_PRESERVE_PLACEHOLDER: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Placeholder {
    None,
    Free,
    Replaced,
}

pub const PAGE_NOACCESS: u32 = 0x01;
pub const PAGE_READONLY: u32 = 0x02;
pub const PAGE_READWRITE: u32 = 0x04;
pub const PAGE_WRITECOPY: u32 = 0x08;
pub const PAGE_EXECUTE: u32 = 0x10;
pub const PAGE_EXECUTE_READ: u32 = 0x20;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
pub const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
const PAGE_GUARD: u32 = 0x100;
/// A stack guard page.
pub const PAGE_GUARD_READWRITE: u32 = PAGE_GUARD | PAGE_READWRITE;
const PAGE_NOCACHE: u32 = 0x200;
const PAGE_WRITECOMBINE: u32 = 0x400;

/// The highest address of the user address space.
pub const USER_END: u32 = 0x7fff_0000;
/// `NtCurrentProcess()`.
pub const CURRENT_PROCESS: u32 = u32::MAX;
const BASIC_INFORMATION: u32 = 0;
const BASIC_INFORMATION_SIZE: u32 = 28;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Private = 0x2_0000,
    Mapped = 0x4_0000,
    Image = 0x100_0000,
}

/// A page's protection, or `None` while it is only reserved.
type Page = Option<u32>;

struct Region {
    kind: Kind,
    protection: u32,
    placeholder: Placeholder,
    pages: Vec<Page>,
    write_watch: Option<Vec<bool>>,
    maximum_protection: Option<u32>,
}

impl Region {
    fn end(&self, base: u32) -> u32 {
        base + self.pages.len() as u32 * PAGE
    }
}

/// The process's NT allocations, by allocation base.
#[derive(Default)]
pub struct Memory {
    regions: BTreeMap<u32, Region>,
    /// Readable pages also execute: the main image does not opt in to DEP.
    pub execute_readable: bool,
}

impl Memory {
    /// Records memory the platform mapped itself, with each page's protection.
    pub fn insert(&mut self, base: u32, kind: Kind, protection: u32, pages: Vec<Page>) {
        self.regions.insert(
            base,
            Region {
                kind,
                protection,
                placeholder: Placeholder::None,
                write_watch: None,
                maximum_protection: None,
                pages,
            },
        );
    }

    /// Forgets the platform's private region at `base..end`, which the
    /// platform releases itself, so it cannot shadow a later allocation.
    pub fn forget(&mut self, base: u32, end: u32) {
        if self
            .regions
            .get(&base)
            .is_some_and(|region| region.kind == Kind::Private && region.end(base) == end)
        {
            self.regions.remove(&base);
        }
    }

    fn prot(&self, protection: u32) -> Result<Prot, Status> {
        let prot = match protection & !(PAGE_GUARD | PAGE_NOCACHE | PAGE_WRITECOMBINE) {
            PAGE_NOACCESS => Prot(0),
            PAGE_READONLY => Prot::READ,
            PAGE_READWRITE | PAGE_WRITECOPY => Prot::RW,
            PAGE_EXECUTE => Prot::EXEC,
            PAGE_EXECUTE_READ => Prot::RX,
            PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => Prot::RWX,
            _ => return Err(status::INVALID_PAGE_PROTECTION),
        };
        // A guard page faults on first touch; it is never accessible here.
        Ok(if protection & PAGE_GUARD != 0 {
            Prot(0)
        } else if self.execute_readable && prot.contains(Prot::READ) {
            prot | Prot::EXEC
        } else {
            prot
        })
    }

    fn region_of(&mut self, address: u32) -> Option<(u32, &mut Region)> {
        self.regions
            .range_mut(..=address)
            .next_back()
            .filter(|(base, region)| address < region.end(**base))
            .map(|(base, region)| (*base, region))
    }
}

/// Reserves and commits `size` bytes of read-write private memory for the
/// platform, as NtAllocateVirtualMemory would.
pub fn allocate_committed(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    size: u32,
) -> Option<u32> {
    let size = align_up(size, PAGE);
    let base = find_free(vm, size, false)?;
    if !vm.reserve_at(base, size) {
        return None;
    }
    if cpu.map(base, size, Prot::RW).is_err() {
        let _ = vm.free(cpu, base);
        return None;
    }
    memory.insert(
        base,
        Kind::Private,
        PAGE_READWRITE,
        vec![Some(PAGE_READWRITE); (size / PAGE) as usize],
    );
    Some(base)
}

/// Maps the image of a Unix library, whose page `i` gets `protections[i]`,
/// high in the address space as a host loader would, and returns its base.
pub fn map_library(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    image: &[u8],
    protections: &[u32],
) -> Option<u32> {
    let size = protections.len() as u32 * PAGE;
    let base = find_free(vm, size, true)?;
    if !vm.reserve_at(base, size) {
        return None;
    }
    let mapped = cpu.map(base, size, Prot::RW).is_ok()
        && cpu.write(base, image).is_ok()
        && protections.iter().enumerate().all(|(page, &protection)| {
            memory
                .prot(protection)
                .is_ok_and(|prot| cpu.protect(base + page as u32 * PAGE, PAGE, prot).is_ok())
        });
    if !mapped {
        let _ = vm.free(cpu, base);
        return None;
    }
    memory.insert(
        base,
        Kind::Mapped,
        PAGE_EXECUTE_WRITECOPY,
        protections
            .iter()
            .map(|&protection| Some(protection))
            .collect(),
    );
    Some(base)
}

/// Records new protections for the pages of the region at `base`, which
/// the caller has applied.
pub fn set_protections(memory: &mut Memory, base: u32, protections: &[u32]) {
    if let Some(region) = memory.regions.get_mut(&base) {
        for (page, &protection) in region.pages.iter_mut().zip(protections) {
            *page = Some(protection);
        }
    }
}

/// Maps `bytes`, zero-filled to `size`, as a view of a data section with
/// `protection`, and returns its base.
pub fn map_view(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    bytes: &[u8],
    size: u32,
    protection: u32,
    base: u32,
) -> Result<u32, Status> {
    let prot = memory.prot(protection)?;
    let size = align_up(size, PAGE);
    let placeholder = replaced_placeholder(memory, base);
    let mapped = cpu.map(base, size, Prot::RW).is_ok()
        && cpu.write(base, bytes).is_ok()
        && cpu.protect(base, size, prot).is_ok();
    if !mapped {
        memory.regions.remove(&base);
        let _ = vm.free(cpu, base);
        return Err(status::NO_MEMORY);
    }
    memory.insert(
        base,
        Kind::Mapped,
        protection,
        vec![Some(protection); (size / PAGE) as usize],
    );
    if placeholder {
        memory.regions.get_mut(&base).unwrap().placeholder = Placeholder::Replaced;
    }
    Ok(base)
}

/// Maps a shared data section at an automatically chosen address.
pub fn map_shared_view(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    fd: std::os::fd::BorrowedFd<'_>,
    offset: u64,
    size: u32,
    protection: u32,
) -> Result<u32, Status> {
    memory.prot(protection)?;
    let size = align_up(size, PAGE);
    let base = find_free(vm, size, false).ok_or(status::NO_MEMORY)?;
    if !vm.reserve_at(base, size) {
        return Err(status::NO_MEMORY);
    }
    map_shared_view_at(cpu, vm, memory, fd, offset, protection, (base, size))
}

/// Maps `size` bytes of the file `fd` from `offset` as a view of a data
/// section with `protection`, read-only or read-write, and returns its
/// base. The view shares the file's pages with every other mapping of it,
/// such as the server's.
pub(crate) fn map_shared_view_at(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    fd: std::os::fd::BorrowedFd<'_>,
    offset: u64,
    protection: u32,
    (base, size): (u32, u32),
) -> Result<u32, Status> {
    // Data views never execute, whatever the image's DEP choice.
    let prot = Prot(memory.prot(protection)?.0 & !Prot::EXEC.0);
    let size = align_up(size, PAGE);
    let placeholder = replaced_placeholder(memory, base);
    if cpu.map_shared_file(base, size, fd, offset, prot).is_err() {
        memory.regions.remove(&base);
        let _ = vm.free(cpu, base);
        return Err(status::NO_MEMORY);
    }
    memory.insert(
        base,
        Kind::Mapped,
        protection,
        vec![Some(protection); (size / PAGE) as usize],
    );
    if placeholder {
        memory.regions.get_mut(&base).unwrap().placeholder = Placeholder::Replaced;
    }
    Ok(base)
}

/// Map an immutable provider's pages without reading untouched ranges.
pub(crate) fn map_source_view(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    source: &dyn crate::file::volume::ReadFile,
    offset: u64,
    protection: u32,
    (base, size): (u32, u32),
) -> Result<u32, Status> {
    // The backend's shared file pages are data pages, never executable views.
    if protection & 0xf0 != 0 {
        memory.regions.remove(&base);
        let _ = vm.free(cpu, base);
        return Err(status::NOT_SUPPORTED);
    }
    let prot = Prot(memory.prot(protection)?.0 & !Prot::EXEC.0);
    let size = align_up(size, PAGE);
    let placeholder = replaced_placeholder(memory, base);
    let pages: Vec<_> = (0..size / PAGE)
        .map(|index| source.page(offset / u64::from(PAGE) + u64::from(index)))
        .collect();
    let copy = matches!(protection, PAGE_WRITECOPY | PAGE_EXECUTE_WRITECOPY);
    if cpu.map_file_pages(base, &pages, prot, copy).is_err() {
        memory.regions.remove(&base);
        let _ = vm.free(cpu, base);
        return Err(status::NO_MEMORY);
    }
    memory.insert(
        base,
        Kind::Mapped,
        protection,
        vec![Some(protection); (size / PAGE) as usize],
    );
    memory.regions.get_mut(&base).unwrap().maximum_protection = Some(
        PAGE_NOACCESS
            | PAGE_READONLY
            | if copy {
                PAGE_READWRITE | PAGE_WRITECOPY
            } else {
                0
            },
    );
    if placeholder {
        memory.regions.get_mut(&base).unwrap().placeholder = Placeholder::Replaced;
    }
    Ok(base)
}

/// The base and kind of the region that holds `address`.
pub fn region(memory: &mut Memory, address: u32) -> Option<(u32, Kind)> {
    memory
        .region_of(address)
        .map(|(base, region)| (base, region.kind))
}

/// The base of the section view that holds `address`.
pub fn view_base(memory: &mut Memory, address: u32) -> Option<u32> {
    memory
        .region_of(address)
        .filter(|(_, region)| region.kind != Kind::Private)
        .map(|(base, _)| base)
}

/// Unmaps the view at `base`.
pub fn remove_view(cpu: &mut dyn Guest, vm: &mut Vm, memory: &mut Memory, base: u32) {
    memory.regions.remove(&base);
    let _ = vm.free(cpu, base);
}

fn memory<'a>(env: &'a mut Env<'_>) -> &'a mut Memory {
    env.ext.get_or_default::<Memory>()
}

fn read(env: &Env<'_>, address: u32) -> Result<u32, Status> {
    env.cpu
        .read_u32(address)
        .map_err(|_| status::ACCESS_VIOLATION)
}

fn write(env: &mut Env<'_>, address: u32, value: u32) -> Result<(), Status> {
    env.cpu
        .write_u32(address, value)
        .map_err(|_| status::ACCESS_VIOLATION)
}

/// The pages `[address, address + size)` touch.
fn pages(address: u32, size: u32) -> Result<(u32, u32), Status> {
    let start = address & !(PAGE - 1);
    let end = u64::from(address) + u64::from(size);
    let end = end.next_multiple_of(u64::from(PAGE));
    if end > u64::from(USER_END) {
        return Err(status::INVALID_PARAMETER);
    }
    Ok((start, end as u32 - start))
}

/// A free granule-aligned range of `size` bytes in the dynamic area.
pub fn find_free(vm: &Vm, size: u32, top_down: bool) -> Option<u32> {
    let size = align_up(size, GRANULE);
    let mut cursor = DYN_START;
    let mut found = None;
    while cursor < DYN_END {
        if let Some((base, extent)) = vm.containing(cursor) {
            let end = u64::from(base) + u64::from(extent);
            cursor = u32::try_from(end.next_multiple_of(u64::from(GRANULE))).ok()?;
            continue;
        }
        let end = vm.next_region(cursor).unwrap_or(DYN_END).min(DYN_END);
        if end - cursor >= size {
            found = Some(if top_down {
                (end - size) & !(GRANULE - 1)
            } else {
                cursor
            });
            if !top_down {
                break;
            }
        }
        cursor = end;
    }
    found
}

fn finish(result: Result<(), Status>) -> Outcome {
    status::finish(result.map_err(status::Error::from))
}

/// NtAllocateVirtualMemory(process, *base, zero_bits, *size, type, protection)
pub fn allocate(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != CURRENT_PROCESS {
        match crate::information::current_process(env, args[0]) {
            Ok(()) => {}
            Err(crate::status::Error::Status(status::NOT_SUPPORTED)) => {
                return crate::apc::memory(env, args, 3);
            }
            Err(error) => return status::finish(Err(error)),
        }
        let mut args = args.to_vec();
        args[0] = CURRENT_PROCESS;
        return allocate(env, &args);
    }
    finish((|| {
        let zero_bits = args[2];
        if read(env, args[3])? == 0 {
            return Err(status::INVALID_PARAMETER);
        }
        if zero_bits > 21 {
            return Err(0xc000_00f1);
        }
        if args[4] & !(MEM_COMMIT | MEM_RESERVE | MEM_RESET | MEM_TOP_DOWN | MEM_WRITE_WATCH) != 0 {
            return Err(status::INVALID_PARAMETER);
        }
        let high = if zero_bits == 0 || read(env, args[1])? != 0 {
            0
        } else {
            u32::MAX >> zero_bits
        };
        allocate_in(env, args, 0, high, GRANULE)
    })())
}

fn allocate_in(
    env: &mut Env<'_>,
    args: &[u32],
    low: u32,
    high: u32,
    alignment: u32,
) -> Result<(), Status> {
    let &[process, base_out, _zero_bits, size_out, kind, protection] = args else {
        unreachable!()
    };
    if process != CURRENT_PROCESS {
        return Err(status::INVALID_HANDLE);
    }
    if kind & (MEM_COMMIT | MEM_RESERVE | MEM_RESET) == 0
        || kind & MEM_REPLACE_PLACEHOLDER != 0 && kind & MEM_RESERVE == 0
        || kind & MEM_RESERVE_PLACEHOLDER != 0 && protection != PAGE_NOACCESS
    {
        return Err(status::INVALID_PARAMETER);
    }
    let address = read(env, base_out)?;
    let size = read(env, size_out)?;
    if size == 0 {
        return Err(status::INVALID_PARAMETER);
    }
    let prot = memory(env).prot(protection)?;
    if kind & MEM_RESET != 0 {
        // Contents may be discarded; keeping them is allowed.
        return Ok(());
    }

    let reserve = kind & MEM_RESERVE != 0 || address == 0;
    let (start, length) = if kind & MEM_REPLACE_PLACEHOLDER != 0 {
        let (start, length) = pages(address, size)?;
        let Some((base, region)) = memory(env).region_of(start) else {
            return Err(status::INVALID_PARAMETER);
        };
        if start != base || length != region.end(base) - base {
            return Err(status::CONFLICTING_ADDRESSES);
        }
        if region.placeholder != Placeholder::Free {
            return Err(status::INVALID_PARAMETER);
        }
        region.placeholder = Placeholder::Replaced;
        region.protection = protection;
        (start, length)
    } else if reserve {
        let (start, length) = if address == 0 {
            let length = align_up(size, PAGE);
            let start = if low == 0 && high == 0 && alignment == GRANULE {
                find_free(&env.sys.vm, length, kind & MEM_TOP_DOWN != 0)
            } else {
                find_free_range(
                    &env.sys.vm,
                    length,
                    kind & MEM_TOP_DOWN != 0,
                    low,
                    high,
                    alignment,
                )
            }
            .ok_or(status::NO_MEMORY)?;
            (start, length)
        } else {
            let start = address & !(GRANULE - 1);
            let (first, length) = pages(address, size)?;
            (start, first + length - start)
        };
        if start < GRANULE || !env.sys.vm.reserve_at(start, length) {
            return Err(status::CONFLICTING_ADDRESSES);
        }
        memory(env).insert(
            start,
            Kind::Private,
            protection,
            vec![None; (length / PAGE) as usize],
        );
        if kind & MEM_RESERVE_PLACEHOLDER != 0 {
            memory(env).regions.get_mut(&start).unwrap().placeholder = Placeholder::Free;
        }
        (start, length)
    } else {
        let (start, length) = pages(address, size)?;
        match memory(env).region_of(start) {
            Some((base, region))
                if region.kind == Kind::Private
                    && region.placeholder != Placeholder::Free
                    && start + length <= region.end(base) => {}
            _ => return Err(status::CONFLICTING_ADDRESSES),
        }
        (start, length)
    };

    if kind & MEM_WRITE_WATCH != 0 {
        let (_, region) = memory(env).region_of(start).expect("region");
        region.write_watch = Some(vec![false; region.pages.len()]);
    }
    if kind & MEM_COMMIT != 0 {
        let (base, region) = memory(env).region_of(start).expect("region");
        let first = ((start - base) / PAGE) as usize;
        let reserved: Vec<u32> = (0..(length / PAGE) as usize)
            .filter(|&page| region.pages[first + page].is_none())
            .map(|page| start + page as u32 * PAGE)
            .collect();
        for page in &mut region.pages[first..first + (length / PAGE) as usize] {
            *page = Some(protection);
        }
        for page in reserved {
            if env.cpu.map(page, PAGE, prot).is_err() {
                return Err(status::NO_MEMORY);
            }
        }
        // Committing again keeps the contents and applies the protection.
        env.cpu
            .protect(start, length, prot)
            .map_err(|_| status::NO_MEMORY)?;
    }
    apply_write_watch(env, start, length)?;
    write(env, base_out, start)?;
    write(env, size_out, length)
}

/// NtFreeVirtualMemory(process, *base, *size, type)
pub fn free(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != CURRENT_PROCESS {
        match crate::information::current_process(env, args[0]) {
            Ok(()) => {}
            Err(crate::status::Error::Status(status::NOT_SUPPORTED)) => {
                return crate::apc::memory(env, args, 5);
            }
            Err(error) => return status::finish(Err(error)),
        }
        let mut args = args.to_vec();
        args[0] = CURRENT_PROCESS;
        return free(env, &args);
    }
    finish(free_in(env, args))
}

fn free_in(env: &mut Env<'_>, args: &[u32]) -> Result<(), Status> {
    let &[process, base_out, size_out, kind] = args else {
        unreachable!()
    };
    if process != CURRENT_PROCESS {
        return Err(status::INVALID_HANDLE);
    }
    let address = read(env, base_out)?;
    let size = read(env, size_out)?;
    let (base, end) = match memory(env).region_of(address) {
        Some((base, region)) if region.kind == Kind::Private => (base, region.end(base)),
        _ => return Err(status::MEMORY_NOT_ALLOCATED),
    };
    let first = address & !(PAGE - 1);
    if size == 0 && first != base {
        return Err(status::FREE_VM_NOT_AT_BASE);
    }
    match kind {
        kind if kind == MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER => {
            let (start, length) = pages(address, size)?;
            if size == 0 {
                return Err(0xc000_00f1);
            }
            let (_, region) = memory(env).region_of(base).unwrap();
            if region.placeholder == Placeholder::None
                || region.placeholder == Placeholder::Free && length == end - base
            {
                return Err(status::CONFLICTING_ADDRESSES);
            }
            if start != base || length != end - base {
                return Err(status::CONFLICTING_ADDRESSES);
            }
            region.placeholder = Placeholder::Free;
            region.protection = PAGE_NOACCESS;
            region.pages.fill(None);
            for page in (base..end).step_by(PAGE as usize) {
                let _ = env.cpu.unmap(page, PAGE);
            }
            write(env, base_out, start)?;
            write(env, size_out, length)
        }
        MEM_RELEASE => {
            if size != 0 {
                let (start, length) = pages(address, size)?;
                if start != base || start + length != end {
                    return Err(status::UNABLE_TO_FREE_VM);
                }
            }
            memory(env).regions.remove(&base);
            let _ = env.sys.vm.free(env.cpu, base);
            write(env, base_out, base)?;
            write(env, size_out, end - base)
        }
        MEM_DECOMMIT => {
            let (start, length) = if size == 0 {
                (address & !(PAGE - 1), end - (address & !(PAGE - 1)))
            } else {
                pages(address, size)?
            };
            if start + length > end {
                return Err(status::UNABLE_TO_FREE_VM);
            }
            let (_, region) = memory(env).region_of(start).expect("region");
            let first = ((start - base) / PAGE) as usize;
            for page in &mut region.pages[first..first + (length / PAGE) as usize] {
                *page = None;
            }
            for page in (start..start + length).step_by(PAGE as usize) {
                let _ = env.cpu.unmap(page, PAGE);
            }
            write(env, base_out, start)?;
            write(env, size_out, if size == 0 { 0 } else { length })
        }
        _ => Err(status::INVALID_PARAMETER),
    }
}

/// NtProtectVirtualMemory(process, *base, *size, protection, *old)
pub fn protect(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != CURRENT_PROCESS {
        match crate::information::current_process(env, args[0]) {
            Ok(()) => {}
            Err(crate::status::Error::Status(status::NOT_SUPPORTED)) => {
                return crate::apc::memory(env, args, 7);
            }
            Err(error) => return status::finish(Err(error)),
        }
        let mut args = args.to_vec();
        args[0] = CURRENT_PROCESS;
        return protect(env, &args);
    }
    finish(protect_in(env, args))
}

fn protect_in(env: &mut Env<'_>, args: &[u32]) -> Result<(), Status> {
    let &[process, base_out, size_out, protection, old_out] = args else {
        unreachable!()
    };
    if process != CURRENT_PROCESS {
        return Err(status::INVALID_HANDLE);
    }
    let address = read(env, base_out)?;
    let size = read(env, size_out)?;
    let (start, length) = pages(address, size)?;
    let mut prot = memory(env).prot(protection)?;
    let Some((base, region)) = memory(env).region_of(start) else {
        return Err(status::NOT_MAPPED_VIEW);
    };
    if start + length > region.end(base) {
        return Err(status::NOT_MAPPED_VIEW);
    }
    if let Some(maximum) = region.maximum_protection {
        if protection & 0xff & !maximum != 0 {
            return Err(0xc000_004e);
        }
        prot.0 &= !Prot::EXEC.0;
    }
    let first = ((start - base) / PAGE) as usize;
    let range = first..first + (length / PAGE) as usize;
    let Some(old) = region.pages[first] else {
        return Err(status::NOT_COMMITTED);
    };
    if region.pages[range.clone()].iter().any(Option::is_none) {
        return Err(status::NOT_COMMITTED);
    }
    region.pages[range].fill(Some(protection));
    env.cpu
        .protect(start, length, prot)
        .map_err(|_| status::NOT_COMMITTED)?;
    apply_write_watch(env, start, length)?;
    write(env, old_out, old)?;
    write(env, base_out, start)?;
    write(env, size_out, length)
}

/// NtQueryVirtualMemory(process, address, class, *buffer, length, *returned)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != CURRENT_PROCESS {
        match crate::information::current_process(env, args[0]) {
            Ok(()) => {}
            Err(crate::status::Error::Status(status::NOT_SUPPORTED)) => {
                return crate::apc::memory(env, args, 6);
            }
            Err(error) => return status::finish(Err(error)),
        }
        let mut args = args.to_vec();
        args[0] = CURRENT_PROCESS;
        return query(env, &args);
    }
    let &[process, address, class, buffer, length, _] = args else {
        unreachable!()
    };
    if process == CURRENT_PROCESS
        && matches!(
            class,
            unixlib::MEMORY_WINE_UNIX_FUNCS | unixlib::MEMORY_WINE_UNIX_WOW64_FUNCS
        )
    {
        return unixlib::query(env, address, class, buffer, length);
    }
    finish(query_in(env, args))
}

fn query_in(env: &mut Env<'_>, args: &[u32]) -> Result<(), Status> {
    let &[process, address, class, buffer, length, returned] = args else {
        unreachable!()
    };
    if process != CURRENT_PROCESS {
        return Err(status::INVALID_HANDLE);
    }
    if class != BASIC_INFORMATION {
        return Err(status::INVALID_INFO_CLASS);
    }
    if length < BASIC_INFORMATION_SIZE {
        return Err(status::INFO_LENGTH_MISMATCH);
    }
    if address >= USER_END {
        return Err(status::INVALID_PARAMETER);
    }
    let info = basic_information(env, address & !(PAGE - 1));
    for (index, value) in info.into_iter().enumerate() {
        write(env, buffer + 4 * index as u32, value)?;
    }
    if returned != 0 {
        write(env, returned, BASIC_INFORMATION_SIZE)?;
    }
    Ok(())
}

/// MEMORY_BASIC_INFORMATION of the run of like pages from `page`.
fn basic_information(env: &mut Env<'_>, page: u32) -> [u32; 7] {
    let vm_region = env.sys.vm.containing(page);
    if let Some((base, region)) = memory(env).region_of(page) {
        let first = ((page - base) / PAGE) as usize;
        let state = region.pages[first];
        let run = region.pages[first..]
            .iter()
            .take_while(|&&other| other == state)
            .count() as u32;
        return [
            page,
            base,
            region.protection,
            run * PAGE,
            if state.is_some() {
                MEM_COMMIT
            } else {
                MEM_RESERVE
            },
            state.unwrap_or(0),
            region.kind as u32,
        ];
    }
    if let Some((base, size)) = vm_region {
        let end = (u64::from(base) + u64::from(size)).min(u64::from(USER_END)) as u32;
        return [
            page,
            base,
            PAGE_NOACCESS,
            end - page,
            MEM_RESERVE,
            0,
            Kind::Private as u32,
        ];
    }
    let end = env
        .sys
        .vm
        .next_region(page)
        .unwrap_or(USER_END)
        .min(USER_END);
    [page, 0, 0, end - page, MEM_FREE, PAGE_NOACCESS, 0]
}

/// The protection a mapped page of an image section gets.
pub fn section_protection(characteristics: u32) -> u32 {
    const EXECUTE: u32 = 0x2000_0000;
    const READ: u32 = 0x4000_0000;
    const WRITE: u32 = 0x8000_0000;
    match (
        characteristics & EXECUTE != 0,
        characteristics & READ != 0,
        characteristics & WRITE != 0,
    ) {
        (false, false, false) => PAGE_NOACCESS,
        (false, _, false) => PAGE_READONLY,
        (false, _, true) => PAGE_WRITECOPY,
        (true, false, false) => PAGE_EXECUTE,
        (true, _, false) => PAGE_EXECUTE_READ,
        (true, _, true) => PAGE_EXECUTE_WRITECOPY,
    }
}

/// The guest protection for an NT protection value.
pub fn guest_protection(memory: &Memory, protection: u32) -> Prot {
    memory.prot(protection).unwrap_or(Prot(0))
}

/// NtFlushInstructionCache(process, address, size). Every guest store already
/// commits through the CPU's code-mutation barrier before execution resumes.
pub fn flush_instruction_cache(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    match crate::information::current_process(env, args[0]) {
        Err(crate::status::Error::Status(status::NOT_SUPPORTED)) => {
            Outcome::Return(status::SUCCESS)
        }
        result => status::finish(result),
    }
}

/// NtAllocateVirtualMemoryEx(process, *base, *size, type, protection, *parameters, count)
pub fn allocate_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != CURRENT_PROCESS {
        if let Err(error) = crate::information::current_process(env, args[0]) {
            return status::finish(Err(error));
        }
        let mut args = args.to_vec();
        args[0] = CURRENT_PROCESS;
        return allocate_ex(env, &args);
    }
    let &[process, base, size, kind, protection, parameters, count] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut present = 0u32;
        let (mut low, mut high, mut alignment) = (0, 0, GRANULE);
        let mut attributes = 0;
        let mut constrained = false;
        if count != 0 && parameters == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        for index in 0..count {
            let param = parameters + index * 16;
            let typ = read(env, param)? & 0xff;
            if typ >= 32 || present & (1 << typ) != 0 {
                return Err(status::INVALID_PARAMETER.into());
            }
            present |= 1 << typ;
            let value = read(env, param + 8)?;
            match typ {
                1 => {
                    low = read(env, value)?;
                    high = read(env, value + 4)?;
                    let align = read(env, value + 8)?;
                    constrained = low != 0 || high != 0 || align != 0;
                    if align != 0 {
                        if !align.is_power_of_two() || align < GRANULE {
                            return Err(status::INVALID_PARAMETER.into());
                        }
                        alignment = align;
                    }
                    if low != 0 && (low >= USER_END || low & (GRANULE - 1) != 0) {
                        return Err(status::INVALID_PARAMETER.into());
                    }
                    if high != 0
                        && (high > USER_END
                            || high <= low
                            || high.wrapping_add(1) & (PAGE - 2) != 0)
                    {
                        return Err(status::INVALID_PARAMETER.into());
                    }
                }
                5 => attributes = value,
                2 | 3 | 4 | 6 => (),
                _ => return Err(status::INVALID_PARAMETER.into()),
            }
        }
        let known = MEM_COMMIT
            | MEM_RESERVE
            | MEM_TOP_DOWN
            | MEM_WRITE_WATCH
            | MEM_RESET
            | MEM_RESERVE_PLACEHOLDER
            | MEM_REPLACE_PLACEHOLDER;
        if kind & !known != 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read(env, base)? != 0 && constrained {
            return Err(status::INVALID_PARAMETER.into());
        }
        if read(env, size)? == 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if attributes & 0x40 != 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        allocate_in(
            env,
            &[process, base, 0, size, kind, protection],
            low,
            high,
            alignment,
        )
        .map_err(Into::into)
    })())
}

fn find_free_range(
    vm: &Vm,
    size: u32,
    top_down: bool,
    low: u32,
    high: u32,
    alignment: u32,
) -> Option<u32> {
    let end_limit = if high == 0 {
        USER_END
    } else {
        (high + 1).min(USER_END)
    };
    let mut cursor = u64::from(low.max(GRANULE)).next_multiple_of(u64::from(alignment));
    let mut found = None;
    while cursor + u64::from(size) <= u64::from(end_limit) {
        let address = cursor as u32;
        if let Some((base, extent)) = vm.containing(address) {
            cursor = (u64::from(base) + u64::from(extent)).next_multiple_of(u64::from(alignment));
            continue;
        }
        let end = vm.next_region(address).unwrap_or(end_limit).min(end_limit);
        if u64::from(end) - cursor >= u64::from(size) {
            found = Some(if top_down {
                (end - size) & !(alignment - 1)
            } else {
                address
            });
            if !top_down {
                break;
            }
        }
        cursor = u64::from(end).next_multiple_of(u64::from(alignment));
    }
    found
}

/// The process's reserved and committed guest memory, in bytes.
pub fn usage(env: &mut Env<'_>) -> (u32, u32) {
    let memory = memory(env);
    let reserved = memory
        .regions
        .values()
        .map(|region| region.pages.len() as u32 * PAGE)
        .sum();
    let committed = memory
        .regions
        .values()
        .map(|region| region.pages.iter().filter(|page| page.is_some()).count() as u32 * PAGE)
        .sum();
    (reserved, committed)
}

/// Reapply Wine's write-watch protection after a commit, protection change or reset.
fn apply_write_watch(env: &mut Env<'_>, start: u32, length: u32) -> Result<(), Status> {
    for address in (start..start + length).step_by(PAGE as usize) {
        let memory = memory(env);
        let Some((base, region)) = memory.region_of(address) else {
            continue;
        };
        let index = ((address - base) / PAGE) as usize;
        let Some(watches) = &region.write_watch else {
            continue;
        };
        if watches[index] {
            continue;
        }
        let Some(protection) = region.pages[index] else {
            continue;
        };
        let prot = memory.prot(protection)?;
        env.cpu
            .protect(address, PAGE, Prot(prot.0 & !Prot::WRITE.0))
            .map_err(|_| status::NOT_COMMITTED)?;
    }
    Ok(())
}

/// Resolve a first write to a watched page, leaving read-only and guard faults intact.
pub fn write_watch_fault(env: &mut Env<'_>, address: u32) -> bool {
    let memory = memory(env);
    let Some((base, region)) = memory.region_of(address) else {
        return false;
    };
    let index = ((address - base) / PAGE) as usize;
    let Some(protection) = region.pages[index] else {
        return false;
    };
    let Some(watches) = &mut region.write_watch else {
        return false;
    };
    if watches[index] {
        return false;
    }
    let Ok(prot) = memory.prot(protection) else {
        return false;
    };
    if !prot.contains(Prot::WRITE) {
        return false;
    }
    let (_, region) = memory.region_of(address).unwrap();
    region.write_watch.as_mut().unwrap()[index] = true;
    env.cpu.protect(address & !(PAGE - 1), PAGE, prot).is_ok()
}

/// NtGetWriteWatch(process, flags, base, size, *addresses, *count, *granularity)
pub fn get_write_watch(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    finish((|| {
        let &[_, flags, address, size, addresses, count, granularity] = args else {
            unreachable!()
        };
        if count == 0 || granularity == 0 {
            return Err(status::ACCESS_VIOLATION);
        }
        let capacity = read(env, count)?;
        if capacity == 0 || size == 0 || flags & !1 != 0 {
            return Err(status::INVALID_PARAMETER);
        }
        if addresses == 0 {
            return Err(status::ACCESS_VIOLATION);
        }
        let (start, length) = pages(address, size)?;
        let Some((base, region)) = memory(env).region_of(start) else {
            return Err(status::INVALID_PARAMETER);
        };
        if start + length > region.end(base) {
            return Err(status::INVALID_PARAMETER);
        }
        let Some(watches) = &mut region.write_watch else {
            return Err(status::INVALID_PARAMETER);
        };
        let first = ((start - base) / PAGE) as usize;
        let mut found = Vec::new();
        let mut scanned = 0;
        for (index, watched) in watches
            .iter_mut()
            .enumerate()
            .skip(first)
            .take((length / PAGE) as usize)
        {
            scanned += 1;
            if *watched {
                found.push(base + index as u32 * PAGE);
                if flags & 1 != 0 {
                    *watched = false;
                }
                if found.len() == capacity as usize {
                    break;
                }
            }
        }
        if flags & 1 != 0 {
            apply_write_watch(env, start, scanned * PAGE)?;
        }
        for (index, address) in found.iter().enumerate() {
            write(env, addresses + index as u32 * 4, *address)?;
        }
        write(env, count, found.len() as u32)?;
        write(env, granularity, PAGE)
    })())
}

/// NtResetWriteWatch(process, base, size)
pub fn reset_write_watch(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    finish((|| {
        if args[2] == 0 {
            return Err(status::INVALID_PARAMETER);
        }
        let (start, length) = pages(args[1], args[2])?;
        let Some((base, region)) = memory(env).region_of(start) else {
            return Err(status::INVALID_PARAMETER);
        };
        if start + length > region.end(base) {
            return Err(status::INVALID_PARAMETER);
        }
        let Some(watches) = &mut region.write_watch else {
            return Err(status::INVALID_PARAMETER);
        };
        let first = ((start - base) / PAGE) as usize;
        watches[first..first + (length / PAGE) as usize].fill(false);
        apply_write_watch(env, start, length)
    })())
}

/// NtReadVirtualMemory(process, source, destination, size, *read)
pub fn read_virtual(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, source, destination, size, returned] = args else {
        unreachable!()
    };
    let result = (|| {
        if let Err(error) = crate::information::current_process(env, process) {
            if !matches!(error, crate::status::Error::Status(status::NOT_SUPPORTED)) {
                return Err(error);
            }
            let mut bytes = vec![0; size as usize];
            let mut request = crate::protocol::ReadProcessMemoryRequest {
                handle: process,
                addr: u64::from(source),
                ..Default::default()
            };
            let (reply, count) = crate::server::request(env, &mut request, &[], &mut bytes)?;
            status::check(reply.header.error)?;
            crate::object::write_bytes(env, destination, &bytes[..count])?;
            return Ok(());
        }
        if !env.cpu.accessible(destination, size, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let mut bytes = vec![0; size as usize];
        if !env.cpu.accessible(source, size, Prot::READ)
            || env.cpu.read(source, &mut bytes).is_err()
        {
            return Err(0x8000_000du32.into());
        }
        crate::object::write_bytes(env, destination, &bytes)?;
        Ok(())
    })();
    if returned != 0
        && let Err(error) = write(env, returned, if result.is_ok() { size } else { 0 })
    {
        return Outcome::Return(error);
    }
    status::finish(result)
}

/// NtWriteVirtualMemory(process, destination, source, size, *written)
pub fn write_virtual(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, destination, source, size, returned] = args else {
        unreachable!()
    };
    let result = (|| {
        if let Err(error) = crate::information::current_process(env, process) {
            if !matches!(error, crate::status::Error::Status(status::NOT_SUPPORTED)) {
                return Err(error);
            }
            let mut bytes = vec![0; size as usize];
            env.cpu
                .read(source, &mut bytes)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            let mut request = crate::protocol::WriteProcessMemoryRequest {
                handle: process,
                addr: u64::from(destination),
                ..Default::default()
            };
            let (reply, _) = crate::server::request(env, &mut request, &[&bytes], &mut [])?;
            status::check(reply.header.error)?;
            return Ok(());
        }
        let mut bytes = vec![0; size as usize];
        if !env.cpu.accessible(source, size, Prot::READ)
            || env.cpu.read(source, &mut bytes).is_err()
        {
            return Err(0x8000_000du32.into());
        }
        if !env.cpu.accessible(destination, size, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        crate::object::write_bytes(env, destination, &bytes)?;
        Ok(())
    })();
    if returned != 0
        && let Err(error) = write(env, returned, if result.is_ok() { size } else { 0 })
    {
        return Outcome::Return(error);
    }
    status::finish(result)
}

/// Wine's virtual_set_force_exec reapplies protections to existing views.
pub fn set_execute(env: &mut Env<'_>, enabled: bool) -> Result<(), Status> {
    let memory = memory(env);
    memory.execute_readable = enabled;
    let mut pages = Vec::new();
    for (&base, region) in &memory.regions {
        for (index, page) in region.pages.iter().enumerate() {
            if let Some(protection) = page {
                if protection & PAGE_GUARD != 0
                    || matches!(
                        *protection & 0xff,
                        PAGE_NOACCESS
                            | PAGE_EXECUTE
                            | PAGE_EXECUTE_READ
                            | PAGE_EXECUTE_READWRITE
                            | PAGE_EXECUTE_WRITECOPY
                    )
                {
                    continue;
                }
                let mut prot = memory.prot(*protection)?;
                // Data-file views retain their non-executable mapping contract.
                if region.kind == Kind::Mapped
                    && matches!(
                        *protection & 0xff,
                        PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY
                    )
                {
                    prot.0 &= !Prot::EXEC.0;
                }
                pages.push((base + index as u32 * PAGE, prot));
            }
        }
    }
    for (address, prot) in pages {
        env.cpu
            .protect(address, PAGE, prot)
            .map_err(|_| status::NOT_COMMITTED)?;
        apply_write_watch(env, address, PAGE)?;
    }
    Ok(())
}

/// Wine 11.0's NtCreatePagingFile stub.
pub fn create_paging_file(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}

/// Wine returns STATUS_NOT_IMPLEMENTED without an LDT; the CPU interface
/// has no operation for installing LDT descriptors.
pub fn set_ldt_entries(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// NtFlushVirtualMemory(process, *base, *size, unknown)
pub fn flush_virtual(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != u32::MAX
        && matches!(
            crate::information::current_process(env, args[0]),
            Err(crate::status::Error::Status(status::NOT_SUPPORTED))
        )
    {
        return crate::apc::memory(env, args, 8);
    }
    status::finish((|| {
        let address = read(env, args[1])? & !(PAGE - 1);
        let size = read(env, args[2])?;
        crate::information::current_process(env, args[0])?;
        let (base, region) = memory(env)
            .region_of(address)
            .ok_or(status::INVALID_PARAMETER)?;
        if u64::from(address) + u64::from(size) > u64::from(region.end(base)) {
            return Err(status::INVALID_PARAMETER.into());
        }
        if size == 0 {
            let size = region.end(base) - base;
            write(env, args[2], size)?;
        }
        write(env, args[1], address)?;
        // Views share the server file's pages, so there is nothing to
        // write back: Wine's MS_ASYNC msync only schedules writeback.
        Ok(())
    })())
}

/// NtLockVirtualMemory(process, *base, *size, unknown)
pub fn lock_virtual(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != u32::MAX
        && matches!(
            crate::information::current_process(env, args[0]),
            Err(crate::status::Error::Status(status::NOT_SUPPORTED))
        )
    {
        return crate::apc::memory(env, args, 9);
    }
    pin_virtual(env, args, true)
}

/// NtUnlockVirtualMemory(process, *base, *size, unknown)
pub fn unlock_virtual(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] != u32::MAX
        && matches!(
            crate::information::current_process(env, args[0]),
            Err(crate::status::Error::Status(status::NOT_SUPPORTED))
        )
    {
        return crate::apc::memory(env, args, 10);
    }
    pin_virtual(env, args, false)
}

fn pin_virtual(env: &mut Env<'_>, args: &[u32], lock: bool) -> Outcome {
    status::finish((|| {
        let address = read(env, args[1])?;
        let size = read(env, args[2])?;
        crate::information::current_process(env, args[0])?;
        let length =
            (u64::from(address & (PAGE - 1)) + u64::from(size)).next_multiple_of(u64::from(PAGE));
        let start = address & !(PAGE - 1);
        write(env, args[2], length as u32)?;
        write(env, args[1], start)?;
        if lock
            && !(0..length).step_by(PAGE as usize).all(|offset| {
                let page = start.wrapping_add(offset as u32);
                [Prot::READ, Prot::WRITE, Prot::EXEC]
                    .into_iter()
                    .any(|prot| env.cpu.accessible(page, PAGE, prot))
            })
        {
            return Err(0xc000_0022u32.into());
        }
        // Guest memory is never paged out from under the guest, so the pin
        // itself has no host counterpart.
        Ok(())
    })())
}

/// NtSetInformationVirtualMemory: Wine prefetches ranges, with dirty-state
/// tracking unsupported when write exceptions are disabled.
pub fn set_information(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        match args[1] {
            0 => {
                if args[4] == 0 {
                    return Err(0xc000_00f3u32.into());
                }
                if args[5] != 4 {
                    return Err(0xc000_00f4u32.into());
                }
                if args[2] == 0 {
                    return Err(0xc000_00f1u32.into());
                }
                let _flags = read(env, args[4])?;
                for index in 0..args[2] {
                    if read(env, args[3] + index * 8 + 4)? == 0 {
                        return Err(0xc000_00f2u32.into());
                    }
                }
                // Prefetching is only a hint; guest pages are host memory.
                Ok(())
            }
            3 => Err(status::NOT_SUPPORTED.into()),
            _ => Err(0xc000_00f0u32.into()),
        }
    })())
}

/// Guest threads share one execution owner; a fence completes Wine's barrier.
pub fn flush_write_buffers(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    Outcome::Return(status::SUCCESS)
}

/// NtWow64AllocateVirtualMemory64(process, *base64, zero_bits64, *size64, type, protect)
pub fn allocate64(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let size = u64::from(read(env, args[4])?) | u64::from(read(env, args[4] + 4)?) << 32;
        let zero_bits = u64::from(args[2]) | u64::from(args[3]) << 32;
        if size == 0 {
            return Err(0xc000_00f2u32.into());
        }
        if zero_bits > 21 && zero_bits < 32 {
            return Err(0xc000_00f1u32.into());
        }
        let base = u64::from(read(env, args[1])?) | u64::from(read(env, args[1] + 4)?) << 32;
        crate::information::current_process(env, args[0])?;
        let base = u32::try_from(base).map_err(|_| status::CONFLICTING_ADDRESSES)?;
        let size = u32::try_from(size).map_err(|_| 0xc000_00a1u32)?;
        let scratch = env.cpu.reg(adv_cpu::backend::Reg::Esp) - 8;
        write(env, scratch, base)?;
        write(env, scratch + 4, size)?;
        match allocate(
            env,
            &[
                CURRENT_PROCESS,
                scratch,
                zero_bits as u32,
                scratch + 4,
                args[5],
                args[6],
            ],
        ) {
            Outcome::Return(0) => {
                let base = u64::from(read(env, scratch)?);
                let size = u64::from(read(env, scratch + 4)?);
                crate::object::write_bytes(env, args[1], &base.to_le_bytes())?;
                crate::object::write_bytes(env, args[4], &size.to_le_bytes())?;
                Ok(())
            }
            Outcome::Return(code) => Err(code.into()),
            _ => unreachable!(),
        }
    })())
}

/// NtWow64ReadVirtualMemory64 always uses Wine's read_process_memory request.
pub fn read_virtual64(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let size = (u64::from(args[4]) | u64::from(args[5]) << 32).min(i32::MAX as u64) as u32;
    let result = (|| {
        if size != 0 && !env.cpu.accessible(args[3], size, Prot::WRITE) {
            return Err(status::ACCESS_VIOLATION.into());
        }
        let mut data = vec![0; size as usize];
        let mut request = crate::protocol::ReadProcessMemoryRequest {
            handle: args[0],
            addr: u64::from(args[1]) | u64::from(args[2]) << 32,
            ..Default::default()
        };
        let (reply, count) = crate::server::request(env, &mut request, &[], &mut data)?;
        status::check(reply.header.error)?;
        if count != 0 {
            crate::object::write_bytes(env, args[3], &data[..count])?;
        }
        Ok(())
    })();
    if args[6] != 0 {
        let count = if result.is_ok() { u64::from(size) } else { 0 };
        if let Err(error) = crate::object::write_bytes(env, args[6], &count.to_le_bytes()) {
            return Outcome::Return(error);
        }
    }
    status::finish(result)
}

/// NtWow64WriteVirtualMemory64 always uses Wine's write_process_memory request.
pub fn write_virtual64(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let size = (u64::from(args[4]) | u64::from(args[5]) << 32).min(i32::MAX as u64) as u32;
    let result = (|| {
        if size != 0 && !env.cpu.accessible(args[3], size, Prot::READ) {
            return Err(0x8000_000du32.into());
        }
        let mut data = vec![0; size as usize];
        if size != 0 {
            env.cpu
                .read(args[3], &mut data)
                .map_err(|_| 0x8000_000du32)?;
        }
        let mut request = crate::protocol::WriteProcessMemoryRequest {
            handle: args[0],
            addr: u64::from(args[1]) | u64::from(args[2]) << 32,
            ..Default::default()
        };
        let (reply, _) = crate::server::request(env, &mut request, &[&data], &mut [])?;
        status::check(reply.header.error)
    })();
    if args[6] != 0 {
        let count = if result.is_ok() { u64::from(size) } else { 0 };
        if let Err(error) = crate::object::write_bytes(env, args[6], &count.to_le_bytes()) {
            return Outcome::Return(error);
        }
    }
    status::finish(result)
}

/// Reserves a section view, or replaces an exact free placeholder, as
/// Wine's map_view does for NtMapViewOfSectionEx.
pub(crate) fn reserve_section(
    env: &mut Env<'_>,
    address: u32,
    size: u32,
    kind: u32,
    low: u32,
    high: u32,
) -> Result<u32, Status> {
    if kind & MEM_REPLACE_PLACEHOLDER != 0 {
        let (base, region) = memory(env)
            .region_of(address)
            .ok_or(status::INVALID_PARAMETER)?;
        if address != base || size != region.end(base) - base {
            return Err(status::CONFLICTING_ADDRESSES);
        }
        if region.placeholder != Placeholder::Free {
            return Err(status::INVALID_PARAMETER);
        }
        region.placeholder = Placeholder::Replaced;
        return Ok(base);
    }
    let base = if address == 0 {
        if low == 0 && high == 0 {
            find_free(&env.sys.vm, size, kind & MEM_TOP_DOWN != 0)
        } else {
            find_free_range(
                &env.sys.vm,
                size,
                kind & MEM_TOP_DOWN != 0,
                low,
                high,
                GRANULE,
            )
        }
        .ok_or(status::NO_MEMORY)?
    } else {
        if u64::from(address) + u64::from(size) > u64::from(USER_END) {
            return Err(0xc000_00a1);
        }
        address
    };
    if !env.sys.vm.reserve_at(base, size) {
        return Err(status::CONFLICTING_ADDRESSES);
    }
    Ok(base)
}

/// Whether a mapped view replaced a placeholder.
pub(crate) fn replaced_placeholder(memory: &mut Memory, base: u32) -> bool {
    memory
        .region_of(base)
        .is_some_and(|(_, region)| region.placeholder == Placeholder::Replaced)
}

/// Unmaps a section while restoring the reservation it replaced.
pub(crate) fn preserve_view(env: &mut Env<'_>, base: u32) {
    let (_, region) = memory(env).region_of(base).expect("section view");
    let size = region.end(base) - base;
    region.kind = Kind::Private;
    region.protection = PAGE_NOACCESS;
    region.placeholder = Placeholder::Free;
    region.pages.fill(None);
    let _ = env.cpu.unmap(base, size);
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use adv_cpu::{Cpu, backend::FilePage};
    use std::{cell::Cell, rc::Rc};

    struct Source(Vec<Rc<FilePage>>);
    impl crate::file::volume::ReadFile for Source {
        fn read_at(&self, _: u64, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("mapping must not read a whole file");
        }
        fn page(&self, index: u64) -> Rc<FilePage> {
            self.0[index as usize].clone()
        }
    }

    // spec: DEV-DISC-5, DEV-DISC-7
    #[test]
    fn immutable_volume_views_load_only_touched_pages_and_copy_writes() {
        let loads = Rc::new(Cell::new(0));
        let source = Source(
            (0..32)
                .map(|index| {
                    let loads = loads.clone();
                    FilePage::lazy(move || {
                        loads.set(loads.get() + 1);
                        Ok([index as u8 + 1; 4096])
                    })
                })
                .collect(),
        );
        let mut cpu = Cpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        let mut vm = Vm::new();
        let mut memory = Memory::default();
        let size = 32 * PAGE;
        let first = vm.reserve(size).unwrap();
        let second = vm.reserve(size).unwrap();
        map_source_view(
            &mut cpu,
            &mut vm,
            &mut memory,
            &source,
            0,
            PAGE_READONLY,
            (first, size),
        )
        .unwrap();
        map_source_view(
            &mut cpu,
            &mut vm,
            &mut memory,
            &source,
            0,
            PAGE_WRITECOPY,
            (second, size),
        )
        .unwrap();
        assert_eq!(loads.get(), 0);
        let mut byte = [0];
        cpu.read(first, &mut byte).unwrap();
        assert_eq!(byte, [1]);
        assert_eq!(loads.get(), 1);
        cpu.write(second, &[99]).unwrap();
        cpu.read(first, &mut byte).unwrap();
        assert_eq!(byte, [1]);
        cpu.read(second, &mut byte).unwrap();
        assert_eq!(byte, [99]);
        cpu.read(first + 31 * PAGE, &mut byte).unwrap();
        assert_eq!(byte, [32]);
        assert_eq!(loads.get(), 2);
    }
}
