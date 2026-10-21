//! Authoritative case-insensitive index of the host directories behind the VFS.
//! Guest-visible entries change only through `vfs::host`, which invalidates the
//! directories it touches, so a lookup never revalidates against the host.
//! Read-only layers do not change while a game runs (WIN32-OBJECT-8).
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

const MAX_DIRECTORIES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Dir,
    /// Symbolic links and other entries resolve through the host on use.
    Other,
}

/// Folded name to the entries spelled that way, in enumeration order.
/// `None` records a directory that does not exist or cannot be listed.
type Listing = Option<HashMap<Vec<u16>, Vec<(OsString, Kind)>>>;

#[derive(Default)]
struct Index {
    /// Bumped by every invalidation, so a listing read concurrently with a
    /// change is used once but never stored.
    generation: u64,
    dirs: HashMap<PathBuf, Arc<Listing>>,
    /// Directories whose entries change outside a call, e.g. delete-on-close.
    volatile: Vec<PathBuf>,
}

static INDEX: LazyLock<Mutex<Index>> = LazyLock::new(Mutex::default);
/// Counts answers the index does not hold: volatile or racing listings and
/// entries that resolve through the host. Callers memoize only without them.
static UNCACHED: AtomicU64 = AtomicU64::new(0);

pub(super) fn generation() -> u64 {
    INDEX.lock().unwrap().generation
}

pub(super) fn uncached() -> u64 {
    UNCACHED.load(Ordering::Relaxed)
}

fn key(name: &str) -> Vec<u16> {
    name.encode_utf16()
        .map(|unit| crate::nls::unit_case(unit, true))
        .collect()
}

fn read(dir: &Path) -> Listing {
    let mut names: HashMap<Vec<u16>, Vec<(OsString, Kind)>> = HashMap::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let kind = match entry.file_type() {
            Ok(kind) if kind.is_dir() => Kind::Dir,
            Ok(kind) if kind.is_file() => Kind::File,
            _ => Kind::Other,
        };
        let name = entry.file_name();
        names
            .entry(key(&name.to_string_lossy()))
            .or_default()
            .push((name, kind));
    }
    Some(names)
}

fn listing(dir: &Path) -> Arc<Listing> {
    let generation = {
        let index = INDEX.lock().unwrap();
        if let Some(listing) = index.dirs.get(dir) {
            return listing.clone();
        }
        index.generation
    };
    let listing = Arc::new(read(dir));
    let mut index = INDEX.lock().unwrap();
    if index.generation == generation && !index.volatile.iter().any(|v| v == dir) {
        if index.dirs.len() >= MAX_DIRECTORIES {
            index.dirs.clear();
        }
        index.dirs.insert(dir.to_owned(), listing.clone());
    } else {
        UNCACHED.fetch_add(1, Ordering::Relaxed);
    }
    listing
}

/// Windows-only filesystem aliases that cannot be answered from directory
/// entries. Unicode spelling belongs to the shared invariant name index.
fn host_resolved(name: &str) -> bool {
    cfg!(windows) && (name.contains('~') || name.ends_with(['.', ' ']))
}

/// Host entry in `dir` whose name matches the shared Windows invariant casing.
/// A case-sensitive host must not resolve Unicode paths with different rules.
pub(super) fn find(dir: &Path, name: &str) -> Option<(PathBuf, Kind)> {
    if host_resolved(name) {
        if INDEX.lock().unwrap().volatile.iter().any(|v| v == dir) {
            UNCACHED.fetch_add(1, Ordering::Relaxed);
        }
        let path = dir.join(name);
        let metadata = std::fs::metadata(&path).ok()?;
        let kind = if metadata.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        return Some((path, kind));
    }
    let listing = listing(dir);
    let spellings = listing.as_ref().as_ref()?.get(&key(name))?;
    // Exact spelling wins if two host entries differ only by case.
    let (spelling, kind) = spellings
        .iter()
        .find(|(spelling, _)| spelling.as_os_str() == name)
        .unwrap_or(&spellings[0]);
    if *kind == Kind::Other {
        UNCACHED.fetch_add(1, Ordering::Relaxed);
    }
    Some((dir.join(spelling), *kind))
}

/// Forget `path`, its parent and everything cached below it.
pub(super) fn invalidate(path: &Path) {
    let mut index = INDEX.lock().unwrap();
    index.generation += 1;
    let parent = path.parent();
    index
        .dirs
        .retain(|dir, _| !dir.starts_with(path) && Some(dir.as_path()) != parent);
}

/// Start a run from the host's current state: the trees may have changed
/// between runs hosted by the same process.
pub(super) fn clear() {
    #[cfg(all(test, unix))]
    let _held = tests::EXCLUSIVE.lock().unwrap_or_else(|e| e.into_inner());
    let mut index = INDEX.lock().unwrap();
    index.generation += 1;
    index.dirs.clear();
    index.volatile.clear();
}

/// Never cache `dir` again: its entries change at times no call observes.
pub(super) fn volatile(dir: &Path) {
    invalidate(&dir.join("."));
    let mut index = INDEX.lock().unwrap();
    if !index.volatile.iter().any(|v| v == dir) {
        index.volatile.push(dir.to_owned());
    }
}

#[cfg(all(test, unix))]
pub(super) mod tests {
    use super::*;

    /// Parallel tests construct Vfs instances, whose `clear` would drop the
    /// cached listings this test relies on to ignore unannounced changes.
    pub(in crate::vfs) static EXCLUSIVE: Mutex<()> = Mutex::new(());

    #[test]
    // spec: WIN32-OBJECT-8, PERS-FILE-1
    fn the_index_answers_until_its_directory_is_invalidated() {
        let _held = EXCLUSIVE.lock().unwrap_or_else(|e| e.into_inner());
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/vfs-names")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Data.PCK"), b"").unwrap();
        let file = |path: &Path| Some((path.to_owned(), Kind::File));
        assert_eq!(find(&root, "data.pck"), file(&root.join("Data.PCK")));
        assert_eq!(find(&root, "missing.g00"), None);
        // An unannounced host change is not observed; an announced one is.
        std::fs::write(root.join("Missing.G00"), b"").unwrap();
        assert_eq!(find(&root, "missing.g00"), None);
        invalidate(&root.join("Missing.G00"));
        assert_eq!(find(&root, "missing.g00"), file(&root.join("Missing.G00")));
        std::fs::remove_file(root.join("Data.PCK")).unwrap();
        invalidate(&root.join("Data.PCK"));
        assert_eq!(find(&root, "data.pck"), None);
        std::fs::write(root.join("Case"), b"upper").unwrap();
        std::fs::write(root.join("case"), b"lower").unwrap();
        invalidate(&root.join("case"));
        // Exact spelling wins on case-sensitive hosts.
        for name in ["Case", "case"] {
            assert_eq!(
                std::fs::read(find(&root, name).unwrap().0).unwrap(),
                std::fs::read(root.join(name)).unwrap()
            );
        }
        std::os::unix::fs::symlink("absent", root.join("broken")).unwrap();
        invalidate(&root.join("broken"));
        assert_eq!(find(&root, "broken").unwrap().1, Kind::Other);
        std::fs::create_dir(root.join("Sub")).unwrap();
        invalidate(&root.join("Sub"));
        assert_eq!(find(&root, "sub").unwrap().1, Kind::Dir);
        assert_eq!(find(&root.join("Sub"), "x"), None);
        // A volatile directory is read on every lookup.
        volatile(&root.join("Sub"));
        std::fs::write(root.join("Sub/X"), b"").unwrap();
        assert_eq!(find(&root.join("Sub"), "x"), file(&root.join("Sub/X")));
        assert_eq!(find(&root, "é.txt"), None);
        std::fs::write(root.join("É.TXT"), b"unicode").unwrap();
        assert_eq!(
            find(&root, "é.txt"),
            None,
            "Unicode host writes also stay invisible"
        );
        invalidate(&root.join("É.TXT"));
        assert_eq!(find(&root, "é.txt"), file(&root.join("É.TXT")));
        std::fs::write(root.join("表a.BIN"), b"mixed Unicode and ASCII").unwrap();
        invalidate(&root.join("表a.BIN"));
        assert_eq!(find(&root, "表A.bin"), file(&root.join("表a.BIN")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
