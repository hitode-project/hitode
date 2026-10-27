//! Read-only volumes supplied by the assembly, independent of their image format.
//!
//! Sparse identity files give wineserver ordinary file handles, sharing and PE
//! section metadata. They contain no medium data. Reads and mapped pages come
//! from the retained source, including when a handle reaches another process.
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use super::{Lookup, path, status};
use adv_cpu::backend::FilePage;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
    pub allocated: u64,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub attributes: u32,
}

impl Entry {
    pub(crate) fn basic(&self) -> [u8; 40] {
        let ticks = |time: Option<SystemTime>| -> u64 {
            let Some(time) = time else {
                return 0;
            };
            let delta = |duration: std::time::Duration| {
                duration.as_secs() * 10_000_000 + u64::from(duration.subsec_nanos()) / 100
            };
            let epoch = adv_machine::clock::UNIX_EPOCH_SYSTEM_TIME;
            match time.duration_since(SystemTime::UNIX_EPOCH) {
                Ok(duration) => epoch + delta(duration),
                Err(error) => epoch - delta(error.duration()),
            }
        };
        let mut bytes = [0; 40];
        for (index, time) in [self.created, self.accessed, self.modified, self.modified]
            .into_iter()
            .enumerate()
        {
            bytes[index * 8..index * 8 + 8].copy_from_slice(&ticks(time).to_le_bytes());
        }
        bytes[32..36].copy_from_slice(&self.attributes.to_le_bytes());
        bytes
    }
}

#[derive(Clone)]
pub struct Info {
    pub label: String,
    pub serial: u32,
    pub sector_size: u32,
    pub total_bytes: u64,
    pub filesystem: String,
    pub flags: u32,
    pub max_name: u32,
}

/// The content and lazy pages of one immutable file, retained by mappings.
pub trait ReadFile {
    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<usize>;
    fn page(&self, index: u64) -> Rc<FilePage>;
}

/// Paths are relative to the volume root, with backslash separators.
pub trait Volume {
    fn info(&self) -> Info;
    fn entry(&self, path: &str) -> io::Result<Entry>;
    fn list(&self, path: &str) -> io::Result<Vec<Entry>>;
    fn open(&self, path: &str) -> io::Result<Rc<dyn ReadFile>>;
}

struct Mounted {
    root: PathBuf,
    source: Rc<dyn Volume>,
}
struct Identity {
    entry: Entry,
    path: String,
    source: Rc<dyn Volume>,
    reader: RefCell<Option<Rc<dyn ReadFile>>>,
}
impl Identity {
    fn reader(&self) -> io::Result<Rc<dyn ReadFile>> {
        if let Some(reader) = self.reader.borrow().as_ref() {
            return Ok(reader.clone());
        }
        let reader = self.source.open(&self.path)?;
        *self.reader.borrow_mut() = Some(reader.clone());
        Ok(reader)
    }
}

/// A family shares its mount namespace and immutable file identities.
#[derive(Clone, Default)]
pub struct Mounts(Rc<Namespace>);
#[derive(Default)]
struct Namespace {
    registered: Cell<bool>,
    install: Option<PathBuf>,
    root: Option<PathBuf>,
    volumes: BTreeMap<char, Mounted>,
    identities: RefCell<HashMap<(u64, u64), Rc<Identity>>>,
}
impl Drop for Namespace {
    fn drop(&mut self) {
        if let Some(root) = &self.root {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
impl Mounts {
    pub fn new(install: Option<PathBuf>, root: Option<PathBuf>) -> Self {
        Self(Rc::new(Namespace {
            registered: Cell::new(false),
            install,
            root,
            volumes: BTreeMap::new(),
            identities: RefCell::new(HashMap::new()),
        }))
    }
    /// Publish this namespace once, shared by wineboot, the game and its children.
    pub(crate) fn begin_registration(&self) -> bool {
        !self.0.registered.replace(true)
    }
    /// Called before the namespace is shared with any process.
    pub fn attach(
        &mut self,
        letter: char,
        root: PathBuf,
        source: Rc<dyn Volume>,
    ) -> io::Result<()> {
        let ns = Rc::get_mut(&mut self.0).expect("mounts precede process startup");
        std::fs::create_dir(&root)?;
        let metadata = std::fs::metadata(&root)?;
        let entry = source.entry("")?;
        ns.identities.get_mut().insert(
            (metadata.dev(), metadata.ino()),
            Rc::new(Identity {
                entry,
                path: String::new(),
                source: source.clone(),
                reader: RefCell::new(None),
            }),
        );
        ns.volumes.insert(letter, Mounted { root, source });
        Ok(())
    }
    pub fn device_name(&self, letter: char) -> Option<String> {
        self.0
            .volumes
            .keys()
            .position(|mounted| *mounted == letter)
            .map(|index| format!(r"\Device\CdRom{index}"))
    }
    pub fn roots(&self) -> impl Iterator<Item = (char, &Path)> {
        self.0
            .volumes
            .iter()
            .map(|(letter, mount)| (*letter, mount.root.as_path()))
    }
    pub fn lookup(&self, devices: &Path, name: &[u16]) -> Option<Result<Lookup, u32>> {
        let text = String::from_utf16(name).ok()?;
        let rest = text.strip_prefix(r"\??\")?;
        let (drive, rest) = rest.split_once('\\').unwrap_or((rest, ""));
        if drive.len() != 2 || !drive.ends_with(':') {
            return None;
        }
        let letter = drive.as_bytes()[0].to_ascii_uppercase() as char;
        if letter == 'C'
            && let Some(install) = &self.0.install
        {
            let (first, tail) = rest.split_once('\\').unwrap_or((rest, ""));
            let short = path::short_name(&"Program Files".encode_utf16().collect::<Vec<_>>());
            if first.eq_ignore_ascii_case("Program Files")
                || first.eq_ignore_ascii_case(&String::from_utf16_lossy(&short))
            {
                let name = format!(
                    r"\??\unix{}\{tail}",
                    install.to_string_lossy().replace('/', "\\")
                );
                return Some(path::lookup(
                    devices,
                    &name.encode_utf16().collect::<Vec<_>>(),
                ));
            }
        }
        let mount = self.0.volumes.get(&letter)?;
        Some((|| {
            let rest = rest.strip_prefix('\\').unwrap_or(rest);
            path::validate_components(rest, false)?;
            let mut host = mount.root.clone();
            let mut source = String::new();
            let components: Vec<_> = rest
                .trim_end_matches('\\')
                .split('\\')
                .filter(|part| !part.is_empty())
                .collect();
            for (index, component) in components.iter().enumerate() {
                let requested = if source.is_empty() {
                    (*component).to_owned()
                } else {
                    format!("{source}\\{component}")
                };
                let entry = match mount.source.entry(&requested) {
                    Ok(entry) => entry,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let alias = mount.source.list(&source).ok().and_then(|entries| {
                            entries.into_iter().find(|entry| {
                                let short = path::short_name(
                                    &entry.name.encode_utf16().collect::<Vec<_>>(),
                                );
                                component.eq_ignore_ascii_case(&String::from_utf16_lossy(&short))
                            })
                        });
                        match alias {
                            Some(entry) => entry,
                            None if index + 1 == components.len() => {
                                return Ok(Lookup::Missing(host.join(component)));
                            }
                            None => return Err(status::OBJECT_PATH_NOT_FOUND),
                        }
                    }
                    Err(_) => return Err(status::OBJECT_PATH_NOT_FOUND),
                };
                if !source.is_empty() {
                    source.push('\\');
                }
                source.push_str(&entry.name);
                host.push(&entry.name);
                self.identity(mount, &host, &source, entry)
                    .map_err(|_| 0xc000_0022u32)?;
            }
            if rest.ends_with('\\') && !host.is_dir() {
                return Err(status::OBJECT_NAME_INVALID);
            }
            Ok(Lookup::Found(host))
        })())
    }
    fn identity(&self, mount: &Mounted, host: &Path, path: &str, entry: Entry) -> io::Result<()> {
        if host.exists() {
            return Ok(());
        }
        if entry.directory {
            std::fs::create_dir(host)?;
        } else {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(host)?;
            file.set_len(entry.size)?;
            let mut times = std::fs::FileTimes::new();
            if let Some(time) = entry.modified {
                times = times.set_modified(time);
            }
            if let Some(time) = entry.accessed {
                times = times.set_accessed(time);
            }
            file.set_times(times)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
        }
        let metadata = std::fs::metadata(host)?;
        self.0.identities.borrow_mut().insert(
            (metadata.dev(), metadata.ino()),
            Rc::new(Identity {
                entry,
                path: path.into(),
                source: mount.source.clone(),
                reader: RefCell::new(None),
            }),
        );
        Ok(())
    }
    pub fn prepare_directory(&self, host: &Path) -> io::Result<()> {
        for mount in self.0.volumes.values() {
            if let Ok(relative) = host.strip_prefix(&mount.root) {
                let relative = relative.to_string_lossy().replace('/', "\\");
                for entry in mount.source.list(&relative)? {
                    let path = if relative.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{relative}\\{}", entry.name)
                    };
                    self.identity(mount, &host.join(&entry.name), &path, entry)?;
                }
                break;
            }
        }
        Ok(())
    }
    pub fn read_only(&self, host: &Path) -> bool {
        self.0
            .volumes
            .values()
            .any(|mount| host.starts_with(&mount.root))
    }
    pub fn nt_name(&self, host: &Path) -> Option<String> {
        let install = self.0.install.as_ref()?;
        let relative = host.strip_prefix(install).ok()?;
        let relative = relative.to_string_lossy().replace('/', "\\");
        Some(if relative.is_empty() {
            r"\??\C:\Program Files".into()
        } else {
            format!(r"\??\C:\Program Files\{relative}")
        })
    }
    fn of_fd(&self, fd: RawFd) -> Option<Rc<Identity>> {
        if self.0.identities.borrow().is_empty() {
            return None;
        }
        // SAFETY: this borrows the server's live descriptor without closing it.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        let metadata = file.metadata().ok()?;
        self.0
            .identities
            .borrow()
            .get(&(metadata.dev(), metadata.ino()))
            .cloned()
    }
    pub fn reader(&self, fd: RawFd) -> Option<io::Result<Rc<dyn ReadFile>>> {
        self.of_fd(fd)
            .filter(|file| !file.entry.directory)
            .map(|file| file.reader())
    }
    pub fn info(&self, fd: RawFd) -> Option<Info> {
        self.of_fd(fd).map(|file| file.source.info())
    }
    pub fn metadata(&self, metadata: &std::fs::Metadata) -> Option<Entry> {
        self.0
            .identities
            .borrow()
            .get(&(metadata.dev(), metadata.ino()))
            .map(|file| file.entry.clone())
    }
}

pub const WRITE_PROTECTED: u32 = 0xc000_00a2;
pub(crate) fn mounts(env: &adv_machine::Env<'_>) -> Mounts {
    env.ext.try_get::<Mounts>().cloned().unwrap_or_default()
}

/// FILE_FS_* information from the same immutable volume identity as its files.
pub(super) fn information(
    info: &Info,
    class: u32,
    length: u32,
) -> Result<(Vec<u8>, u32), super::Error> {
    let mut bytes = match class {
        1 => {
            let label: Vec<_> = info
                .label
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut bytes = vec![0; 18];
            bytes[8..12].copy_from_slice(&info.serial.to_le_bytes());
            bytes[12..16].copy_from_slice(&(label.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&label);
            bytes
        }
        3 | 7 => {
            let mut bytes = vec![0; if class == 3 { 24 } else { 32 }];
            bytes[..8]
                .copy_from_slice(&(info.total_bytes / u64::from(info.sector_size)).to_le_bytes());
            let offset = bytes.len() - 8;
            bytes[offset..offset + 4].copy_from_slice(&1u32.to_le_bytes());
            bytes[offset + 4..].copy_from_slice(&info.sector_size.to_le_bytes());
            bytes
        }
        4 => [3u32, 3].into_iter().flat_map(u32::to_le_bytes).collect(),
        5 => {
            let name: Vec<_> = info
                .filesystem
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut bytes: Vec<_> = [info.flags, info.max_name, name.len() as u32]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            bytes.extend_from_slice(&name);
            bytes
        }
        _ => return Err(status::NOT_SUPPORTED.into()),
    };
    let fixed = match class {
        1 => 18,
        5 => 12,
        _ => bytes.len(),
    };
    if (length as usize) < fixed {
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    let status = if bytes.len() > length as usize {
        0x8000_0005
    } else {
        status::SUCCESS
    };
    bytes.truncate(length as usize);
    Ok((bytes, status))
}

pub fn read_all(mounts: &Mounts, fd: std::os::fd::RawFd) -> std::io::Result<Vec<u8>> {
    use std::os::fd::FromRawFd;
    // SAFETY: a descriptor kept open by the caller; ManuallyDrop leaves it open.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let size = file.metadata()?.len();
    let mut bytes = vec![0u8; size as usize];
    if let Some(reader) = mounts.reader(fd) {
        let reader = reader?;
        let mut read = 0;
        while read < bytes.len() {
            let count = reader.read_at(read as u64, &mut bytes[read..])?;
            if count == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            read += count;
        }
    } else {
        std::os::unix::fs::FileExt::read_exact_at(&*file, &mut bytes, 0)?;
    }
    Ok(bytes)
}
