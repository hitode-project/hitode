//! Worker-owned shared file content, used by mapped views and ordinary I/O.
//! The identity cache is weak. Section/view owners retain content; a path alone
//! never revives a closed or replaced file instance. No Win32 names or handles.
use adv_cpu::backend::FilePage;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::rc::{Rc, Weak};
use std::sync::Arc;

const PAGE: u64 = 4096;
pub type FileIdentity = (u64, u64);
pub struct FileSection {
    file: RefCell<Option<File>>,
    virtual_data: Option<std::sync::Arc<[u8]>>,
    optical_data: Option<super::OpticalFile>,
    file_len: Cell<u64>,
    writable: Cell<bool>,
    pages: RefCell<BTreeMap<u64, Rc<FilePage>>>,
}
impl Drop for FileSection {
    fn drop(&mut self) {
        // Views can outlive every section/file handle. Process teardown must
        // still persist dirty shared data when the guest omits Flush/Unmap.
        // Rc confines this final owner to the guest worker, never an audio callback.
        if let Err(error) = self.flush(0, self.file_len.get()) {
            log::error!("file mapping teardown could not persist dirty data: {error}");
        }
    }
}

pub fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    #[cfg(test)]
    IDENTITY_QUERIES.with(|count| count.set(count.get() + 1));
    host_file_identity(file)
}

#[cfg(test)]
thread_local! {
    static IDENTITY_QUERIES: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn identity_queries() -> usize {
    IDENTITY_QUERIES.with(Cell::get)
}

#[cfg(windows)]
fn host_file_identity(file: &File) -> std::io::Result<FileIdentity> {
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
    Ok((
        u64::from(info.volume),
        (u64::from(info.high_index) << 32) | u64::from(info.low_index),
    ))
}
#[cfg(unix)]
fn host_file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(not(any(windows, unix)))]
fn host_file_identity(_: &File) -> std::io::Result<FileIdentity> {
    Err(std::io::ErrorKind::Unsupported.into())
}

impl FileSection {
    /// Synchronize each successful short-write prefix, including when a later
    /// write fails. No lazy read is performed after external file mutation.
    pub fn write_to(&self, writer: &mut impl Write, offset: u64, input: &[u8]) -> io::Result<()> {
        offset
            .checked_add(input.len() as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let mut committed = 0;
        while committed < input.len() {
            match writer.write(&input[committed..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    self.publish_write(
                        offset + committed as u64,
                        &input[committed..committed + count],
                    );
                    committed += count;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    pub fn page(self: &Rc<Self>, index: u64) -> Rc<FilePage> {
        if let Some(page) = self.pages.borrow().get(&index) {
            return page.clone();
        }
        let backing = Rc::downgrade(self);
        let page = FilePage::lazy(move || {
            backing
                .upgrade()
                .ok_or("file mapping backing was retired")?
                .load_page(index)
                .map_err(|e| e.to_string())
        });
        self.pages.borrow_mut().insert(index, page.clone());
        page
    }

    fn load_page(&self, index: u64) -> std::io::Result<[u8; 4096]> {
        let mut bytes = [0; 4096];
        if let Some(file) = &self.optical_data {
            file.read_at(index.checked_mul(PAGE).ok_or(io::ErrorKind::InvalidInput)?, &mut bytes)?;
            return Ok(bytes);
        }
        if let Some(data) = &self.virtual_data {
            let start = usize::try_from(
                index
                    .checked_mul(PAGE)
                    .ok_or(std::io::ErrorKind::InvalidInput)?,
            )
            .map_err(|_| std::io::ErrorKind::InvalidInput)?;
            if let Some(source) = data.get(start..) {
                let length = source.len().min(bytes.len());
                bytes[..length].copy_from_slice(&source[..length]);
            }
            return Ok(bytes);
        }
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

    pub fn flush(&self, start: u64, end: u64) -> std::io::Result<()> {
        let file = self.file.borrow();
        let Some(file) = file.as_ref() else {
            return Ok(());
        };
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

impl FileSection {
    pub fn anonymous() -> Rc<Self> {
        Rc::new(Self {
            file: RefCell::new(None),
            virtual_data: None,
            optical_data: None,
            file_len: Cell::new(0),
            writable: Cell::new(false),
            pages: RefCell::new(BTreeMap::new()),
        })
    }

    fn from_file(file: File, len: u64, writable: bool) -> Rc<Self> {
        Rc::new(Self {
            file: RefCell::new(Some(file)),
            virtual_data: None,
            optical_data: None,
            file_len: Cell::new(len),
            writable: Cell::new(writable),
            pages: RefCell::new(BTreeMap::new()),
        })
    }

    pub fn prune_pages(&self) {
        self.pages
            .borrow_mut()
            .retain(|_, page| page.dirty() || Rc::strong_count(page) > 1);
    }

    /// Overlay copy-up is an explicit identity transfer, not a path-cache hit.
    fn retarget(&self, file: File, len: u64, writable: bool) {
        *self.file.borrow_mut() = Some(file);
        self.file_len.set(len);
        self.writable.set(writable);
    }

    /// Preserve mapped dirty bytes without flushing or changing the file cursor.
    /// Clean bytes came from the actual file read; untouched lazy pages need no I/O.
    pub fn merge_read(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        if output.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        for (&index, page) in self.pages.borrow().range(offset / PAGE..end.div_ceil(PAGE)) {
            if !page.dirty() {
                continue;
            }
            let begin = offset.max(index * PAGE);
            let stop = end.min((index * PAGE).saturating_add(PAGE));
            page.read_into(
                (begin - index * PAGE) as usize,
                &mut output[(begin - offset) as usize..(stop - offset) as usize],
            )
            .map_err(io::Error::other)?;
        }
        Ok(())
    }

    /// Publish only bytes already written successfully. Initialized pages have
    /// stable worker-owned memory, so no late lazy I/O can follow the file write.
    /// A page's existing dirty neighbors survive; a clean page stays clean.
    pub fn publish_write(&self, offset: u64, input: &[u8]) {
        if input.is_empty() {
            return;
        }
        let end = offset
            .checked_add(input.len() as u64)
            .expect("validated file write span");
        self.file_len.set(self.file_len.get().max(end));
        for (&index, page) in self.pages.borrow().range(offset / PAGE..end.div_ceil(PAGE)) {
            if !page.initialized() {
                continue;
            }
            let begin = offset.max(index * PAGE);
            let stop = end.min((index * PAGE).saturating_add(PAGE));
            let dirty = page.dirty();
            page.write(
                (begin - index * PAGE) as usize,
                &input[(begin - offset) as usize..(stop - offset) as usize],
            )
            .expect("initialized shared page cannot perform late I/O");
            if !dirty {
                page.clear_dirty();
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct SharedFiles {
    files: Rc<RefCell<HashMap<FileIdentity, Weak<FileSection>>>>,
}

impl SharedFiles {
    pub fn has_entries(&self) -> bool {
        !self.files.borrow().is_empty()
    }
    pub fn lookup(&self, id: FileIdentity) -> Option<Rc<FileSection>> {
        self.files.borrow().get(&id).and_then(Weak::upgrade)
    }

    pub fn file(&self, file: File, len: u64, writable: bool) -> io::Result<Rc<FileSection>> {
        let id = file_identity(&file)?;
        self.transfer(id, file, len, writable)
    }

    /// Both old and overlay host identities refer to one guest file instance.
    /// A truly different file at the same pathname has no transfer and no hit.
    pub fn transfer(
        &self,
        old: FileIdentity,
        file: File,
        len: u64,
        writable: bool,
    ) -> io::Result<Rc<FileSection>> {
        let new = file_identity(&file)?;
        self.prune();
        let section = if let Some(section) = self.lookup(old) {
            if writable {
                section.retarget(file, len, true);
            }
            section
        } else {
            FileSection::from_file(file, len, writable)
        };
        let mut files = self.files.borrow_mut();
        files.insert(old, Rc::downgrade(&section));
        files.insert(new, Rc::downgrade(&section));
        Ok(section)
    }

    pub fn memory(&self, data: Arc<[u8]>) -> Rc<FileSection> {
        let id = (u64::MAX, data.as_ptr() as usize as u64);
        self.prune();
        if let Some(section) = self.lookup(id) {
            return section;
        }
        let section = Rc::new(FileSection {
            file: RefCell::new(None),
            file_len: Cell::new(data.len() as u64),
            virtual_data: Some(data),
            optical_data: None,
            writable: Cell::new(false),
            pages: RefCell::new(BTreeMap::new()),
        });
        self.files.borrow_mut().insert(id, Rc::downgrade(&section));
        section
    }

    /// Optical pages retain a bounded file owner and load only requested ranges.
    pub fn optical(&self, file: super::OpticalFile) -> Rc<FileSection> {
        let id = file.identity();
        self.prune();
        if let Some(section) = self.lookup(id) {
            return section;
        }
        let section = Rc::new(FileSection {
            file: RefCell::new(None),
            virtual_data: None,
            file_len: Cell::new(file.len()),
            optical_data: Some(file),
            writable: Cell::new(false),
            pages: RefCell::new(BTreeMap::new()),
        });
        self.files.borrow_mut().insert(id, Rc::downgrade(&section));
        section
    }

    pub fn prune(&self) {
        self.files
            .borrow_mut()
            .retain(|_, section| section.strong_count() != 0);
    }

    pub fn prune_pages(&self) {
        self.prune();
        for section in self.files.borrow().values().filter_map(Weak::upgrade) {
            section.prune_pages();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_short_write_prefix_stays_coherent_when_later_io_fails() {
        struct ShortWrite {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for ShortWrite {
            fn write(&mut self, input: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls == 1 {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if self.calls == 4 {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                let count = input.len().min(2);
                self.bytes.extend_from_slice(&input[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let section = FileSection::anonymous();
        let page = section.page(0);
        page.prepare().unwrap();
        page.write(7, &[0x7a]).unwrap(); // Existing unflushed neighbor.
        let mut writer = ShortWrite {
            bytes: Vec::new(),
            calls: 0,
        };
        assert!(
            section
                .write_to(&mut writer, 1, &[1, 2, 3, 4, 5, 6])
                .is_err()
        );
        assert_eq!(writer.bytes, [1, 2, 3, 4]);
        assert_eq!(&page.snapshot().unwrap()[..8], &[0, 1, 2, 3, 4, 0, 0, 0x7a]);
        assert!(page.dirty());
        let mut read = [0; 8];
        section.merge_read(0, &mut read).unwrap();
        assert_eq!(read, [0, 1, 2, 3, 4, 0, 0, 0x7a]);
    }

    #[test]
    fn ordinary_write_updates_clean_pages_without_io_or_materializing_lazy_pages() {
        let section = FileSection::anonymous();
        let ready = section.page(0);
        ready.prepare().unwrap();
        let lazy = section.page(1);
        section.publish_write(4094, &[1, 2, 3, 4]);
        assert_eq!(&ready.snapshot().unwrap()[4094..], &[1, 2]);
        assert!(!ready.dirty());
        assert!(!lazy.initialized());
        // No external write was performed for this anonymous test page, so the
        // untouched lazy page has its original zero initializer, not a snapshot.
        section.publish_write(u64::MAX, &[]);
        section.merge_read(u64::MAX, &mut []).unwrap();
    }

    #[test]
    fn explicit_overlay_identity_transfer_preserves_pages_and_weak_cache_does_not_own_them() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shared-file-section-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(format!("old-{}.bin", std::process::id()));
        let new = dir.join(format!("new-{}.bin", std::process::id()));
        std::fs::write(&old, [0x11u8; 8192]).unwrap();
        std::fs::write(&new, [0x11u8; 8192]).unwrap();
        let source = File::open(&old).unwrap();
        let old_id = file_identity(&source).unwrap();
        let files = SharedFiles::default();
        let section = files.file(source, 8192, false).unwrap();
        let page = section.page(0);
        assert_eq!(page.snapshot().unwrap()[0], 0x11);
        let target = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&new)
            .unwrap();
        let new_id = file_identity(&target).unwrap();
        let transferred = files.transfer(old_id, target, 8192, true).unwrap();
        assert!(Rc::ptr_eq(&section, &transferred));
        assert!(Rc::ptr_eq(&section, &files.lookup(new_id).unwrap()));
        transferred.publish_write(0, &[0x22]);
        assert_eq!(page.snapshot().unwrap()[0], 0x22);
        assert_eq!(std::fs::read(&old).unwrap()[0], 0x11);
        let weak = Rc::downgrade(&section);
        drop(transferred);
        drop(section);
        assert!(weak.upgrade().is_none());
        // Dead entries may remain until a lifecycle operation prunes them;
        // the ordinary-I/O lookup still cannot revive or own their content.
        assert!(files.has_entries());
        assert!(files.lookup(old_id).is_none());
        assert!(files.lookup(new_id).is_none());
        files.prune();
        assert!(files.files.borrow().is_empty());
        assert!(!files.has_entries());
        std::fs::remove_file(old).unwrap();
        std::fs::remove_file(new).unwrap();
    }

    #[test]
    fn a_different_file_instance_never_reuses_a_previous_path_identity() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/shared-file-section-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("replaced-{}.bin", std::process::id()));
        std::fs::write(&path, [0x11; 4096]).unwrap();
        let files = SharedFiles::default();
        let old = files.file(File::open(&path).unwrap(), 4096, false).unwrap();
        old.page(0).prepare().unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, [0x22; 4096]).unwrap();
        let new = files.file(File::open(&path).unwrap(), 4096, false).unwrap();
        assert!(!Rc::ptr_eq(&old, &new));
        assert_eq!(old.page(0).snapshot().unwrap()[0], 0x11);
        assert_eq!(new.page(0).snapshot().unwrap()[0], 0x22);
        drop(old);
        drop(new);
        files.prune();
        assert!(files.files.borrow().is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_flush_keeps_all_dirty_pages_and_cursor_for_retry() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/mapping-flush-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("retry-{}.bin", std::process::id()));
        std::fs::write(&path, [0u8; 8192]).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let fail = Rc::new(Cell::new(true));
        let injected = fail.clone();
        let first = FilePage::new([0x11; 4096]);
        first.mark_dirty();
        let second = FilePage::lazy(move || {
            if injected.get() {
                Err("second page snapshot failure".into())
            } else {
                Ok([0x22; 4096])
            }
        });
        second.mark_dirty();
        let backing = FileSection {
            file: RefCell::new(Some(file)),
            virtual_data: None,
            optical_data: None,
            file_len: Cell::new(8192),
            writable: Cell::new(true),
            pages: RefCell::new(BTreeMap::from([(0, first.clone()), (1, second.clone())])),
        };
        let retained = backing.file.borrow();
        retained
            .as_ref()
            .unwrap()
            .seek(SeekFrom::Start(17))
            .unwrap();
        drop(retained);
        assert!(backing.flush(0, 8192).is_err());
        assert!(first.dirty() && second.dirty());
        assert_eq!(
            backing
                .file
                .borrow()
                .as_ref()
                .unwrap()
                .stream_position()
                .unwrap(),
            17
        );
        fail.set(false);
        backing.flush(0, 8192).unwrap();
        assert!(!first.dirty() && !second.dirty());
        assert_eq!(
            backing
                .file
                .borrow()
                .as_ref()
                .unwrap()
                .stream_position()
                .unwrap(),
            17
        );
        let output = std::fs::read(&path).unwrap();
        assert_eq!(&output[..4096], &[0x11; 4096]);
        assert_eq!(&output[4096..], &[0x22; 4096]);
        drop(backing);
        std::fs::remove_file(path).unwrap();
    }
}
