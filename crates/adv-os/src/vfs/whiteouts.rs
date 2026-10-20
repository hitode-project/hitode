//! Persistent deletions of read-only game files. The index lives beside the
//! overlay, never inside it, so guest listings cannot see it.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

pub(super) struct Whiteouts {
    index: PathBuf,
    paths: Mutex<BTreeSet<String>>,
}

fn key(rel: &[String]) -> String {
    rel.iter()
        .map(|c| c.to_lowercase())
        .collect::<Vec<_>>()
        .join("\\")
}

impl Whiteouts {
    pub(super) fn load(index: PathBuf) -> Self {
        let paths = std::fs::read_to_string(&index)
            .map(|text| {
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            index,
            paths: Mutex::new(paths),
        }
    }

    pub(super) fn contains(&self, rel: &[String]) -> bool {
        !rel.is_empty() && self.paths.lock().unwrap().contains(&key(rel))
    }

    /// Hide or reveal the read-only file at `rel`, persisting the index first.
    pub(super) fn set(&self, rel: &[String], hidden: bool) -> std::io::Result<()> {
        let mut paths = self.paths.lock().unwrap();
        let mut next = paths.clone();
        let changed = if hidden {
            next.insert(key(rel))
        } else {
            next.remove(&key(rel))
        };
        if !changed {
            return Ok(());
        }
        if let Some(parent) = self.index.parent() {
            // host-only: the whiteout index lives in the data directory, outside guest trees.
            std::fs::create_dir_all(parent)?;
        }
        let staged = self.index.with_extension("tmp");
        let text: String = next.iter().map(|path| format!("{path}\n")).collect();
        std::fs::write(&staged, text)?; // host-only: whiteout index
        std::fs::rename(&staged, &self.index)?; // host-only: whiteout index
        *paths = next;
        Ok(())
    }
}
