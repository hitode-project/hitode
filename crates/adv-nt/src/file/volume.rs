//! Read-only volumes supplied by the assembly, independent of their image format.
//!
//! Sparse identity files give wineserver ordinary file handles, sharing and PE
//! section metadata. They contain no medium data. Reads and mapped pages come
//! from the retained source, including when a handle reaches another process.
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    prepared: RefCell<HashSet<String>>,
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

/// The host folder behind `C:\Program Files`. A folder the player chose
/// also holds the folders setups make at the root of drive C:, which a
/// record in it names for later sessions (WINE-SES-15).
pub struct Install {
    folder: PathBuf,
    roots: Option<Roots>,
}
struct Roots {
    /// Names at the root of the prefix's drive C:, which stay there.
    kept: Vec<String>,
    /// Root folders of drive C: that lie in the chosen folder.
    moved: RefCell<Vec<String>>,
}
/// Lists, one per line, the root folders of drive C: a chosen folder holds.
const RECORD: &str = ".hitode-drive-c";
/// Wine's own folders at the root of drive C:, before wineboot makes them.
const SYSTEM: [&str; 5] = [
    "windows",
    "users",
    "ProgramData",
    "Program Files",
    "Program Files (x86)",
];

/// `component` names `long` itself or its short name.
fn names(component: &str, long: &str) -> bool {
    component.eq_ignore_ascii_case(long)
        || component.eq_ignore_ascii_case(&String::from_utf16_lossy(&path::short_name(
            &long.encode_utf16().collect::<Vec<_>>(),
        )))
}

impl Install {
    /// `C:\Program Files` alone, as a bundled program's folder.
    pub fn program(folder: PathBuf) -> Self {
        Self {
            folder,
            roots: None,
        }
    }
    /// The folder the player chose, over the prefix's drive C: at `drive_c`.
    pub fn chosen(folder: PathBuf, drive_c: &Path) -> Self {
        let mut kept: Vec<String> = SYSTEM.iter().map(|name| (*name).to_owned()).collect();
        kept.extend(
            std::fs::read_dir(drive_c)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok()),
        );
        let moved = std::fs::read_to_string(folder.join(RECORD))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            folder,
            roots: Some(Roots {
                kept,
                moved: RefCell::new(moved),
            }),
        }
    }
    pub fn folder(&self) -> &Path {
        &self.folder
    }
    /// The root folder of drive C: named `first` lies in the chosen folder.
    pub fn holds_root(&self, first: &str) -> bool {
        self.roots
            .as_ref()
            .is_some_and(|roots| roots.moved.borrow().iter().any(|moved| names(first, moved)))
    }
    /// The host path of `C:\{first}\{tail}` when this folder holds it. A
    /// new root folder is recorded before it is made; a name the folder holds
    /// without a record came from Program Files and stays in the prefix.
    fn host(&self, first: &str, tail: &str) -> Option<PathBuf> {
        if names(first, "Program Files") {
            return Some(self.folder.join(tail));
        }
        let roots = self.roots.as_ref()?;
        if first.is_empty() || roots.kept.iter().any(|kept| names(first, kept)) {
            return None;
        }
        if !self.holds_root(first) {
            let taken = std::fs::read_dir(&self.folder)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| names(first, name))
                });
            if taken {
                return None;
            }
            let mut moved = roots.moved.borrow_mut();
            moved.push(first.to_owned());
            std::fs::create_dir_all(&self.folder)
                .and_then(|()| std::fs::write(self.folder.join(RECORD), moved.join("\n") + "\n"))
                .ok()?;
        }
        Some(self.folder.join(first).join(tail))
    }
    /// The DOS path of `host` in this folder.
    pub fn dos(&self, host: &Path) -> Option<String> {
        let relative = host.strip_prefix(&self.folder).ok()?;
        let relative = relative.to_str()?.replace('/', "\\");
        let first = relative.split('\\').next().unwrap_or_default();
        Some(if !first.is_empty() && self.holds_root(first) {
            format!(r"C:\{relative}")
        } else if relative.is_empty() {
            r"C:\Program Files".into()
        } else {
            format!(r"C:\Program Files\{relative}")
        })
    }
}

/// A family shares its mount namespace and immutable file identities.
#[derive(Clone, Default)]
pub struct Mounts(Rc<Namespace>);
#[derive(Default)]
struct Namespace {
    registered: Cell<bool>,
    install: Option<Install>,
    root: Option<PathBuf>,
    /// A drive's medium changes in place (WINE-SES-13); open files keep theirs.
    volumes: RefCell<BTreeMap<char, Rc<Mounted>>>,
    /// Roots of media taken out, which files opened on them still reach.
    removed: RefCell<Vec<PathBuf>>,
    identities: RefCell<HashMap<(u64, u64), Rc<Identity>>>,
    replaced: Vec<Replaced>,
}
/// A host file the guest sees in place of another (NT-FILE-5).
struct Replaced {
    identity: (u64, u64),
    original: PathBuf,
    file: PathBuf,
}
impl Namespace {
    fn mount(&self, root: PathBuf, source: Rc<dyn Volume>) -> io::Result<Rc<Mounted>> {
        std::fs::create_dir(&root)?;
        let metadata = std::fs::metadata(&root)?;
        let entry = source.entry("")?;
        self.identities.borrow_mut().insert(
            (metadata.dev(), metadata.ino()),
            Rc::new(Identity {
                entry,
                path: String::new(),
                source: source.clone(),
                reader: RefCell::new(None),
            }),
        );
        Ok(Rc::new(Mounted {
            root,
            source,
            prepared: RefCell::new(HashSet::new()),
        }))
    }
}
impl Drop for Namespace {
    fn drop(&mut self) {
        if let Some(root) = &self.root {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
impl Mounts {
    pub fn new(install: Option<Install>, root: Option<PathBuf>) -> Self {
        Self(Rc::new(Namespace {
            registered: Cell::new(false),
            install,
            root,
            volumes: RefCell::new(BTreeMap::new()),
            removed: RefCell::new(Vec::new()),
            identities: RefCell::new(HashMap::new()),
            replaced: Vec::new(),
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
        let mounted = ns.mount(root, source)?;
        ns.volumes.get_mut().insert(letter, mounted);
        Ok(())
    }
    /// Puts another medium in drive `letter` and returns the directory it
    /// shows under. Files opened before keep reading the medium they were
    /// opened on.
    pub fn change(&self, letter: char, source: Rc<dyn Volume>) -> io::Result<PathBuf> {
        let (Some(files), true) = (&self.0.root, self.0.volumes.borrow().contains_key(&letter))
        else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no disc drive {letter}:"),
            ));
        };
        let changes = self.0.removed.borrow().len();
        let root = files.join(format!("{letter}.{}", changes + 1));
        let mounted = self.0.mount(root.clone(), source)?;
        if let Some(out) = self.0.volumes.borrow_mut().insert(letter, mounted) {
            self.0.removed.borrow_mut().push(out.root.clone());
        }
        Ok(root)
    }
    /// Opens that resolve to `original` reach `file` instead (NT-FILE-5).
    /// Called before the namespace is shared with any process.
    pub fn replace(&mut self, original: &Path, file: PathBuf) -> io::Result<()> {
        let ns = Rc::get_mut(&mut self.0).expect("mounts precede process startup");
        let metadata = std::fs::metadata(original)?;
        ns.replaced.push(Replaced {
            identity: (metadata.dev(), metadata.ino()),
            original: original.to_owned(),
            file,
        });
        Ok(())
    }
    /// The file that stands in for `host`, matched by identity so that every
    /// spelling of its name resolves alike.
    pub(crate) fn replacement(&self, host: &Path) -> Option<PathBuf> {
        if self.0.replaced.is_empty() {
            return None;
        }
        let metadata = std::fs::metadata(host).ok()?;
        let identity = (metadata.dev(), metadata.ino());
        self.0
            .replaced
            .iter()
            .find(|replaced| replaced.identity == identity)
            .map(|replaced| replaced.file.clone())
    }
    /// The original a replacement stands in for, to name it to the guest.
    pub(crate) fn original<'a>(&'a self, host: &'a Path) -> &'a Path {
        self.0
            .replaced
            .iter()
            .find(|replaced| replaced.file == host)
            .map_or(host, |replaced| &replaced.original)
    }
    pub fn device_name(&self, letter: char) -> Option<String> {
        self.0
            .volumes
            .borrow()
            .keys()
            .position(|mounted| *mounted == letter)
            .map(|index| format!(r"\Device\CdRom{index}"))
    }
    pub fn has_discs(&self) -> bool {
        !self.0.volumes.borrow().is_empty()
    }
    /// Each drive and the directory its medium shows under.
    pub fn roots(&self) -> Vec<(char, PathBuf)> {
        self.0
            .volumes
            .borrow()
            .iter()
            .map(|(letter, mount)| (*letter, mount.root.clone()))
            .collect()
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
            if let Some(host) = install.host(first, tail) {
                let name = format!(r"\??\unix{}", host.to_string_lossy().replace('/', "\\"));
                return Some(path::lookup(
                    devices,
                    &name.encode_utf16().collect::<Vec<_>>(),
                ));
            }
        }
        let mount = self.0.volumes.borrow().get(&letter)?.clone();
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
                self.identity(&mount, &host, &source, entry)
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
        let found = self.0.volumes.borrow().values().find_map(|mount| {
            host.strip_prefix(&mount.root)
                .ok()
                .map(|relative| (mount.clone(), relative.to_string_lossy().replace('/', "\\")))
        });
        let Some((mount, relative)) = found else {
            return Ok(());
        };
        if mount.prepared.borrow().contains(&relative) {
            return Ok(());
        }
        for entry in mount.source.list(&relative)? {
            let path = if relative.is_empty() {
                entry.name.clone()
            } else {
                format!("{relative}\\{}", entry.name)
            };
            self.identity(&mount, &host.join(&entry.name), &path, entry)?;
        }
        mount.prepared.borrow_mut().insert(relative);
        Ok(())
    }
    pub fn read_only(&self, host: &Path) -> bool {
        self.0
            .volumes
            .borrow()
            .values()
            .any(|mount| host.starts_with(&mount.root))
            || self
                .0
                .removed
                .borrow()
                .iter()
                .any(|root| host.starts_with(root))
    }
    pub fn nt_name(&self, host: &Path) -> Option<String> {
        Some(format!(r"\??\{}", self.0.install.as_ref()?.dos(host)?))
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
    pub(crate) fn file_standard(&self, fd: RawFd) -> Option<[u8; 22]> {
        let identity = self.of_fd(fd).filter(|file| !file.entry.directory)?;
        // SAFETY: borrow the descriptor during its cold admission.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        let metadata = file.metadata().ok()?;
        let mut bytes = [0; 22];
        bytes[..8].copy_from_slice(&identity.entry.allocated.to_le_bytes());
        bytes[8..16].copy_from_slice(&identity.entry.size.to_le_bytes());
        bytes[16..20].copy_from_slice(&(metadata.nlink() as u32).to_le_bytes());
        Some(bytes)
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

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-SES-15
    #[test]
    fn a_chosen_folder_takes_new_root_folders_of_drive_c_and_names_them_there() {
        let root = std::env::temp_dir().join(format!("adv-install-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (drive_c, folder) = (root.join("drive_c"), root.join("chosen"));
        std::fs::create_dir_all(drive_c.join("Old")).unwrap();
        std::fs::create_dir_all(folder.join("Vendor")).unwrap();
        let install = Install::chosen(folder.clone(), &drive_c);
        // Program Files, by its long or short name.
        let short = String::from_utf16_lossy(&path::short_name(
            &"Program Files".encode_utf16().collect::<Vec<_>>(),
        ));
        for name in ["program files", short.as_str()] {
            assert_eq!(install.host(name, "Vendor"), Some(folder.join("Vendor")));
        }
        // Wine's folders, the prefix's own root names and Program Files'
        // folders stay where they are.
        for kept in ["windows", "old", "vendor", ""] {
            assert_eq!(install.host(kept, "x"), None, "{kept}");
        }
        assert_eq!(
            install.host("Studio", "a.txt"),
            Some(folder.join("Studio/a.txt"))
        );
        assert_eq!(
            std::fs::read_to_string(folder.join(RECORD)).unwrap(),
            "Studio\n"
        );
        std::fs::create_dir_all(folder.join("Studio")).unwrap();
        // A later session names both folders as their setups did.
        let later = Install::chosen(folder.clone(), &drive_c);
        assert_eq!(later.host("STUDIO", "b"), Some(folder.join("STUDIO/b")));
        assert_eq!(
            later.dos(&folder.join("Studio/a.txt")).as_deref(),
            Some(r"C:\Studio\a.txt") // privacy: allow authored guest path
        );
        assert_eq!(
            later.dos(&folder.join("Vendor/game.exe")).as_deref(),
            Some(r"C:\Program Files\Vendor\game.exe")
        );
        assert_eq!(later.dos(&folder).as_deref(), Some(r"C:\Program Files"));
        // A bundled program's folder is Program Files alone.
        assert_eq!(
            Install::program(folder.clone()).host("Studio", "a.txt"),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // spec: NT-FILE-5
    #[test]
    fn a_replaced_file_answers_every_spelling_and_keeps_its_name() {
        let root = std::env::temp_dir().join(format!("adv-nt-replaced-{}", std::process::id()));
        let game = root.join("game");
        std::fs::create_dir_all(&game).unwrap();
        let original = game.join("Config.ini");
        std::fs::write(&original, "original").unwrap();
        std::fs::write(game.join("other.ini"), "other").unwrap();
        let file = root.join("replaced.ini");
        std::fs::write(&file, "replaced").unwrap();
        // A drive link reaches the same file under another host path.
        let drive = root.join("d:");
        std::os::unix::fs::symlink(&game, &drive).unwrap();
        let mut mounts = Mounts::default();
        mounts.replace(&original, file.clone()).unwrap();

        assert_eq!(
            mounts.replacement(&drive.join("Config.ini")),
            Some(file.clone())
        );
        assert_eq!(mounts.replacement(&original), Some(file.clone()));
        assert_eq!(mounts.replacement(&drive.join("other.ini")), None);
        assert_eq!(mounts.original(&file), original);
        assert_eq!(mounts.original(&drive), drive);
        std::fs::remove_dir_all(root).unwrap();
    }

    // spec: WINE-SES-13
    #[test]
    fn prepare_directory_caches_immutable_mount_and_resets_on_disc_change() {
        struct CountingVolume {
            entries: Vec<Entry>,
            list_calls: Rc<Cell<usize>>,
        }
        impl Volume for CountingVolume {
            fn info(&self) -> Info {
                Info {
                    label: "DISC".into(),
                    serial: 1,
                    sector_size: 2048,
                    total_bytes: 2048 * 512,
                    filesystem: "CDFS".into(),
                    flags: 0,
                    max_name: 255,
                }
            }
            fn entry(&self, path: &str) -> io::Result<Entry> {
                if path.is_empty() {
                    return Ok(Entry {
                        name: String::new(),
                        directory: true,
                        size: 0,
                        allocated: 0,
                        created: None,
                        accessed: None,
                        modified: None,
                        attributes: 0x10,
                    });
                }
                self.entries
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(path))
                    .cloned()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
            }
            fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
                assert_eq!(path, "");
                self.list_calls.set(self.list_calls.get() + 1);
                Ok(self.entries.clone())
            }
            fn open(&self, _path: &str) -> io::Result<Rc<dyn ReadFile>> {
                Err(io::Error::from(io::ErrorKind::Unsupported))
            }
        }

        let root = std::env::temp_dir().join(format!("adv-nt-volume-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let make_entries = |prefix: &str, count: usize| -> Vec<Entry> {
            (0..count)
                .map(|i| Entry {
                    name: format!("{prefix}_{i:04}.dat"),
                    directory: false,
                    size: 4,
                    allocated: 2048,
                    created: None,
                    accessed: None,
                    modified: None,
                    attributes: 0x20,
                })
                .collect()
        };

        let disc1_calls = Rc::new(Cell::new(0));
        let disc1: Rc<dyn Volume> = Rc::new(CountingVolume {
            entries: make_entries("disc1", 512),
            list_calls: disc1_calls.clone(),
        });
        let mount_root = root.join("D");
        let mut mounts = Mounts::new(None, Some(root.clone()));
        mounts.attach('D', mount_root.clone(), disc1).unwrap();

        for _ in 0..513 {
            mounts.prepare_directory(&mount_root).unwrap();
        }
        assert_eq!(disc1_calls.get(), 1);
        assert!(mount_root.join("disc1_0511.dat").exists());

        // Swapping the disc creates a fresh Mounted instance whose directories prepare once.
        let disc2_calls = Rc::new(Cell::new(0));
        let disc2: Rc<dyn Volume> = Rc::new(CountingVolume {
            entries: make_entries("disc2", 8),
            list_calls: disc2_calls.clone(),
        });
        let disc2_root = mounts.change('D', disc2).unwrap();
        for _ in 0..10 {
            mounts.prepare_directory(&disc2_root).unwrap();
        }
        assert_eq!(disc2_calls.get(), 1);
        assert!(disc2_root.join("disc2_0007.dat").exists());

        drop(mounts);
        let _ = std::fs::remove_dir_all(&root);
    }
}
