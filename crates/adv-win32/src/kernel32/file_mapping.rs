//! Worker-owned data mappings. Views retain their section after handles close.
//! Executable/image/reserved/device mappings and cross-host-process sharing are
//! explicitly outside this implementation; no native OS named object is created.
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::rc::{Rc, Weak};

use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Object, Outcome};
use adv_os::vfs::{FileIdentity, FileSection, file_identity as identity};

use super::K32;
use crate::{FileObj, Win32, stdcall, util::set_last_error};

const PAGE: u64 = 4096;
const GRANULE: u64 = 65536;
const READ: u32 = 4;
const WRITE: u32 = 2;
const COPY: u32 = 1;
const ALL: u32 = 0xf001f;
const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;

#[derive(Default)]
struct Mappings {
    names: HashMap<Vec<u16>, Named>,
    views: BTreeMap<u32, View>,
}
/// A section object in the handle table. Its section counts open section objects, so a
/// name stays bound while any exists.
struct SectionHandle {
    section: Rc<Section>,
    access: u32,
}
impl SectionHandle {
    fn new(section: Rc<Section>, access: u32) -> Self {
        section.handles.set(section.handles.get() + 1);
        Self { section, access }
    }
}
impl Drop for SectionHandle {
    fn drop(&mut self) {
        self.section.handles.set(self.section.handles.get() - 1);
    }
}
enum Named {
    Mapping(Weak<Section>),
    Sync { identity: u32, kind: u32 },
}
impl Mappings {
    fn prune(&mut self, objects: &adv_machine::Objects) {
        self.names.retain(|_, name| match name {
            Named::Mapping(section) => section
                .upgrade()
                .is_some_and(|section| section.handles.get() != 0),
            Named::Sync { identity, .. } => objects.has_identity(*identity),
        });
    }
}
struct Section {
    backing: Rc<FileSection>,
    len: u64,
    protection: u32,
    handles: Cell<u32>,
}
struct View {
    section: Rc<Section>,
    offset: u64,
    len: u32,
    copy: bool,
    maximum: u32,
    protections: Vec<u32>,
}

fn state<'a>(env: &'a mut Env<'_>) -> &'a mut Mappings {
    if env.ext.try_get::<Mappings>().is_none() {
        env.ext.insert(Mappings::default());
    }
    env.ext.get_mut::<Mappings>()
}
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn io_error(error: &std::io::Error) -> u32 {
    match error.kind() {
        std::io::ErrorKind::NotFound => 2,
        std::io::ErrorKind::PermissionDenied => 5,
        std::io::ErrorKind::Unsupported => 50,
        std::io::ErrorKind::OutOfMemory => 8,
        _ => 1117,
    }
}

fn name(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Option<Vec<u16>>, u32> {
    let Some(mut name) = raw_name(env, pointer, wide)? else {
        return Ok(None);
    };
    let local: Vec<_> = "Local\\".encode_utf16().collect();
    let global: Vec<_> = "Global\\".encode_utf16().collect();
    let scope = if name.starts_with(&local) {
        name.drain(..local.len());
        0
    } else if name.starts_with(&global) {
        name.drain(..global.len());
        1
    } else {
        0
    };
    if name.is_empty() || name.contains(&(b'\\' as u16)) {
        return Err(123);
    }
    name.insert(0, scope);
    Ok(Some(name))
}
fn raw_name(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Option<Vec<u16>>, u32> {
    if pointer == 0 {
        return Ok(None);
    }
    let units = if wide {
        let mut text = Vec::new();
        for i in 0..=260u32 {
            let address = pointer.checked_add(i * 2).ok_or(87u32)?;
            let unit = env.cpu.read_u16(address).map_err(|_| 998u32)?;
            if unit == 0 {
                return Ok((!text.is_empty()).then_some(text));
            }
            text.push(unit);
        }
        return Err(206);
    } else {
        let bytes = env.cpu.read_cstr(pointer, 261).map_err(|_| 998u32)?;
        if bytes.len() > 260 {
            return Err(206);
        }
        env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
    };
    Ok((!units.is_empty()).then_some(units))
}

fn file_backing(
    env: &mut Env<'_>,
    handle: u32,
    writable: bool,
    maximum: u64,
) -> Result<Rc<FileSection>, u32> {
    let source = env.sys.objects.host::<FileObj>(handle).ok_or(6u32)?;
    if source.access & (GENERIC_READ | 0x10000000) == 0
        || (writable && source.access & (GENERIC_WRITE | 0x10000000) == 0)
    {
        return Err(5);
    }
    if let Some(data) = source.file.memory_bytes().cloned() {
        if writable || maximum > data.len() as u64 {
            return Err(5);
        }
        return Ok(env.ext.get::<Win32>().vfs.sections.memory(data));
    }
    let old_id = identity(source.file.host().ok_or(6u32)?).map_err(|e| io_error(&e))?;
    let guest_path = source.guest_path.clone();
    let length = source.file.metadata().map_err(|e| io_error(&e))?.len();
    if !writable && maximum > length {
        return Err(5);
    }
    let mut replacements = None;
    let file = if writable {
        let w = env.ext.get::<Win32>();
        let selected = w.vfs.lookup(&guest_path).ok_or(2u32)?;
        let selected_file = File::open(&selected).map_err(|e| io_error(&e))?;
        // Do not copy a replacement file over a still-open older file instance.
        if identity(&selected_file).map_err(|e| io_error(&e))? != old_id {
            return Err(6);
        }
        let destination = w.vfs.mapping_write_path(&guest_path).ok_or(5u32)?;
        if destination != selected {
            std::fs::copy(&selected, &destination).map_err(|e| io_error(&e))?;
        }
        let target = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)
            .map_err(|e| io_error(&e))?;
        if maximum > length {
            target.set_len(maximum).map_err(|e| io_error(&e))?;
        }
        replacements = Some(prepare_retarget_handles(
            env,
            &guest_path,
            old_id,
            &destination,
        )?);
        target
    } else {
        env.sys
            .objects
            .host::<FileObj>(handle)
            .unwrap()
            .file
            .host()
            .ok_or(6u32)?
            .try_clone()
            .map_err(|e| io_error(&e))?
    };
    let backing = env
        .ext
        .get::<Win32>()
        .vfs
        .sections
        .transfer(old_id, file, length.max(maximum), writable)
        .map_err(|e| io_error(&e))?;
    if let Some(replacements) = replacements {
        replacements.apply(&mut env.sys.objects);
    }
    Ok(backing)
}

pub(super) struct Retarget(Vec<(adv_machine::Identity<FileObj>, adv_os::vfs::FileStream)>);

impl Retarget {
    pub fn apply(self, objects: &mut adv_machine::Objects) {
        for (identity, replacement) in self.0 {
            objects.owned_mut(identity).unwrap().file = replacement;
        }
    }
}

/// Reopen matching guest file instances independently, preserving cursors. An
/// overlay copy transfers identity; unrelated files at the same name stay old.
pub(super) fn prepare_retarget_handles(
    env: &Env<'_>,
    guest_path: &str,
    old_id: FileIdentity,
    destination: &std::path::Path,
) -> Result<Retarget, u32> {
    let mut replacements = Vec::new();
    for (token, file) in env.sys.objects.all_hosts::<FileObj>() {
        if file.guest_path.eq_ignore_ascii_case(guest_path)
            && file
                .file
                .host()
                .is_some_and(|source| identity(source).ok() == Some(old_id))
        {
            let position = file.file.stream_position().map_err(|e| io_error(&e))?;
            let mut replacement = std::fs::OpenOptions::new()
                .read(true)
                .write(file.access & (GENERIC_WRITE | 0x10000000) != 0)
                .open(destination)
                .map_err(|e| io_error(&e))?;
            replacement
                .seek(SeekFrom::Start(position))
                .map_err(|e| io_error(&e))?;
            replacements.push((token, replacement.into()));
        }
    }
    Ok(Retarget(replacements))
}

fn create(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (file, attributes, protection) = (env.arg(0), env.arg(1), env.arg(2));
    let maximum = (u64::from(env.arg(3)) << 32) | u64::from(env.arg(4));
    let name = match name(env, env.arg(5), wide) {
        Ok(name) => name,
        Err(e) => return fail(env, e),
    };
    if protection & !(0x08000000 | 0xff) != 0 || protection & 0xff >= 0x10 {
        return fail(env, 50);
    }
    let protection = protection & 0xff;
    if !matches!(protection, 2 | 4 | 8) {
        return fail(env, 87);
    }
    if attributes != 0 {
        if !env.cpu.accessible(attributes, 12, Prot::READ)
            || env.cpu.read_u32(attributes).unwrap_or(0) != 12
        {
            return fail(env, 87);
        }
        if env.cpu.read_u32(attributes + 4).unwrap_or(1) != 0 {
            return fail(env, 50);
        }
    }
    let existing = if let Some(name) = &name {
        match state(env).names.get(name) {
            Some(Named::Mapping(section)) => section
                .upgrade()
                .filter(|section| section.handles.get() != 0),
            Some(Named::Sync { identity, .. }) => {
                let id = *identity;
                if env.sys.objects.has_identity(id) {
                    return fail(env, 6);
                }
                None
            }
            None => None,
        }
    } else {
        None
    };
    if let Some(existing) = existing {
        let handle = env
            .sys
            .objects
            .insert_host(SectionHandle::new(existing, ALL));
        if handle == 0 {
            return fail(env, 8);
        }
        set_last_error(env, 183);
        return Outcome::Return(handle);
    }
    // Extending a backing file is irreversible within this call. Refuse table
    // exhaustion before file_backing can grow it.
    if !env.sys.objects.can_insert() {
        return fail(env, 8);
    }
    let (len, backing) = if file == u32::MAX {
        if maximum == 0 {
            return fail(env, 87);
        }
        (maximum, FileSection::anonymous())
    } else {
        let Some(file_obj) = env.sys.objects.host::<FileObj>(file) else {
            return fail(env, 6);
        };
        let size = match file_obj.file.metadata() {
            Ok(info) => info.len(),
            Err(e) => return fail(env, io_error(&e)),
        };
        let len = if maximum == 0 { size } else { maximum };
        if len == 0 {
            return fail(env, 1006);
        }
        let backing = match file_backing(env, file, protection == 4, len) {
            Ok(backing) => backing,
            Err(e) => return fail(env, e),
        };
        (len, backing)
    };
    if len > u64::MAX - (PAGE - 1) {
        return fail(env, 87);
    }
    let section = Rc::new(Section {
        backing,
        len: len.div_ceil(PAGE) * PAGE,
        protection,
        handles: Cell::new(0),
    });
    if let Some(name) = name {
        state(env)
            .names
            .insert(name, Named::Mapping(Rc::downgrade(&section)));
    }
    let handle = env
        .sys
        .objects
        .insert_host(SectionHandle::new(section, ALL));
    set_last_error(env, 0);
    Outcome::Return(handle)
}

fn open(env: &mut Env<'_>, wide: bool) -> Outcome {
    let access = if env.arg(0) == COPY { READ } else { env.arg(0) };
    let name = match name(env, env.arg(2), wide) {
        Ok(Some(name)) => name,
        Ok(None) => return fail(env, 87),
        Err(e) => return fail(env, e),
    };
    let section = match state(env).names.get(&name) {
        Some(Named::Mapping(section)) => section
            .upgrade()
            .filter(|section| section.handles.get() != 0),
        Some(Named::Sync { identity, .. }) => {
            let id = *identity;
            if env.sys.objects.has_identity(id) {
                return fail(env, 6);
            }
            None
        }
        None => None,
    };
    let Some(section) = section else {
        return fail(env, 2);
    };
    if access & 0x20 != 0 {
        return fail(env, 50);
    }
    let handle = env
        .sys
        .objects
        .insert_host(SectionHandle::new(section, access));
    if handle == 0 {
        return fail(env, 8);
    }
    Outcome::Return(handle)
}

fn map(env: &mut Env<'_>, requested: bool) -> Outcome {
    let (handle, access) = (env.arg(0), env.arg(1));
    let offset = (u64::from(env.arg(2)) << 32) | u64::from(env.arg(3));
    let count = u64::from(env.arg(4));
    let base = if requested { env.arg(5) } else { 0 };
    let Some(handle) = env.sys.objects.host::<SectionHandle>(handle) else {
        return fail(env, 6);
    };
    let section = handle.section.clone();
    let rights = handle.access;
    if access & 0x20 != 0 {
        return fail(env, 50);
    }
    let access = if access == ALL { WRITE } else { access };
    if access & !(READ | WRITE | COPY) != 0 || access == 0 {
        return fail(env, 87);
    }
    let copy = access & WRITE == 0 && access & COPY != 0;
    let write = access & WRITE != 0;
    let required = if write { WRITE } else { READ };
    if rights & required == 0 || (write && section.protection != 4) {
        return fail(env, 5);
    }
    if offset % GRANULE != 0 {
        return fail(env, 1132);
    }
    let count = if count == 0 {
        section.len.saturating_sub(offset)
    } else {
        count
    };
    if count == 0
        || offset
            .checked_add(count)
            .is_none_or(|end| end > section.len)
    {
        return fail(env, 87);
    }
    let span = count.div_ceil(PAGE) * PAGE;
    if span > u64::from(u32::MAX) - GRANULE {
        return fail(env, 8);
    }
    if base != 0 && u64::from(base) % GRANULE != 0 {
        return fail(env, 1132);
    }
    let reserved = span.div_ceil(GRANULE) * GRANULE;
    let address = if base == 0 {
        match env.sys.vm.reserve(span as u32) {
            Some(addr) => addr,
            None => return fail(env, 8),
        }
    } else {
        if !env.sys.vm.reserve_at(base, reserved as u32) {
            return fail(env, 487);
        }
        base
    };
    let mut pages = Vec::new();
    if pages.try_reserve_exact((span / PAGE) as usize).is_err() {
        let _ = env.sys.vm.free(env.cpu, address);
        return fail(env, 8);
    }
    for index in offset / PAGE..(offset + span) / PAGE {
        pages.push(section.backing.page(index));
    }
    let protection = if copy {
        8
    } else if write {
        4
    } else {
        2
    };
    let prot = if write || copy { Prot::RW } else { Prot::READ };
    if env.cpu.map_file_pages(address, &pages, prot, copy).is_err() {
        let _ = env.sys.vm.free(env.cpu, address);
        return fail(env, 8);
    }
    state(env).views.insert(
        address,
        View {
            section,
            offset,
            len: span as u32,
            copy,
            maximum: protection,
            protections: vec![protection; (span / PAGE) as usize],
        },
    );
    Outcome::Return(address)
}

fn flush(env: &mut Env<'_>) -> Outcome {
    let (address, count) = (env.arg(0), env.arg(1));
    let Some((&base, view)) = state(env).views.range(..=address).next_back() else {
        return fail(env, 487);
    };
    if address - base >= view.len {
        return fail(env, 487);
    }
    let count = if count == 0 {
        view.len - (address - base)
    } else {
        count
    };
    if u64::from(address - base) + u64::from(count) > u64::from(view.len) {
        return fail(env, 487);
    }
    if !view.copy {
        let start = view.offset + u64::from(address - base);
        let end = start + u64::from(count);
        if let Err(e) = view.section.backing.flush(start, end) {
            return fail(env, io_error(&e));
        }
    }
    Outcome::Return(1)
}

fn unmap(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    let Some(view) = state(env).views.get(&address) else {
        return fail(env, 487);
    };
    if !view.copy
        && let Err(e) = view
            .section
            .backing
            .flush(view.offset, view.offset + u64::from(view.len))
    {
        return fail(env, io_error(&e));
    }
    if env.sys.vm.free(env.cpu, address).is_err() {
        return fail(env, 487);
    }
    state(env).views.remove(&address);
    if let Some(state) = env.ext.try_get_mut::<Mappings>() {
        state.prune(&env.sys.objects);
    }
    env.ext.get::<Win32>().vfs.sections.prune_pages();
    Outcome::Return(1)
}

/// Forget names whose objects are gone, after a handle closes.
pub(crate) fn prune(env: &mut Env<'_>) {
    if let Some(state) = env.ext.try_get_mut::<Mappings>() {
        state.prune(&env.sys.objects);
    }
}

pub(super) fn sync_created(env: &mut Env<'_>, object: Object, pointer: u32, wide: bool) -> Outcome {
    let name = match name(env, pointer, wide) {
        Ok(name) => name,
        Err(e) => return fail(env, e),
    };
    let kind = match object {
        Object::Event { .. } => 1,
        Object::Mutex { .. } => 2,
        Object::Semaphore { .. } => 3,
        _ => unreachable!(),
    };
    if let Some(name) = &name {
        match state(env).names.get(name) {
            Some(Named::Mapping(section))
                if section
                    .upgrade()
                    .is_some_and(|section| section.handles.get() != 0) =>
            {
                return fail(env, 6);
            }
            Some(Named::Sync {
                identity,
                kind: existing,
            }) => {
                let (id, existing) = (*identity, *existing);
                if env.sys.objects.has_identity(id) {
                    if kind != existing {
                        return fail(env, 6);
                    }
                    let Some(handle) = env.sys.objects.alias_identity(id) else {
                        return fail(env, 8);
                    };
                    set_last_error(env, 183);
                    return Outcome::Return(handle);
                }
            }
            _ => {}
        }
    }
    let handle = env.sys.objects.insert(object);
    if handle == 0 {
        return fail(env, 8);
    }
    if let Some(name) = name {
        let identity = env.sys.objects.identity(handle).unwrap();
        state(env)
            .names
            .insert(name, Named::Sync { identity, kind });
    }
    set_last_error(env, 0);
    Outcome::Return(handle)
}

fn page_protection(env: &Env<'_>, view: &View, base: u32, page: u32) -> u32 {
    let value = view.protections[((page - base) / 4096) as usize];
    if value == 8 && env.cpu.file_page_copy_on_write(page) == Some(false) {
        4
    } else {
        value
    }
}

pub(super) fn contains(env: &Env<'_>, addr: u32) -> bool {
    env.ext
        .try_get::<Mappings>()
        .and_then(|state| state.views.range(..=addr).next_back())
        .is_some_and(|(&base, view)| addr - base < view.len)
}

pub(super) fn query(env: &Env<'_>, addr: u32) -> Option<[u32; 7]> {
    let (&base, view) = env
        .ext
        .try_get::<Mappings>()?
        .views
        .range(..=addr)
        .next_back()?;
    if addr - base >= view.len {
        return None;
    }
    let page = addr & !4095;
    let protection = page_protection(env, view, base, page);
    let mut begin = page;
    let mut end = u64::from(page) + PAGE;
    while begin > base && page_protection(env, view, base, begin - 4096) == protection {
        begin -= 4096;
    }
    while end < u64::from(base) + u64::from(view.len)
        && page_protection(env, view, base, end as u32) == protection
    {
        end += PAGE;
    }
    Some([
        begin,
        base,
        view.maximum,
        (end - u64::from(begin)) as u32,
        0x1000,
        protection,
        0x40000,
    ])
}

pub(super) fn protect(
    env: &mut Env<'_>,
    addr: u32,
    len: u32,
    value: u32,
) -> Option<Result<u32, u32>> {
    let (&base, view) = env
        .ext
        .try_get::<Mappings>()?
        .views
        .range(..=addr)
        .next_back()?;
    if addr - base >= view.len {
        return None;
    }
    if u64::from(addr - base) + u64::from(len) > u64::from(view.len) {
        return Some(Err(487));
    }
    if !matches!(value, 1 | 2 | 4 | 8) {
        return Some(Err(50));
    }
    if view.maximum == 2 && !matches!(value, 1 | 2) {
        return Some(Err(87));
    }
    if value == 8 && !view.copy {
        return Some(Err(50));
    } // Changing a WRITE view into COPY needs a new view.
    let previous = page_protection(env, view, base, addr);
    let prot = match value {
        1 => Prot::default(),
        2 => Prot::READ,
        _ => Prot::RW,
    };
    if env.cpu.protect(addr, len, prot).is_err() {
        return Some(Err(487));
    }
    let view = state(env).views.get_mut(&base).unwrap();
    let start = ((addr - base) / 4096) as usize;
    view.protections[start..start + (len / 4096) as usize].fill(value);
    Some(Ok(previous))
}

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, K32, "CreateFileMappingA", 6, |env| {
        create(env, false)
    });
    stdcall(registry, K32, "CreateFileMappingW", 6, |env| {
        create(env, true)
    });
    stdcall(registry, K32, "OpenFileMappingA", 3, |env| open(env, false));
    stdcall(registry, K32, "OpenFileMappingW", 3, |env| open(env, true));
    stdcall(registry, K32, "MapViewOfFile", 5, |env| map(env, false));
    stdcall(registry, K32, "MapViewOfFileEx", 6, |env| map(env, true));
    stdcall(registry, K32, "FlushViewOfFile", 2, flush);
    stdcall(registry, K32, "UnmapViewOfFile", 1, unmap);
}
