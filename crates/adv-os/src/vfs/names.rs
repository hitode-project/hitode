//! Case-insensitive host directory index with exact-spelling precedence.
//! Entries are reused while the directory modification time is unchanged.
//! Recently changed directories are not cached: another change may share their timestamp.
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
    /// Preserve enumeration order and exact spelling on case-sensitive hosts.
    names: HashMap<String, Vec<OsString>>,
    ascii: bool,
}

fn choose(
    dir: &Path,
    name: &str,
    spellings: Option<&Vec<OsString>>,
    ascii: bool,
) -> Option<PathBuf> {
    // Preserve native short names on Windows and Unicode filesystem folding.
    if (cfg!(windows) || !ascii || !name.is_ascii()) && dir.join(name).exists() {
        return Some(dir.join(name));
    }
    let spellings = spellings?;
    // Exact spelling wins if two live host entries differ only by case.
    if let Some(exact) = spellings.iter().skip(1).find(|s| s.as_os_str() == name)
        && dir.join(exact).exists()
    {
        return Some(dir.join(exact));
    }
    Some(dir.join(&spellings[0]))
}

thread_local! {
    static INDEX: RefCell<HashMap<PathBuf, Names>> = RefCell::default();
}

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Host path of the entry in `dir` whose name matches `name` ASCII case-insensitively.
pub(super) fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let modified = match std::fs::metadata(dir) {
        Ok(metadata) => metadata.modified().ok(),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return None;
        }
        Err(_) => None,
    };
    let key = key(name);
    if let Some(modified) = modified {
        let cached = INDEX.with_borrow(|index| {
            let names = index.get(dir).filter(|names| names.modified == modified)?;
            Some(choose(dir, name, names.names.get(&key), names.ascii))
        });
        if let Some(found) = cached {
            return found;
        }
    }
    let settled = modified.filter(|&modified| {
        SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age >= SETTLED)
    });
    let mut names: HashMap<String, Vec<OsString>> = HashMap::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        names
            .entry(self::key(&name.to_string_lossy()))
            .or_default()
            .push(name);
    }
    let ascii = names.keys().all(|name| name.is_ascii());
    let found = choose(dir, name, names.get(&key), ascii);
    if let Some(modified) = settled {
        INDEX.with_borrow_mut(|index| {
            if index.len() >= MAX_DIRECTORIES && !index.contains_key(dir) {
                index.clear();
            }
            index.insert(
                dir.to_owned(),
                Names {
                    modified,
                    names,
                    ascii,
                },
            );
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
        std::fs::write(root.join("Case"), b"upper").unwrap();
        std::fs::write(root.join("case"), b"lower").unwrap();
        settle(&root);
        // Exact spelling wins even if a folded directory index already exists.
        find(&root, "absent");
        assert_eq!(
            std::fs::read(find(&root, "Case").unwrap()).unwrap(),
            std::fs::read(root.join("Case")).unwrap()
        );
        assert_eq!(
            std::fs::read(find(&root, "case").unwrap()).unwrap(),
            std::fs::read(root.join("case")).unwrap()
        );
        std::os::unix::fs::symlink("absent", root.join("broken")).unwrap();
        settle(&root);
        assert!(!find(&root, "broken").unwrap().exists());
        std::fs::write(root.join("É.TXT"), b"unicode").unwrap();
        settle(&root);
        find(&root, "absent");
        assert_eq!(find(&root, "é.txt").is_some(), root.join("é.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
