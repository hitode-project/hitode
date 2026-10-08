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

pub(crate) fn restrict_section_access(env: &mut Env<'_>, handle: u32, access: u32) {
    if let Some(handle) = env.sys.objects.host_mut::<SectionHandle>(handle) {
        handle.access = access & (READ | WRITE);
    }
}

pub(crate) fn view_extent(env: &Env<'_>, address: u32) -> Option<(u32, u32)> {
    let (&base, view) = env
        .ext
        .try_get::<Mappings>()?
        .views
        .range(..=address)
        .next_back()?;
    (address - base < view.len).then_some((base, view.len))
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
    // NT checks the mapped FILE_READ_DATA / FILE_WRITE_DATA rights, so a
    // specific-rights NtOpenFile handle backs sections like GENERIC_* ones.
    if source.access & (GENERIC_READ | 0x10000000 | 1) == 0
        || (writable && source.access & (GENERIC_WRITE | 0x10000000 | 2) == 0)
    {
        return Err(5);
    }
    if let Some(data) = source.file.memory_bytes().cloned() {
        if writable || maximum > data.len() as u64 {
            return Err(5);
        }
        return Ok(env.ext.get::<Win32>().vfs.sections.memory(data));
    }
    if let Some(file) = source.file.optical_file() {
        if writable || maximum > source.file.metadata().map_err(|e| io_error(&e))?.len() {
            return Err(5);
        }
        return Ok(env.ext.get::<Win32>().vfs.sections.optical(file.clone()));
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
                .write(file.access & (GENERIC_WRITE | 0x10000000 | 2 | 4) != 0)
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
    create_section(env, file, attributes, protection, maximum, name)
}

pub(crate) fn create_section(
    env: &mut Env<'_>,
    file: u32,
    attributes: u32,
    protection: u32,
    maximum: u64,
    name: Option<Vec<u16>>,
) -> Outcome {
    if protection & !(0x08000000 | 0xff) != 0 || protection & 0xff >= 0x10 {
        return fail(env, 50);
    }
    let protection = protection & 0xff;
    if !matches!(protection, 2 | 4 | 8) {
        return fail(env, 87);
    }
    let inherit = match super::handle::security_inherit(env, attributes) {
        Ok(inherit) => inherit,
        Err(code) => return fail(env, code),
    };
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
        super::handle::set_inherit(env, handle, inherit);
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
    super::handle::set_inherit(env, handle, inherit);
    set_last_error(env, 0);
    Outcome::Return(handle)
}

fn open(env: &mut Env<'_>, wide: bool) -> Outcome {
    let inherit = env.arg(1) != 0;
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
    super::handle::set_inherit(env, handle, inherit);
    Outcome::Return(handle)
}

fn map(env: &mut Env<'_>, requested: bool) -> Outcome {
    let (handle, access) = (env.arg(0), env.arg(1));
    let offset = (u64::from(env.arg(2)) << 32) | u64::from(env.arg(3));
    let count = u64::from(env.arg(4));
    let base = if requested { env.arg(5) } else { 0 };
    map_section(env, handle, access, offset, count, base)
}

pub(crate) fn map_section(
    env: &mut Env<'_>,
    handle: u32,
    access: u32,
    offset: u64,
    count: u64,
    base: u32,
) -> Outcome {
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

/// DIBs use DWORD offsets inside a private view of the same shared section.
/// The mapping owner already retains the section after its handle is closed.
pub(crate) fn map_dib_section(
    env: &mut Env<'_>,
    handle: u32,
    offset: u32,
    bytes: u32,
) -> Result<(u32, u32), u32> {
    if offset & 3 != 0 || bytes == 0 {
        return Err(87);
    }
    let protection = env
        .sys
        .objects
        .host::<SectionHandle>(handle)
        .ok_or(6u32)?
        .section
        .protection;
    let access = match protection {
        4 => WRITE,
        8 => COPY,
        _ => return Err(87),
    };
    let aligned = u64::from(offset) / GRANULE * GRANULE;
    let delta = u64::from(offset) - aligned;
    match map_section(env, handle, access, aligned, u64::from(bytes) + delta, 0) {
        Outcome::Return(0) => Err(crate::util::last_error(env)),
        Outcome::Return(base) => Ok((base, base + delta as u32)),
        _ => unreachable!("mapping a data section is synchronous"),
    }
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
    unmap_section(env, address)
}

pub(crate) fn unmap_section(env: &mut Env<'_>, address: u32) -> Outcome {
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
    let name = match sync_name(env, pointer, wide) {
        Ok(name) => name,
        Err(e) => return fail(env, e),
    };
    match sync_create_named(env, object, name, true) {
        Ok((handle, existed)) => {
            set_last_error(env, if existed { 183 } else { 0 });
            Outcome::Return(handle)
        }
        Err(error) => fail(env, error),
    }
}

/// Win32 and native synchronization adapters share this process-owned name key.
pub(super) fn sync_name(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Option<Vec<u16>>, u32> {
    name(env, pointer, wide)
}

pub(super) fn sync_create_named(
    env: &mut Env<'_>,
    object: Object,
    name: Option<Vec<u16>>,
    open_if: bool,
) -> Result<(u32, bool), u32> {
    let kind = match object {
        Object::Event { .. } => 1,
        Object::Mutex { .. } => 2,
        Object::Semaphore { .. } => 3,
        Object::Timer { .. } => 4,
        _ => unreachable!(),
    };
    if let Some(name) = &name {
        match state(env).names.get(name) {
            Some(Named::Mapping(section))
                if section
                    .upgrade()
                    .is_some_and(|section| section.handles.get() != 0) =>
            {
                return Err(6);
            }
            Some(Named::Sync {
                identity,
                kind: existing,
            }) => {
                let (id, existing) = (*identity, *existing);
                if env.sys.objects.has_identity(id) {
                    if kind != existing {
                        return Err(6);
                    }
                    if !open_if {
                        return Err(183);
                    }
                    let handle = env.sys.objects.alias_identity(id).ok_or(8u32)?;
                    return Ok((handle, true));
                }
            }
            _ => {}
        }
    }
    let handle = env.sys.objects.insert(object);
    if handle == 0 {
        return Err(8);
    }
    if let Some(name) = name {
        let identity = env.sys.objects.identity(handle).unwrap();
        state(env)
            .names
            .insert(name, Named::Sync { identity, kind });
    }
    Ok((handle, false))
}

pub(super) fn sync_lookup_named(env: &mut Env<'_>, name: &[u16], kind: u32) -> Result<u32, u32> {
    prune(env);
    match state(env).names.get(name) {
        Some(Named::Sync {
            identity,
            kind: existing,
        }) if *existing == kind => Ok(*identity),
        Some(_) => Err(6),
        None => Err(2),
    }
}

/// Open an existing process-owned mutex through the same namespace as Create.
#[cfg(test)]
pub(super) fn open_mutex(env: &mut Env<'_>, pointer: u32, wide: bool) -> Outcome {
    open_sync(env, pointer, wide, 2, 0x001f0001)
}

pub(super) fn open_sync(
    env: &mut Env<'_>,
    pointer: u32,
    wide: bool,
    kind: u32,
    full_access: u32,
) -> Outcome {
    if pointer == 0 {
        return fail(env, 87);
    }
    let name = match name(env, pointer, wide) {
        Ok(Some(name)) => name,
        Ok(None) => return fail(env, 6),
        Err(123) => return fail(env, 3),
        Err(error) => return fail(env, error),
    };
    let identity = match sync_lookup_named(env, &name, kind) {
        Ok(identity) => identity,
        Err(error) => return fail(env, error),
    };
    // Kernel aliases currently carry full access. Event opens record inherit
    // flags on the existing aliases; other fronts retain their supported profile.
    // Do not silently broaden a restricted access request.
    if (env.arg(0) != full_access && env.arg(0) != 0x10000000) || kind != 1 && env.arg(1) != 0 {
        if kind == 4 {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "OpenWaitableTimerW"
                } else {
                    "OpenWaitableTimerA"
                },
                "restricted access and inherited timer handles are not modeled",
            );
        } else if kind == 1 {
            adv_abi::warn_unavailable(
                env,
                if wide { "OpenEventW" } else { "OpenEventA" },
                "restricted event access is not modeled; use EVENT_ALL_ACCESS or GENERIC_ALL",
            );
        }
        return fail(env, 50);
    }
    match env.sys.objects.alias_identity(identity) {
        Some(handle) => {
            if kind == 1 {
                let inherit = env.arg(1) != 0;
                super::handle::set_inherit(env, handle, inherit);
            }
            set_last_error(env, 0);
            Outcome::Return(handle)
        }
        None => fail(env, 8),
    }
}

fn page_protection(env: &Env<'_>, view: &View, base: u32, page: u32) -> u32 {
    let value = view.protections[((page - base) / 4096) as usize];
    if value == 8 && env.cpu.file_page_copy_on_write(page) == Some(false) {
        4
    } else {
        value
    }
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

#[cfg(test)]
mod mutex_tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Reg},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};

    fn returned(outcome: Outcome) -> u32 {
        match outcome {
            Outcome::Return(value) => value,
            _ => panic!("mutex operation must return"),
        }
    }

    #[test]
    fn open_mutex_uses_shared_named_identity_and_last_alias_lifetime() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/open-mutex-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "authored.exe", &root),
        )
        .unwrap();
        machine.spawn("owner", PARK_ADDR, &[], 65536).unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        machine.cpu.set_reg(Reg::Esp, 0x410000);
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        for (offset, text) in [
            (0x100, "authored-mutex"),
            (0x200, "Local\\authored-mutex"),
            (0x300, "Global\\authored-mutex"),
            (0x400, "Authored-mutex"),
            (0x500, "bad\\name"),
        ] {
            env.cpu
                .write(0x410000 + offset, &[text.as_bytes(), &[0]].concat())
                .unwrap();
        }
        let wide: Vec<_> = "authored-mutex"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        env.cpu.write(0x410600, &wide).unwrap();
        env.cpu.write_u32(0x410004, 0x1f0001).unwrap();
        env.cpu.write_u32(0x410008, 0).unwrap();
        assert_eq!(returned(open_mutex(&mut env, 0x410100, false)), 0);
        assert_eq!(crate::util::last_error(&env), 2);
        let original = returned(sync_created(
            &mut env,
            Object::Mutex {
                owner: Some(0),
                recursion: 1,
            },
            0x410100,
            false,
        ));
        assert_ne!(original, 0);
        let local = returned(open_mutex(&mut env, 0x410200, false));
        let unicode = returned(open_mutex(&mut env, 0x410600, true));
        assert_ne!(local, original);
        assert!(env.sys.objects.same_object(original, local));
        assert!(env.sys.objects.same_object(original, unicode));
        assert!(matches!(
            env.sys.objects.get(local),
            Some(Object::Mutex {
                owner: Some(0),
                recursion: 1
            })
        ));
        for (pointer, error) in [
            (0, 87),
            (0x410400, 2),
            (0x410500, 3),
            (0x410300, 2),
            (0x410700, 6),
        ] {
            assert_eq!(returned(open_mutex(&mut env, pointer, false)), 0);
            assert_eq!(crate::util::last_error(&env), error);
        }
        let global = returned(sync_created(
            &mut env,
            Object::Event {
                manual_reset: true,
                signaled: false,
            },
            0x410300,
            false,
        ));
        assert_ne!(global, 0);
        assert_eq!(returned(open_mutex(&mut env, 0x410300, false)), 0);
        assert_eq!(crate::util::last_error(&env), 6);
        // Unsupported restrictions fail explicitly rather than producing an
        // unrestricted or inheritable alias.
        env.cpu.write_u32(0x410004, 0x100000).unwrap();
        assert_eq!(returned(open_mutex(&mut env, 0x410100, false)), 0);
        assert_eq!(crate::util::last_error(&env), 50);
        env.cpu.write_u32(0x410004, 0x1f0001).unwrap();
        env.cpu.write_u32(0x410008, 1).unwrap();
        assert_eq!(returned(open_mutex(&mut env, 0x410100, false)), 0);
        assert_eq!(crate::util::last_error(&env), 50);
        env.cpu.write_u32(0x410008, 0).unwrap();
        env.sys.objects.close(original).unwrap();
        env.sys.objects.close(local).unwrap();
        prune(&mut env);
        let reopened = returned(open_mutex(&mut env, 0x410100, false));
        assert!(env.sys.objects.same_object(unicode, reopened));
        env.sys.objects.close(unicode).unwrap();
        env.sys.objects.close(reopened).unwrap();
        assert_eq!(returned(open_mutex(&mut env, 0x410100, false)), 0);
        assert_eq!(crate::util::last_error(&env), 2);
    }
}
