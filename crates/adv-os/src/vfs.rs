//! Guest file system.
//!
//! * Game files are writable in portable mode; isolated mode uses a data overlay.
//!   Read-only replacements always use copy-on-write in the data overlay.
//! * An optional read-only resource directory sits between guest writes and game files.
//! * Every other `C:\` path (user profile, Windows dir, ...) maps into a writable host
//!   directory, created on demand.
//! * Path components match case-insensitively; `/` and `\` are equivalent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::OsConfig;

mod attributes;
mod mutation;
mod optical;
mod portable_saves;
mod section;
mod stream;
mod wildcard;
pub use optical::{OpticalDriveInfo, OpticalFile};
pub use section::{FileIdentity, FileSection, SharedFiles, file_identity};
pub use stream::{FileStream, StandardInformation};
pub use wildcard::wildcard;

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    /// The same process-owned attribute view queried through Vfs::file_attributes.
    pub attributes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryError {
    NameNotFound,
    PathNotFound,
    NotDirectory,
    AccessDenied,
}

/// An open guest file: a host object in the machine's handle table. Every personality
/// that opens or traces files shares this one type.
pub struct FileObj {
    pub file: FileStream,
    pub guest_path: String,
    pub access: u32,
    /// Read/write/delete share bits of this existing file owner.
    pub share: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Loc {
    /// Under the game root: relative components.
    Game(Vec<String>),
    /// Elsewhere on C:.
    Other(Vec<String>),
}

pub struct Vfs {
    pub sections: SharedFiles,
    attributes: attributes::Attributes,
    /// Current directory (normalized guest path).
    pub cwd: String,
    game_root: String,
    game_host: PathBuf,
    write_game: bool,
    portable_saves: bool,
    user_name: String,
    resources: Option<PathBuf>,
    overlay: PathBuf,
    other: PathBuf,
    optical: BTreeMap<char, std::sync::Arc<optical::OpticalMedia>>,
}

/// Existing host path for `comps` under `base`, matching each component case-insensitively.
fn ci_find(base: &Path, comps: &[String]) -> Option<PathBuf> {
    let mut cur = base.to_path_buf();
    for c in comps {
        let exact = cur.join(c);
        cur = if exact.exists() {
            exact
        } else {
            std::fs::read_dir(&cur)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(c))?
                .path()
        };
    }
    // With no components the base itself may not exist yet (an overlay nothing was written to).
    cur.exists().then_some(cur)
}

/// Host path for `comps` under `base`, reusing existing components' spelling.
fn ci_path(base: &Path, comps: &[String]) -> PathBuf {
    let mut cur = base.to_path_buf();
    for c in comps {
        cur = ci_find(&cur, std::slice::from_ref(c)).unwrap_or_else(|| cur.join(c));
    }
    cur
}

impl Vfs {
    pub fn new(cfg: &OsConfig) -> Self {
        let root = if cfg.guest_root.starts_with(r"\\") {
            normalize_unc(&cfg.guest_root.replace('/', "\\"))
        } else {
            normalize_abs(&cfg.guest_root)
        };
        let root = if root.len() > 3 {
            root.trim_end_matches('\\').to_owned()
        } else {
            root
        };
        let portable_saves = cfg.write_game
            && match portable_saves::migrate(cfg) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("portable save migration failed; retaining legacy save paths: {e}");
                    false
                }
            };
        Self {
            sections: SharedFiles::default(),
            attributes: attributes::Attributes::default(),
            cwd: root.clone(),
            game_root: root,
            game_host: cfg.game_dir.clone(),
            write_game: cfg.write_game,
            portable_saves,
            user_name: cfg.user_name.clone(),
            resources: cfg.resource_dir.clone(),
            overlay: cfg.data_dir.join("overlay"),
            other: cfg.data_dir.join("c"),
            optical: BTreeMap::new(),
        }
    }

    /// Expand against the process current directory without removing components.
    /// GetFullPathNameW uses this length for its native short-buffer preflight.
    pub fn expand(&self, path: &str) -> String {
        let p = path.replace('/', "\\");
        let bytes = p.as_bytes();
        if p.starts_with("\\\\") || bytes.get(1) == Some(&b':') && bytes.get(2) == Some(&b'\\') {
            p
        } else if bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':')
        {
            let directory = if self
                .cwd
                .get(..2)
                .is_some_and(|d| d.eq_ignore_ascii_case(&p[..2]))
            {
                self.cwd.trim_end_matches('\\').to_owned()
            } else {
                p[..2].to_owned()
            };
            format!("{directory}\\{}", &p[2..])
        } else if p.starts_with('\\') {
            let root = if self.cwd.starts_with("\\\\") {
                format!(
                    "\\\\{}",
                    self.cwd[2..]
                        .split('\\')
                        .take(2)
                        .collect::<Vec<_>>()
                        .join("\\")
                )
            } else {
                self.cwd[..2].to_owned()
            };
            format!("{root}{p}")
        } else {
            format!("{}\\{p}", self.cwd.trim_end_matches('\\'))
        }
    }

    /// Absolute, normalized drive or UNC path. UNC I/O is confined to a configured
    /// virtual game root; it never contacts a host network share.
    pub fn normalize(&self, path: &str) -> String {
        let p = self.expand(path);
        if let Some(inner) = p.strip_prefix("\\\\?\\") {
            if inner.as_bytes().get(1) == Some(&b':') {
                format!("\\\\?\\{}", normalize_abs(inner))
            } else if let Some(unc) = inner.strip_prefix("UNC\\") {
                format!("\\\\?\\UNC\\{}", &normalize_unc(&format!("\\\\{unc}"))[2..])
            } else {
                p
            }
        } else if p.starts_with("\\\\.\\") {
            p
        } else if p.starts_with("\\\\") {
            normalize_unc(&p)
        } else {
            normalize_abs(&p)
        }
    }

    fn locate(&self, guest: &str) -> Option<Loc> {
        let n = self.normalize(guest);
        let comps = |s: &str| {
            s.split('\\')
                .filter(|c| !c.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if n.get(..self.game_root.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&self.game_root))
            && (n.len() == self.game_root.len() || n.as_bytes()[self.game_root.len()] == b'\\')
        {
            return Some(Loc::Game(comps(&n[self.game_root.len()..])));
        }
        n.get(..2)
            .filter(|d| d.eq_ignore_ascii_case("C:"))
            .map(|_| {
                let rel = comps(&n[2..]);
                if self.portable_saves
                    && let Some(save) = portable_saves::relative(&rel, &self.user_name)
                {
                    Loc::Game(save.to_vec())
                } else {
                    Loc::Other(rel)
                }
            })
    }

    /// Existing host file or directory for reading.
    pub fn lookup(&self, guest: &str) -> Option<PathBuf> {
        if self.is_optical_path(guest) {
            return None;
        }
        match self.locate(guest)? {
            Loc::Game(rel) => ci_find(&self.overlay, &rel)
                .or_else(|| self.resources.as_ref().and_then(|p| ci_find(p, &rel)))
                .or_else(|| ci_find(&self.game_host, &rel)),
            Loc::Other(rel) => ci_find(&self.other, &rel),
        }
    }

    /// Whether `guest` exists only in a read-only layer (a write must copy it first).
    pub fn is_base_only(&self, guest: &str) -> bool {
        matches!(self.locate(guest), Some(Loc::Game(rel)) if ci_find(&self.overlay, &rel).is_none()
            && (self.resources.as_ref().and_then(|p| ci_find(p, &rel)).is_some() || (!self.write_game && ci_find(&self.game_host, &rel).is_some())))
    }

    /// Deleting/renaming a writable shadow must not expose a read-only layer.
    /// Persistent whiteouts are not yet supported by this VFS.
    pub fn has_read_only_underlay(&self, guest: &str) -> bool {
        matches!(self.locate(guest), Some(Loc::Game(rel)) if
            self.resources.as_ref().and_then(|p| ci_find(p, &rel)).is_some()
            || (!self.write_game && ci_find(&self.game_host, &rel).is_some()))
    }

    /// Host path to create or write `guest` at; parent directories are created.
    pub fn write_path(&self, guest: &str) -> Option<PathBuf> {
        if self.is_optical_path(guest) {
            return None;
        }
        let p = match self.locate(guest)? {
            Loc::Game(rel) => {
                // Read-only replacements retain copy-on-write semantics. Ordinary saves
                // and settings go straight into the game directory in portable mode.
                let replaced = self
                    .resources
                    .as_ref()
                    .and_then(|p| ci_find(p, &rel))
                    .is_some();
                let base = if self.write_game && !replaced && ci_find(&self.overlay, &rel).is_none()
                {
                    &self.game_host
                } else {
                    &self.overlay
                };
                ci_path(base, &rel)
            }
            Loc::Other(rel) => ci_path(&self.other, &rel),
        };
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Some(p)
    }

    /// File mappings always persist game changes in the isolated overlay,
    /// including when ordinary portable saves use the game directory.
    pub fn mapping_write_path(&self, guest: &str) -> Option<PathBuf> {
        if self.is_optical_path(guest) {
            return None;
        }
        let path = match self.locate(guest)? {
            Loc::Game(rel) => ci_path(&self.overlay, &rel),
            Loc::Other(rel) => ci_path(&self.other, &rel),
        };
        std::fs::create_dir_all(path.parent()?).ok()?;
        Some(path)
    }

    pub fn is_dir(&self, guest: &str) -> bool {
        if self.is_optical_path(guest) {
            return self.optical_entry(guest).is_ok_and(|entry| entry.is_dir);
        }
        self.lookup(guest).is_some_and(|p| p.is_dir())
    }

    /// Change only this process's DOS directory after validating its VFS target.
    pub fn set_current_directory(&mut self, guest: &str) -> Result<(), DirectoryError> {
        let path = self.normalize(guest);
        let missing = || {
            let parent = path.rfind('\\').map(|at| &path[..at + 1]);
            if parent.is_some_and(|parent| self.is_dir(parent)) {
                DirectoryError::NameNotFound
            } else {
                DirectoryError::PathNotFound
            }
        };
        let is_dir = if self.is_optical_path(&path) {
            self.optical_entry(&path).map(|entry| entry.is_dir)
        } else {
            let host = self.lookup(&path).ok_or_else(missing)?;
            std::fs::metadata(host).map(|metadata| metadata.is_dir())
        }
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                missing()
            } else {
                DirectoryError::AccessDenied
            }
        })?;
        if !is_dir {
            return Err(DirectoryError::NotDirectory);
        }
        self.cwd = if path.len() > 3 {
            path.trim_end_matches('\\').to_owned()
        } else {
            path
        };
        Ok(())
    }

    /// Merge game, replacement and writable layers, in increasing priority.
    pub fn list(&self, guest_dir: &str) -> Option<Vec<DirEntry>> {
        if self.is_optical_path(guest_dir) {
            return self.optical_list(guest_dir).ok();
        }
        if !self.is_dir(guest_dir) {
            return None;
        }
        let dirs: Vec<PathBuf> = match self.locate(guest_dir)? {
            Loc::Game(rel) => [
                ci_find(&self.game_host, &rel),
                self.resources.as_ref().and_then(|p| ci_find(p, &rel)),
                ci_find(&self.overlay, &rel),
            ]
            .into_iter()
            .flatten()
            .filter(|p| p.is_dir())
            .collect(),
            Loc::Other(rel) => ci_find(&self.other, &rel).into_iter().collect(),
        };
        if dirs.is_empty() {
            return None;
        }
        let mut map = BTreeMap::new();
        for d in dirs {
            for e in std::fs::read_dir(d).ok()?.flatten() {
                let Ok(md) = e.metadata() else { continue };
                let name = e.file_name().to_string_lossy().into_owned();
                map.insert(
                    name.to_ascii_lowercase(),
                    DirEntry {
                        name,
                        is_dir: md.is_dir(),
                        size: md.len(),
                        created: md.created().ok(),
                        accessed: md.accessed().ok(),
                        modified: md.modified().ok(),
                        attributes: self.attributes.read(&e.path(), &md).ok()?,
                    },
                );
            }
        }
        Some(map.into_values().collect())
    }
}

fn normalize_abs(p: &str) -> String {
    let (drive, rest) = p.split_at(2.min(p.len()));
    let mut out: Vec<&str> = Vec::new();
    for c in rest.split(['\\', '/']) {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    let mut s = drive.to_owned();
    s.push('\\');
    s.push_str(&out.join("\\"));
    if s.len() > 3 {
        s = s.trim_end_matches([' ', '.']).to_owned();
    }
    if p.ends_with(['\\', '/']) && s.len() > 3 {
        s.push('\\');
    }
    s
}

fn normalize_unc(p: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    for component in p[2..].split('\\') {
        match component {
            "" | "." => {}
            ".." => {
                if components.len() > 2 {
                    components.pop();
                }
            }
            _ => components.push(component),
        }
    }
    let mut result = format!("\\\\{}", components.join("\\"));
    if p.ends_with('\\') && !result.ends_with('\\') {
        result.push('\\');
    }
    result
}

impl crate::FileSource for Vfs {
    fn absolute_path(&self, path: &str) -> String {
        self.normalize(path)
    }
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if self.is_optical_path(path) {
            use std::io::Read;
            let mut file = self.open_optical_file(path).ok()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).ok()?;
            return Some(bytes);
        }
        let p = self.lookup(path)?;
        p.is_file().then(|| std::fs::read(p).ok()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vfs(dir: &Path) -> Vfs {
        let cfg = OsConfig::new(dir.join("game"), "x.exe", dir.join("data"));
        Vfs::new(&cfg)
    }

    #[test]
    fn normalize_paths() {
        let v = vfs(Path::new("/nonexistent"));
        assert_eq!(v.normalize("save\\..\\data.pac"), "C:\\Game\\data.pac");
        assert_eq!(
            v.normalize("c:/Game/./dll/PAL.dll"),
            "c:\\Game\\dll\\PAL.dll"
        );
        assert_eq!(v.normalize("\\Windows\\win.ini"), "C:\\Windows\\win.ini");
        assert_eq!(v.normalize("C:\\"), "C:\\");
        assert_eq!(
            v.locate("C:\\時計\\save.dat"),
            Some(Loc::Other(vec!["時計".into(), "save.dat".into()]))
        );
    }

    #[test]
    fn unc_current_directory_keeps_the_server_and_share_root() {
        let mut cfg = OsConfig::new("/nonexistent/game", "x.exe", "/nonexistent/data");
        cfg.guest_root = r"\\server\share\Game\".into();
        let v = Vfs::new(&cfg);
        assert_eq!(v.cwd, r"\\server\share\Game");
        assert_eq!(v.expand(r"\leaf.dat"), r"\\server\share\leaf.dat");
        assert_eq!(v.normalize(r"..\..\leaf.dat"), r"\\server\share\leaf.dat");
        assert_eq!(v.normalize(r"C:leaf.dat"), r"C:\leaf.dat");
        assert_eq!(
            v.locate(r"\\server\share\Game\leaf.dat"),
            Some(Loc::Game(vec!["leaf.dat".into()]))
        );
        assert!(v.locate(r"\\other\share\leaf.dat").is_none());
        assert!(
            vfs(Path::new("/nonexistent"))
                .locate(r"\\server\share\leaf.dat")
                .is_none()
        );
    }

    #[test]
    fn overlay_shadows_game_and_never_writes_it() {
        let tmp = std::env::temp_dir().join(format!("pal-vfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("game/Save")).unwrap();
        std::fs::write(tmp.join("game/Config.ini"), "base").unwrap();
        let v = vfs(&tmp);
        // The root is a directory before anything was written (the overlay doesn't exist yet).
        assert!(v.is_dir(r"C:\Game\"));
        assert_eq!(
            std::fs::read_to_string(v.lookup("config.INI").unwrap()).unwrap(),
            "base"
        );
        assert!(v.is_base_only("C:\\Game\\config.ini"));
        let w = v.write_path("C:\\GAME\\config.ini").unwrap();
        assert!(w.starts_with(tmp.join("data/overlay")));
        std::fs::write(&w, "new").unwrap();
        assert_eq!(
            std::fs::read_to_string(v.lookup("config.ini").unwrap()).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.join("game/Config.ini")).unwrap(),
            "base"
        );
        // Directory listings merge both layers without duplicates.
        let names: Vec<String> = v
            .list("C:\\Game")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names
                .iter()
                .filter(|n| n.eq_ignore_ascii_case("config.ini"))
                .count(),
            1
        );
        assert!(names.iter().any(|n| n == "Save"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn portable_saves_survive_reopen_and_replacements_stay_read_only() {
        let tmp = std::env::temp_dir().join(format!("pal-portable-vfs-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("game/Save")).unwrap();
        std::fs::create_dir_all(tmp.join("resources")).unwrap();
        std::fs::write(tmp.join("game/Config.ini"), b"old").unwrap();
        std::fs::write(tmp.join("resources/Story.dat"), b"replacement").unwrap();
        let mut cfg = OsConfig::new(tmp.join("game"), "x.exe", tmp.join("data"));
        cfg.write_game = true;
        cfg.resource_dir = Some(tmp.join("resources"));
        let v = Vfs::new(&cfg);
        assert!(!v.is_base_only("config.ini"));
        assert!(v.is_base_only("story.dat"));
        std::fs::write(v.write_path("save/Slot01.dat").unwrap(), b"saved").unwrap();
        std::fs::write(v.write_path("config.INI").unwrap(), b"changed").unwrap();
        std::fs::write(v.write_path("story.dat").unwrap(), b"cow").unwrap();
        let reopened = Vfs::new(&cfg);
        assert_eq!(
            std::fs::read(tmp.join("game/Save/Slot01.dat")).unwrap(),
            b"saved"
        );
        assert_eq!(
            std::fs::read(tmp.join("game/Config.ini")).unwrap(),
            b"changed"
        );
        assert_eq!(
            std::fs::read(reopened.lookup("SAVE/slot01.DAT").unwrap()).unwrap(),
            b"saved"
        );
        assert_eq!(
            std::fs::read(reopened.lookup("story.dat").unwrap()).unwrap(),
            b"cow"
        );
        assert_eq!(
            std::fs::read(tmp.join("resources/Story.dat")).unwrap(),
            b"replacement"
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn portable_profile_saves_migrate_once_and_write_at_game_root() {
        let assert_path = |actual: PathBuf, expected: PathBuf| {
            assert!(
                actual
                    .to_string_lossy()
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&expected.to_string_lossy().replace('\\', "/")),
                "{actual:?} != {expected:?}"
            );
        };
        let tmp = std::env::temp_dir().join(format!("pal-profile-saves-{}", std::process::id()));
        let game = tmp.join("game");
        let data = game.join(".pal-runtime");
        let old = data.join("c/Users/Player/Documents/Studio/Title/save");
        std::fs::create_dir_all(old.join("screenshots")).unwrap();
        std::fs::create_dir_all(game.join("Save")).unwrap();
        std::fs::write(old.join("Slot01.dat"), b"legacy slot").unwrap();
        std::fs::write(old.join("system.dat"), b"legacy system").unwrap();
        std::fs::write(old.join("screenshots/01.bmp"), b"thumbnail").unwrap();
        std::fs::write(game.join("Save/System.dat"), b"current system").unwrap();
        let mut cfg = OsConfig::new(&game, "game.exe", &data);
        cfg.write_game = true;
        let v = Vfs::new(&cfg);
        let guest = "c:/users/player/documents/Studio/Title/SAVE";
        assert_path(
            v.lookup(&format!("{guest}/slot01.DAT")).unwrap(),
            game.join("Save/Slot01.dat"),
        );
        assert_eq!(
            std::fs::read(game.join("Save/Slot01.dat")).unwrap(),
            b"legacy slot"
        );
        assert_eq!(
            std::fs::read(game.join("Save/System.dat")).unwrap(),
            b"current system"
        );
        assert_eq!(
            std::fs::read(game.join("Save/screenshots/01.bmp")).unwrap(),
            b"thumbnail"
        );
        assert_eq!(v.list(guest).unwrap().len(), 3);
        let new = v.write_path(&format!("{guest}/Slot02.dat")).unwrap();
        assert_path(new.clone(), game.join("Save/Slot02.dat"));
        std::fs::write(new, b"new slot").unwrap();
        assert!(!old.join("Slot02.dat").exists());
        assert_path(
            v.lookup("save/slot02.dat").unwrap(),
            game.join("Save/Slot02.dat"),
        );
        // Deletion remains effective, while the original backup remains untouched.
        std::fs::remove_file(v.lookup(&format!("{guest}/Slot01.dat")).unwrap()).unwrap();
        let reopened = Vfs::new(&cfg);
        assert!(reopened.lookup(&format!("{guest}/Slot01.dat")).is_none());
        assert_eq!(
            std::fs::read(old.join("Slot01.dat")).unwrap(),
            b"legacy slot"
        );
        // Explicit isolated data keeps its previous layout and original files.
        cfg.write_game = false;
        let isolated = Vfs::new(&cfg);
        assert_path(
            isolated.lookup(&format!("{guest}/Slot01.dat")).unwrap(),
            old.join("Slot01.dat"),
        );
        assert_path(
            isolated.write_path(&format!("{guest}/Slot03.dat")).unwrap(),
            old.join("Slot03.dat"),
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn migration_failure_retains_legacy_mapping() {
        let tmp =
            std::env::temp_dir().join(format!("pal-save-migration-failure-{}", std::process::id()));
        let game = tmp.join("game");
        let data = game.join(".pal-runtime");
        let old = data.join("c/Users/Player/Documents/Title/save");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("slot.dat"), b"legacy").unwrap();
        // A file in place of the destination directory must not lose access to saves.
        std::fs::write(game.join("save"), b"unrelated file").unwrap();
        let mut cfg = OsConfig::new(&game, "game.exe", &data);
        cfg.write_game = true;
        let v = Vfs::new(&cfg);
        let guest = "C:/Users/Player/Documents/Title/save/slot.dat";
        assert_eq!(std::fs::read(v.lookup(guest).unwrap()).unwrap(), b"legacy");
        assert_eq!(
            v.write_path(guest).unwrap().canonicalize().unwrap(),
            old.join("slot.dat").canonicalize().unwrap()
        );
        assert!(!data.join("portable-saves-v1").exists());
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn portable_save_aliases_are_limited_to_profile_save_subtrees() {
        let mut cfg = OsConfig::new("/nonexistent/game", "game.exe", "/nonexistent/data");
        cfg.write_game = true;
        let v = Vfs::new(&cfg);
        for path in [
            "C:/Users/Player/Documents/Studio/Title/save/01.dat",
            "C:/Users/Player/AppData/Roaming/Title/Save/01.dat",
            "C:/Users/Player/AppData/Local/Title/save/01.dat",
            "C:/Users/Public/Documents/Title/save/01.dat",
            "C:/ProgramData/Title/save/01.dat",
        ] {
            assert!(
                matches!(v.locate(path), Some(Loc::Game(parts)) if parts[0].eq_ignore_ascii_case("save"))
            );
        }
        for path in [
            "C:/Windows/save/01.dat",
            "C:/Other/save/01.dat",
            "C:/Users/Player/Documents/config.dat",
        ] {
            assert!(matches!(v.locate(path), Some(Loc::Other(_))));
        }
    }

    #[test]
    fn replacements_merge_with_game_below_guest_writes() {
        let tmp = std::env::temp_dir().join(format!("pal-vfs-resources-{}", std::process::id()));
        for dir in ["game", "resources/Sub", "data/overlay"] {
            std::fs::create_dir_all(tmp.join(dir)).unwrap();
        }
        std::fs::write(tmp.join("game/Story.dat"), b"original").unwrap();
        std::fs::write(tmp.join("game/Only.dat"), b"base only").unwrap();
        std::fs::write(tmp.join("resources/STORY.DAT"), b"replacement").unwrap();
        std::fs::write(tmp.join("resources/Sub/New.dat"), b"new").unwrap();
        let mut cfg = OsConfig::new(tmp.join("game"), "x.exe", tmp.join("data"));
        cfg.resource_dir = Some(tmp.join("resources"));
        let v = Vfs::new(&cfg);
        assert_eq!(
            std::fs::read(v.lookup("story.dat").unwrap()).unwrap(),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(v.lookup("only.dat").unwrap()).unwrap(),
            b"base only"
        );
        assert!(v.is_base_only("story.dat"));
        assert!(v.lookup("sub/new.DAT").is_some());
        assert!(v.lookup("C:\\Windows\\Story.dat").is_none());
        let names: Vec<_> = v
            .list(".")
            .unwrap()
            .into_iter()
            .map(|e| e.name.to_ascii_lowercase())
            .collect();
        assert_eq!(names, ["only.dat", "story.dat", "sub"]);
        std::fs::write(v.write_path("story.dat").unwrap(), b"saved change").unwrap();
        assert_eq!(
            std::fs::read(v.lookup("story.dat").unwrap()).unwrap(),
            b"saved change"
        );
        assert!(!v.is_base_only("story.dat"));
        assert_eq!(
            v.list(".")
                .unwrap()
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case("story.dat"))
                .unwrap()
                .size,
            12
        );
        assert!(v.list("story.dat").is_none());
        assert_eq!(
            std::fs::read(tmp.join("resources/STORY.DAT")).unwrap(),
            b"replacement"
        );
        // Removing the optional layer restores base reads; guest writes still win.
        cfg.resource_dir = None;
        let plain = Vfs::new(&cfg);
        assert!(plain.lookup("sub/new.dat").is_none());
        assert_eq!(
            std::fs::read(plain.lookup("story.dat").unwrap()).unwrap(),
            b"saved change"
        );
        drop((v, plain));
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
