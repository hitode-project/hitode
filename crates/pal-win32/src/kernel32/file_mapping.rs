//! Worker-owned data mappings. Views retain their section after handles close.
//! Executable/image/reserved/device mappings and cross-host-process sharing are
//! explicitly outside this implementation; no native OS named object is created.
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::{Rc, Weak};

use pal_cpu::{FilePage, GuestExt, Prot};
use pal_machine::{Env, Object, Outcome, Registry};

use super::K32;
use crate::{Win32, stdcall, util::set_last_error};

const PAGE: u64 = 4096;
const GRANULE: u64 = 65536;
const READ: u32 = 4;
const WRITE: u32 = 2;
const COPY: u32 = 1;
const ALL: u32 = 0xf001f;
const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;
type Identity = (u64, u64);

#[derive(Default)]
struct Mappings {
    handles: HashMap<u32, Handle>,
    names: HashMap<Vec<u16>, Named>,
    files: HashMap<Identity, Weak<Backing>>,
    views: BTreeMap<u32, View>,
}
struct Handle {
    section: Rc<Section>,
    access: u32,
}
enum Named {
    Mapping(Weak<Section>),
    Sync { identity: u32, kind: u32 },
}
impl Mappings {
    fn prune(&mut self, objects: &pal_machine::Objects) {
        self.names.retain(|_, name| match name {
            Named::Mapping(section) => section.upgrade().is_some_and(|section| section.handles.get() != 0),
            Named::Sync { identity, .. } => objects.has_identity(*identity),
        });
        self.files.retain(|_, backing| backing.strong_count() != 0);
    }
}
struct Section {
    backing: Rc<Backing>,
    len: u64,
    protection: u32,
    handles: Cell<u32>,
}
struct Backing {
    file: RefCell<Option<File>>,
    file_len: Cell<u64>,
    writable: Cell<bool>,
    pages: RefCell<BTreeMap<u64, Rc<FilePage>>>,
}
struct View {
    section: Rc<Section>,
    offset: u64,
    len: u32,
    copy: bool,
    maximum: u32,
    protections: Vec<u32>,
}

impl Drop for Backing {
    fn drop(&mut self) {
        // Views can outlive every section/file handle. Process teardown must
        // still persist dirty shared data when the guest omits Flush/Unmap.
        // Rc confines this final owner to the guest worker, never an audio callback.
        if let Err(error) = self.flush(0, self.file_len.get()) {
            log::error!("file mapping teardown could not persist dirty data: {error}");
        }
    }
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
    let Some(mut name) = raw_name(env, pointer, wide)? else { return Ok(None) };
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

#[cfg(windows)]
fn identity(file: &File) -> std::io::Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        high_size: u32,
        low_size: u32,
        links: u32,
        high_index: u32,
        low_index: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Info) -> i32;
    }
    let mut info = Info::default();
    // SAFETY: live owned file and correctly sized writable native metadata.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((u64::from(info.volume), (u64::from(info.high_index) << 32) | u64::from(info.low_index)))
}
#[cfg(unix)]
fn identity(file: &File) -> std::io::Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(not(any(windows, unix)))]
fn identity(_: &File) -> std::io::Result<Identity> {
    Err(std::io::ErrorKind::Unsupported.into())
}

impl Backing {
    fn page(self: &Rc<Self>, index: u64) -> Rc<FilePage> {
        if let Some(page) = self.pages.borrow().get(&index) {
            return page.clone();
        }
        let backing = Rc::downgrade(self);
        let page = FilePage::lazy(move || {
            backing.upgrade().ok_or("file mapping backing was retired")?.load_page(index).map_err(|e| e.to_string())
        });
        self.pages.borrow_mut().insert(index, page.clone());
        page
    }

    fn load_page(&self, index: u64) -> std::io::Result<[u8; 4096]> {
        let mut bytes = [0; 4096];
        if let Some(file) = self.file.borrow().as_ref() {
            // A cloned Windows FD shares its cursor. Restore it even after an
            // I/O error; all guest operations use this same execution worker.
            let mut file = file;
            let saved = file.stream_position()?;
            let result = (|| {
                file.seek(SeekFrom::Start(index * PAGE))?;
                let mut read = 0;
                while read < bytes.len() {
                    let n = file.read(&mut bytes[read..])?;
                    if n == 0 {
                        break;
                    }
                    read += n;
                }
                Ok::<_, std::io::Error>(())
            })();
            let restored = file.seek(SeekFrom::Start(saved));
            result?;
            restored?;
        }
        Ok(bytes)
    }

    fn flush(&self, start: u64, end: u64) -> std::io::Result<()> {
        let file = self.file.borrow();
        let Some(file) = file.as_ref() else { return Ok(()) };
        let mut file = file;
        let saved = file.stream_position()?;
        let mut flushed = Vec::new();
        let result = (|| {
            let mut wrote = false;
            for (&index, page) in self.pages.borrow().range(start / PAGE..end.div_ceil(PAGE)) {
                if !page.dirty() {
                    continue;
                }
                if !self.writable.get() {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
                let position = index * PAGE;
                let count = self.file_len.get().saturating_sub(position).min(PAGE) as usize;
                if count != 0 {
                    file.seek(SeekFrom::Start(position))?;
                    file.write_all(&page.snapshot().map_err(std::io::Error::other)?[..count])?;
                }
                flushed.push(page.clone());
                wrote = true;
            }
            if wrote {
                file.sync_data()?;
            }
            Ok::<_, std::io::Error>(())
        })();
        let restored = file.seek(SeekFrom::Start(saved));
        result?;
        restored?;
        for page in flushed {
            page.clear_dirty();
        }
        Ok(())
    }
}

fn file_backing(env: &mut Env<'_>, handle: u32, writable: bool, maximum: u64) -> Result<Rc<Backing>, u32> {
    let source = env.ext.get::<Win32>().files.get(&handle).ok_or(6u32)?;
    if source.access & (GENERIC_READ | 0x10000000) == 0
        || (writable && source.access & (GENERIC_WRITE | 0x10000000) == 0)
    {
        return Err(5);
    }
    let old_id = identity(&source.file).map_err(|e| io_error(&e))?;
    let guest_path = source.guest_path.clone();
    let length = source.file.metadata().map_err(|e| io_error(&e))?.len();
    if !writable && maximum > length {
        return Err(5);
    }
    let backing = state(env).files.get(&old_id).and_then(Weak::upgrade);
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
        let target = std::fs::OpenOptions::new().read(true).write(true).open(&destination).map_err(|e| io_error(&e))?;
        if maximum > length {
            target.set_len(maximum).map_err(|e| io_error(&e))?;
        }
        let new_id = identity(&target).map_err(|e| io_error(&e))?;
        // Reopen each matching file instance independently, preserving its own
        // access metadata and position. A different file at the same name stays.
        for file in env.ext.get_mut::<Win32>().files.values_mut() {
            if file.guest_path.eq_ignore_ascii_case(&guest_path)
                && identity(&file.file).map_err(|e| io_error(&e))? == old_id
            {
                let position = file.file.stream_position().map_err(|e| io_error(&e))?;
                let mut replacement = std::fs::OpenOptions::new()
                    .read(true)
                    .write(file.access & (GENERIC_WRITE | 0x10000000) != 0)
                    .open(&destination)
                    .map_err(|e| io_error(&e))?;
                replacement.seek(SeekFrom::Start(position)).map_err(|e| io_error(&e))?;
                file.file = replacement;
            }
        }
        if let Some(backing) = &backing {
            *backing.file.borrow_mut() = Some(target);
            backing.writable.set(true);
            backing.file_len.set(length.max(maximum));
            state(env).files.insert(new_id, Rc::downgrade(backing));
            return Ok(backing.clone());
        }
        target
    } else {
        env.ext.get::<Win32>().files[&handle].file.try_clone().map_err(|e| io_error(&e))?
    };
    if let Some(backing) = backing {
        return Ok(backing);
    }
    let id = identity(&file).map_err(|e| io_error(&e))?;
    let backing = Rc::new(Backing {
        file: RefCell::new(Some(file)),
        file_len: Cell::new(length.max(maximum)),
        writable: Cell::new(writable),
        pages: RefCell::new(BTreeMap::new()),
    });
    state(env).files.insert(old_id, Rc::downgrade(&backing));
    state(env).files.insert(id, Rc::downgrade(&backing));
    Ok(backing)
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
        if !env.cpu.accessible(attributes, 12, Prot::READ) || env.cpu.read_u32(attributes).unwrap_or(0) != 12 {
            return fail(env, 87);
        }
        if env.cpu.read_u32(attributes + 4).unwrap_or(1) != 0 {
            return fail(env, 50);
        }
    }
    let existing = if let Some(name) = &name {
        match state(env).names.get(name) {
            Some(Named::Mapping(section)) => section.upgrade().filter(|section| section.handles.get() != 0),
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
        existing.handles.set(existing.handles.get() + 1);
        let handle = env.sys.objects.insert(Object::Opaque(0x4d4150));
        state(env).handles.insert(handle, Handle { section: existing, access: ALL });
        set_last_error(env, 183);
        return Outcome::Return(handle);
    }
    let (len, backing) = if file == u32::MAX {
        if maximum == 0 {
            return fail(env, 87);
        }
        (
            maximum,
            Rc::new(Backing {
                file: RefCell::new(None),
                file_len: Cell::new(0),
                writable: Cell::new(false),
                pages: RefCell::new(BTreeMap::new()),
            }),
        )
    } else {
        let Some(file_obj) = env.ext.get::<Win32>().files.get(&file) else { return fail(env, 6) };
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
    let section = Rc::new(Section { backing, len: len.div_ceil(PAGE) * PAGE, protection, handles: Cell::new(1) });
    let handle = env.sys.objects.insert(Object::Opaque(0x4d4150));
    if let Some(name) = name {
        state(env).names.insert(name, Named::Mapping(Rc::downgrade(&section)));
    }
    state(env).handles.insert(handle, Handle { section, access: ALL });
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
        Some(Named::Mapping(section)) => section.upgrade().filter(|section| section.handles.get() != 0),
        Some(Named::Sync { identity, .. }) => {
            let id = *identity;
            if env.sys.objects.has_identity(id) {
                return fail(env, 6);
            }
            None
        }
        None => None,
    };
    let Some(section) = section else { return fail(env, 2) };
    if access & 0x20 != 0 {
        return fail(env, 50);
    }
    let handle = env.sys.objects.insert(Object::Opaque(0x4d4150));
    section.handles.set(section.handles.get() + 1);
    state(env).handles.insert(handle, Handle { section, access });
    Outcome::Return(handle)
}

fn map(env: &mut Env<'_>, requested: bool) -> Outcome {
    let (handle, access) = (env.arg(0), env.arg(1));
    let offset = (u64::from(env.arg(2)) << 32) | u64::from(env.arg(3));
    let count = u64::from(env.arg(4));
    let base = if requested { env.arg(5) } else { 0 };
    let Some(handle) = state(env).handles.get(&handle) else { return fail(env, 6) };
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
    let count = if count == 0 { section.len.saturating_sub(offset) } else { count };
    if count == 0 || offset.checked_add(count).is_none_or(|end| end > section.len) {
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
    let Some((&base, view)) = state(env).views.range(..=address).next_back() else { return fail(env, 487) };
    if address - base >= view.len {
        return fail(env, 487);
    }
    let count = if count == 0 { view.len - (address - base) } else { count };
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
    let Some(view) = state(env).views.get(&address) else { return fail(env, 487) };
    if !view.copy {
        if let Err(e) = view.section.backing.flush(view.offset, view.offset + u64::from(view.len)) {
            return fail(env, io_error(&e));
        }
    }
    if env.sys.vm.free(env.cpu, address).is_err() {
        return fail(env, 487);
    }
    state(env).views.remove(&address);
    if let Some(state) = env.ext.try_get_mut::<Mappings>() {
        state.prune(&env.sys.objects);
        for backing in state.files.values().filter_map(Weak::upgrade) {
            backing.pages.borrow_mut().retain(|_, page| page.dirty() || Rc::strong_count(page) > 1);
        }
    }
    Outcome::Return(1)
}

pub(super) fn close(env: &mut Env<'_>, handle: u32) {
    if let Some(state) = env.ext.try_get_mut::<Mappings>() {
        if let Some(handle) = state.handles.remove(&handle) {
            handle.section.handles.set(handle.section.handles.get() - 1);
        }
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
            Some(Named::Mapping(section)) if section.upgrade().is_some_and(|section| section.handles.get() != 0) => {
                return fail(env, 6);
            }
            Some(Named::Sync { identity, kind: existing }) => {
                let (id, existing) = (*identity, *existing);
                if env.sys.objects.has_identity(id) {
                    if kind != existing {
                        return fail(env, 6);
                    }
                    let Some(handle) = env.sys.objects.alias_identity(id) else { return fail(env, 8) };
                    set_last_error(env, 183);
                    return Outcome::Return(handle);
                }
            }
            _ => {}
        }
    }
    let handle = env.sys.objects.insert(object);
    if let Some(name) = name {
        let identity = env.sys.objects.identity(handle).unwrap();
        state(env).names.insert(name, Named::Sync { identity, kind });
    }
    set_last_error(env, 0);
    Outcome::Return(handle)
}

fn page_protection(env: &Env<'_>, view: &View, base: u32, page: u32) -> u32 {
    let value = view.protections[((page - base) / 4096) as usize];
    if value == 8 && env.cpu.file_page_copy_on_write(page) == Some(false) { 4 } else { value }
}

pub(super) fn contains(env: &Env<'_>, addr: u32) -> bool {
    env.ext
        .try_get::<Mappings>()
        .and_then(|state| state.views.range(..=addr).next_back())
        .is_some_and(|(&base, view)| addr - base < view.len)
}

pub(super) fn query(env: &Env<'_>, addr: u32) -> Option<[u32; 7]> {
    let (&base, view) = env.ext.try_get::<Mappings>()?.views.range(..=addr).next_back()?;
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
    while end < u64::from(base) + u64::from(view.len) && page_protection(env, view, base, end as u32) == protection {
        end += PAGE;
    }
    Some([begin, base, view.maximum, (end - u64::from(begin)) as u32, 0x1000, protection, 0x40000])
}

pub(super) fn protect(env: &mut Env<'_>, addr: u32, len: u32, value: u32) -> Option<Result<u32, u32>> {
    let (&base, view) = env.ext.try_get::<Mappings>()?.views.range(..=addr).next_back()?;
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
    stdcall(registry, K32, "CreateFileMappingA", 6, |env| create(env, false));
    stdcall(registry, K32, "CreateFileMappingW", 6, |env| create(env, true));
    stdcall(registry, K32, "OpenFileMappingA", 3, |env| open(env, false));
    stdcall(registry, K32, "OpenFileMappingW", 3, |env| open(env, true));
    stdcall(registry, K32, "MapViewOfFile", 5, |env| map(env, false));
    stdcall(registry, K32, "MapViewOfFileEx", 6, |env| map(env, true));
    stdcall(registry, K32, "FlushViewOfFile", 2, flush);
    stdcall(registry, K32, "UnmapViewOfFile", 1, unmap);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_flush_keeps_all_dirty_pages_and_cursor_for_retry() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/mapping-flush-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("retry-{}.bin", std::process::id()));
        std::fs::write(&path, [0u8; 8192]).unwrap();
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let fail = Rc::new(Cell::new(true));
        let injected = fail.clone();
        let first = FilePage::new([0x11; 4096]);
        first.mark_dirty();
        let second =
            FilePage::lazy(
                move || {
                    if injected.get() { Err("second page snapshot failure".into()) } else { Ok([0x22; 4096]) }
                },
            );
        second.mark_dirty();
        let backing = Backing {
            file: RefCell::new(Some(file)),
            file_len: Cell::new(8192),
            writable: Cell::new(true),
            pages: RefCell::new(BTreeMap::from([(0, first.clone()), (1, second.clone())])),
        };
        let retained = backing.file.borrow();
        retained.as_ref().unwrap().seek(SeekFrom::Start(17)).unwrap();
        drop(retained);
        assert!(backing.flush(0, 8192).is_err());
        assert!(first.dirty() && second.dirty());
        assert_eq!(backing.file.borrow().as_ref().unwrap().stream_position().unwrap(), 17);
        fail.set(false);
        backing.flush(0, 8192).unwrap();
        assert!(!first.dirty() && !second.dirty());
        assert_eq!(backing.file.borrow().as_ref().unwrap().stream_position().unwrap(), 17);
        let output = std::fs::read(&path).unwrap();
        assert_eq!(&output[..4096], &[0x11; 4096]);
        assert_eq!(&output[4096..], &[0x22; 4096]);
        drop(backing);
        std::fs::remove_file(path).unwrap();
    }
}
