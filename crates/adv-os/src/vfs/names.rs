//! Case-insensitive name index for host directories. Games probe for absent
//! files in directories with thousands of entries, and a fresh scan per probe
//! costs about a millisecond. An index is reused only while the directory's
//! modification time is unchanged, and only once that time is old enough that
//! a later change cannot share its timestamp.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_DIRECTORIES: usize = 256;
/// Coarser than the directory timestamp granularity of supported host file systems.
const SETTLED: Duration = Duration::from_secs(2);

struct Names {
    modified: SystemTime,
    /// ASCII-lowercased name to the first host spelling `read_dir` returned.
    names: HashMap<String, OsString>,
}

thread_local! {
    static INDEX: RefCell<HashMap<PathBuf, Names>> = RefCell::default();
}

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Host path of the entry in `dir` whose name matches `name` ASCII case-insensitively.
pub(super) fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
    let key = key(name);
    if let Some(modified) = modified {
        let cached = INDEX.with_borrow(|index| {
            let names = index.get(dir).filter(|names| names.modified == modified)?;
            Some(names.names.get(&key).map(|found| dir.join(found)))
        });
        if let Some(found) = cached {
            return found;
        }
    }
    let mut names = HashMap::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        names
            .entry(self::key(&name.to_string_lossy()))
            .or_insert(name);
    }
    let found = names.get(&key).map(|found| dir.join(found));
    let settled = modified.filter(|&modified| {
        SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age >= SETTLED)
    });
    if let Some(modified) = settled {
        INDEX.with_borrow_mut(|index| {
            if index.len() >= MAX_DIRECTORIES && !index.contains_key(dir) {
                index.clear();
            }
            index.insert(dir.to_owned(), Names { modified, names });
        });
    }
    found
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn settle(dir: &Path) {
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::open(dir).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn indexed_directories_follow_later_entries() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/vfs-names")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Data.PCK"), b"").unwrap();
        // A just-written directory is scanned but not indexed.
        assert_eq!(find(&root, "data.pck"), Some(root.join("Data.PCK")));
        assert!(INDEX.with_borrow(|index| !index.contains_key(&root)));
        settle(&root);
        assert_eq!(find(&root, "missing.g00"), None);
        assert!(INDEX.with_borrow(|index| index.contains_key(&root)));
        assert_eq!(find(&root, "DATA.pck"), Some(root.join("Data.PCK")));
        // Creating the probed name changes the directory time and drops the index.
        std::fs::write(root.join("Missing.G00"), b"").unwrap();
        assert_eq!(find(&root, "missing.g00"), Some(root.join("Missing.G00")));
        settle(&root);
        std::fs::remove_file(root.join("Data.PCK")).unwrap();
        assert_eq!(find(&root, "data.pck"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
