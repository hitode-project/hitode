//! Known directories and their listings answer for absent names without a
//! host lookup until the server creates, removes or renames a name
//! (WINE-SYS-15). File contents and metadata do not change listings.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn revision() -> u64 {
    adv_wineserver::names_revision()
}

/// Lookups in one directory before it is listed.
const LIST_AFTER: u32 = 32;
const DIRECTORIES: usize = 64;
const NAMES: usize = 65536;

enum Directory {
    /// Known to be a directory; the count of absent-name lookups so far.
    Counting(u32),
    /// Every name an ASCII name can match, ASCII-lowercased.
    Listed(HashSet<Box<[u8]>>),
    /// Too large or unreadable; looked up on the host.
    Unlisted,
}

#[derive(Default)]
struct Listings {
    revision: u64,
    directories: HashMap<PathBuf, Directory>,
}

thread_local! {
    static LISTINGS: RefCell<Listings> = RefCell::new(Listings::default());
}

fn with<T>(f: impl FnOnce(&mut HashMap<PathBuf, Directory>) -> T) -> T {
    LISTINGS.with_borrow_mut(|listings| {
        let revision = revision();
        if listings.revision != revision {
            listings.directories.clear();
            listings.revision = revision;
        }
        f(&mut listings.directories)
    })
}

/// Whether `path` is a directory, from memory once seen in this revision.
pub(super) fn is_dir(path: &Path) -> bool {
    if with(|directories| directories.contains_key(path)) {
        return true;
    }
    let since = revision();
    if !path.is_dir() {
        return false;
    }
    with(|directories| {
        if revision() == since {
            if directories.len() >= DIRECTORIES {
                directories.clear();
            }
            directories.insert(path.to_owned(), Directory::Counting(0));
        }
    });
    true
}

/// Whether `name` certainly names nothing in `directory`, an existing directory.
pub(super) fn absent(directory: &Path, name: &str) -> bool {
    // Short names and non-ASCII names need the host's own matching.
    if !name.is_ascii() || name.contains('~') {
        return false;
    }
    let since = revision();
    let known = with(|directories| {
        if directories.len() >= DIRECTORIES && !directories.contains_key(directory) {
            directories.clear();
        }
        let entry = directories
            .entry(directory.to_owned())
            .or_insert(Directory::Counting(0));
        match entry {
            Directory::Listed(names) => Some(!names.contains(name.to_ascii_lowercase().as_bytes())),
            Directory::Unlisted => Some(false),
            Directory::Counting(count) if *count + 1 < LIST_AFTER => {
                *count += 1;
                Some(false)
            }
            Directory::Counting(_) => None,
        }
    });
    if let Some(absent) = known {
        return absent;
    }
    let listing = list(directory);
    with(|directories| {
        // A mutation during the listing leaves the directory unlisted.
        if revision() != since {
            return false;
        }
        let absent = listing
            .as_ref()
            .is_some_and(|names| !names.contains(name.to_ascii_lowercase().as_bytes()));
        directories.insert(
            directory.to_owned(),
            listing.map_or(Directory::Unlisted, Directory::Listed),
        );
        absent
    })
}

fn list(directory: &Path) -> Option<HashSet<Box<[u8]>>> {
    let mut names = HashSet::new();
    for entry in std::fs::read_dir(directory).ok()? {
        let name = entry.ok()?.file_name();
        let text = name.to_str()?;
        if let Some(folded) = ascii_fold(text) {
            names.insert(folded.into_bytes().into_boxed_slice());
        }
        if names.len() > NAMES {
            return None;
        }
    }
    Some(names)
}

/// The ASCII name `text` can match, if any. Host file systems may fold or
/// decompose these characters to ASCII ones; other non-ASCII characters never
/// match ASCII.
fn ascii_fold(text: &str) -> Option<String> {
    text.chars()
        .map(|c| match c {
            '\u{212a}' => Some('k'),
            '\u{17f}' => Some('s'),
            '\u{1fef}' => Some('`'),
            '\u{37e}' => Some(';'),
            c if c.is_ascii() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

/// Drop the listing of a file's containing directory for an NT mutation.
pub(crate) fn invalidate(file: &Path) {
    if let Some(parent) = file.parent() {
        LISTINGS.with_borrow_mut(|listings| listings.directories.remove(parent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-SYS-15
    #[test]
    fn listings_answer_absent_names_until_a_mutation() {
        let root = std::env::temp_dir().join(format!("adv-nt-listing-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Present.g00"), b"").unwrap();
        std::fs::write(root.join("\u{212a}ey"), b"").unwrap();
        std::fs::write(root.join("\u{3042}.png"), b"").unwrap();
        assert!(is_dir(&root));
        for _ in 1..LIST_AFTER {
            assert!(!absent(&root, "missing.png"));
        }
        assert!(absent(&root, "missing.png"));
        assert!(!absent(&root, "PRESENT.G00"));
        // A Kelvin sign may fold to an ASCII k on the host.
        assert!(!absent(&root, "KEY"));
        assert!(!absent(&root, "\u{3042}.png"));
        assert!(!absent(&root, "MISSIN~1.PNG"));
        invalidate(&root.join("missing.png"));
        assert!(!absent(&root, "missing.png"));
        // The server's creation of a name ends every listing.
        assert!(is_dir(&root));
        for _ in 0..LIST_AFTER {
            absent(&root, "missing.png");
        }
        assert!(absent(&root, "missing.png"));
        unsafe extern "C" {
            fn hitode_rmdir(path: *const std::ffi::c_char) -> std::ffi::c_int;
        }
        unsafe { hitode_rmdir(c"/nonexistent-adv-listing".as_ptr()) };
        assert!(!absent(&root, "missing.png"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
