//! The only way guest-visible host entries are created, removed or renamed.
//! Each change invalidates the directory index (`names`), which is otherwise
//! never revalidated; `tools/check.py` rejects direct calls elsewhere.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use super::names;

pub fn create_dir(path: &Path) -> io::Result<()> {
    let result = std::fs::create_dir(path);
    names::invalidate(path);
    result
}

pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let result = std::fs::create_dir_all(path);
    for ancestor in path.ancestors() {
        names::invalidate(ancestor);
    }
    result
}

pub fn remove_file(path: &Path) -> io::Result<()> {
    let result = std::fs::remove_file(path);
    names::invalidate(path);
    result
}

pub fn remove_dir(path: &Path) -> io::Result<()> {
    let result = std::fs::remove_dir(path);
    names::invalidate(path);
    result
}

pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let result = std::fs::rename(from, to);
    names::invalidate(from);
    names::invalidate(to);
    result
}

pub fn copy(from: &Path, to: &Path) -> io::Result<u64> {
    let result = std::fs::copy(from, to);
    names::invalidate(to);
    result
}

pub fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let result = std::fs::write(path, contents);
    names::invalidate(path);
    result
}

/// Open with options that may create `path`.
pub fn open(options: &OpenOptions, path: &Path) -> io::Result<File> {
    let result = options.open(path);
    names::invalidate(path);
    result
}

/// A handle-based rename, removal or disposition changed `path`.
pub fn changed(path: &Path) {
    names::invalidate(path);
}

/// Entries of `dir` change when a handle closes (delete-on-close), so the
/// directory is read again on every lookup.
pub fn volatile(dir: &Path) {
    names::volatile(dir);
}
