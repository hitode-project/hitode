//! Shared launcher game library and executable discovery.
use chrono::{Duration, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::UNIX_EPOCH,
};
use tauri::{AppHandle, State};
#[cfg(desktop)]
use tauri_plugin_dialog::DialogExt;

type Store<'a> = State<'a, Mutex<Library>>;
type Reply<T> = Result<T, String>;
fn reply<T>(result: io::Result<T>) -> Reply<T> {
    result.map_err(|e| e.to_string())
}
const MONTH: u64 = 30 * 24 * 60 * 60 * 1000;
pub fn now() -> u64 {
    UNIX_EPOCH.elapsed().unwrap().as_millis() as u64
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryGame {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub vndb: Option<crate::vndb::Novel>,
    pub folder: PathBuf,
    pub exe: String,
    pub codepage: u32,
    pub added_at: u64,
    pub last_played: Option<u64>,
    pub play_seconds: u64,
    pub favorite: bool,
    pub nsfw: bool,
    pub sessions: Vec<PlaySession>,
    #[serde(default)]
    pub daily: BTreeMap<String, u64>,
    #[serde(default)]
    pub last_run: Option<LastRun>,
    #[serde(skip_deserializing)]
    pub offline: Option<Offline>,
    /// Until its setup has run: the setup program on the game's first disc,
    /// as a DOS path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// The host folder `C:\Program Files` shows for this game: where its
    /// setup installed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program_files: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaySession {
    ended: u64,
    seconds: u32,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LastRun {
    pub ended: u64,
    pub summary: Option<adv_report::RunSummary>,
    pub error: Option<String>,
}
type Offline = &'static str;
#[derive(Serialize, Deserialize)]
pub struct Library {
    version: u32,
    #[serde(default = "tracking_default")]
    track_play_time: bool,
    pub(crate) games: Vec<LibraryGame>,
    #[serde(skip)]
    path: PathBuf,
}
pub(crate) fn present_game(mut game: LibraryGame) -> LibraryGame {
    if let Some(vndb) = &mut game.vndb
        && let Some(cover) = &mut vndb.cover
    {
        *cover = crate::covers::url_for(cover);
    }
    game
}

fn normalize_game(game: &mut LibraryGame) {
    if let Some(vndb) = &mut game.vndb
        && let Some(cover) = &mut vndb.cover
    {
        *cover = crate::covers::normalize_stored(cover);
    }
}

fn valid_import_id(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn rewrite_prefix(path: &Path, old_a: &Path, old_b: &Path, new_base: &Path) -> Option<PathBuf> {
    let rest = path
        .strip_prefix(old_a)
        .or_else(|_| path.strip_prefix(old_b))
        .ok()?;
    Some(if rest.as_os_str().is_empty() {
        new_base.to_path_buf()
    } else {
        new_base.join(rest)
    })
}

/// Moves legacy disc-install folders from `<app_data_dir>/installed/` into
/// `<app_data_dir>/imports/` so managed storage, Android Files, and game
/// removal all recognize them (HOST-LIB-7). Returns whether any library entry
/// was updated.
fn migrate_installed_dir(data_dir: &Path, games: &mut [LibraryGame]) -> bool {
    let installed = data_dir.join("installed");
    let Ok(meta) = fs::symlink_metadata(&installed) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return false;
    }
    let imports = data_dir.join("imports");
    if fs::create_dir_all(&imports).is_err() {
        return false;
    }
    let installed_canon = fs::canonicalize(&installed).unwrap_or_else(|_| installed.clone());
    let Ok(entries) = fs::read_dir(&installed) else {
        return false;
    };
    let mut changed = false;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let raw_name = file_name.to_string_lossy();
        let base = if valid_import_id(&raw_name) {
            raw_name.into_owned()
        } else {
            "installed".to_owned()
        };
        let mut dest_name = base.clone();
        let mut suffix = 1u32;
        while imports.join(&dest_name).exists() {
            dest_name = format!("{base}-{suffix}");
            suffix += 1;
        }
        let old_path = entry.path();
        let old_canon = installed_canon.join(&file_name);
        let new_path = imports.join(&dest_name);
        if fs::rename(&old_path, &new_path).is_err() {
            continue;
        }
        let new_canon = fs::canonicalize(&new_path).unwrap_or(new_path);
        for game in games.iter_mut() {
            if let Some(next) = rewrite_prefix(&game.folder, &old_path, &old_canon, &new_canon) {
                game.folder = next;
                changed = true;
            }
            if let Some(pf) = &game.program_files
                && let Some(next) = rewrite_prefix(pf, &old_path, &old_canon, &new_canon)
            {
                game.program_files = Some(next);
                changed = true;
            }
        }
    }
    let _ = fs::remove_dir(&installed);
    changed
}

impl Library {
    pub fn load(dir: PathBuf) -> io::Result<Self> {
        let path = dir.join("library.json");
        let mut lib = Self {
            version: 1,
            track_play_time: true,
            games: vec![],
            path,
        };
        let mut migrated = false;
        match fs::read(&lib.path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(mut saved) => {
                    let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
                    for (game, fields) in
                        saved.games.iter_mut().zip(raw["games"].as_array().unwrap())
                    {
                        if fields.get("daily").is_none() {
                            for session in &game.sessions {
                                add_daily(&mut game.daily, session.ended, session.seconds, &Local);
                            }
                            migrated = true;
                        }
                        if let Some(vndb) = &mut game.vndb
                            && let Some(cover) = &mut vndb.cover
                        {
                            if let Some(file) =
                                crate::covers::migrate_data_uri(&dir, &vndb.id, cover)
                            {
                                *cover = file;
                                migrated = true;
                            } else {
                                let normalized = crate::covers::normalize_stored(cover);
                                if normalized != *cover {
                                    *cover = normalized;
                                    migrated = true;
                                }
                            }
                        }
                    }
                    lib.track_play_time = saved.track_play_time;
                    lib.games = saved.games;
                }
                Err(e) => {
                    let bad = dir.join(format!("library.json.bad-{}", now()));
                    fs::rename(&lib.path, bad)?;
                    eprintln!("invalid game library moved aside: {e}");
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if migrate_installed_dir(&dir, &mut lib.games) {
            migrated = true;
        }
        if migrated {
            lib.save()?;
        }
        Ok(lib)
    }
    fn data_dir(&self) -> &Path {
        self.path.parent().unwrap()
    }
    pub(crate) fn game_ids(&self) -> HashSet<String> {
        self.games.iter().map(|g| g.id.clone()).collect()
    }
    pub(crate) fn referenced_covers(&self) -> HashSet<String> {
        self.games
            .iter()
            .filter_map(|g| g.vndb.as_ref()?.cover.as_deref())
            .map(crate::covers::normalize_stored)
            .filter(|name| crate::covers::valid_name(name))
            .collect()
    }
    pub(crate) fn save(&self) -> io::Result<()> {
        fs::create_dir_all(self.data_dir())?;
        let mut value = serde_json::to_value(self)?;
        for game in value["games"].as_array_mut().unwrap() {
            let object = game.as_object_mut().unwrap();
            object.remove("offline");
            if let Some(cover) = object
                .get_mut("vndb")
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|vndb| vndb.get_mut("cover"))
                && let Some(text) = cover.as_str()
            {
                let normalized = crate::covers::normalize_stored(text);
                if normalized != text {
                    *cover = serde_json::Value::String(normalized);
                }
            }
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&value)?).and_then(|()| fs::rename(tmp, &self.path))
    }
    pub(crate) fn game(&self, id: &str) -> io::Result<usize> {
        let index = self.games.iter().position(|g| g.id == id);
        index.ok_or_else(|| io::Error::other("game not found"))
    }
    pub(crate) fn metadata_target(&self, id: &str) -> io::Result<LibraryGame> {
        let mut game = self.games[self.game(id)?].clone();
        game.offline = offline(&game.folder);
        Ok(game)
    }
    pub(crate) fn set_metadata(
        &mut self,
        id: &str,
        mut novel: Option<crate::vndb::Novel>,
        title: Option<String>,
    ) -> io::Result<LibraryGame> {
        let dir = self.data_dir().to_path_buf();
        if let Some(next) = &mut novel
            && let Some(cover) = &mut next.cover
        {
            if let Some(file) = crate::covers::migrate_data_uri(&dir, &next.id, cover) {
                *cover = file;
            } else {
                *cover = crate::covers::normalize_stored(cover);
            }
        }
        let index = self.game(id)?;
        let game = &mut self.games[index];
        let before = game.clone();
        let old_cover = before.vndb.as_ref().and_then(|old| old.cover.clone());
        if let Some(next) = &novel
            && game.vndb.as_ref().map(|old| &old.id) != Some(&next.id)
            && next.image.as_ref().is_some_and(|image| image.sexual >= 1.0)
        {
            game.nsfw = true;
        }
        game.vndb = novel;
        if let Some(title) = title {
            game.title = title;
        }
        if let Err(error) = self.save() {
            self.games[index] = before;
            return Err(error);
        }
        if let Some(old_cover) = old_cover {
            let referenced = self.referenced_covers();
            crate::covers::remove_if_unused(&dir, &old_cover, &referenced);
        }
        self.metadata_target(id)
    }
    #[cfg(any(test, target_os = "macos", target_os = "android", target_os = "ios"))]
    pub fn launch(&self, id: &str) -> io::Result<(PathBuf, PathBuf, u32)> {
        let game = &self.games[self.game(id)?];
        let data = self.path.parent().unwrap().join("games").join(id);
        Ok((game.folder.join(&game.exe), data, game.codepage))
    }
    /// The setup a pending install runs, and the folder behind its
    /// `C:\Program Files`.
    #[cfg(any(test, target_os = "macos", target_os = "android", target_os = "ios"))]
    pub fn installation(&self, id: &str) -> io::Result<(Option<String>, Option<PathBuf>)> {
        let game = &self.games[self.game(id)?];
        Ok((game.setup.clone(), game.program_files.clone()))
    }
    #[cfg(any(test, target_os = "macos", target_os = "android", target_os = "ios"))]
    pub fn finished(&mut self, id: &str, seconds: u32, run: LastRun) -> io::Result<()> {
        let ended = run.ended;
        let i = self.game(id)?;
        let game = &mut self.games[i];
        if self.track_play_time {
            game.sessions.push(PlaySession { ended, seconds });
            game.play_seconds += u64::from(seconds);
            add_daily(&mut game.daily, ended, seconds, &Local);
        }
        game.last_played = Some(ended);
        game.last_run = Some(run);
        self.prune(ended);
        self.save()
    }
    fn prune(&mut self, time: u64) -> bool {
        let mut changed = false;
        for game in &mut self.games {
            let len = game.sessions.len();
            let cutoff = time.saturating_sub(MONTH);
            game.sessions.retain(|s| s.ended >= cutoff);
            changed |= len != game.sessions.len();
        }
        changed
    }
    fn add(&mut self, folder: &Path, exe: &str, cp: u32, title: &str) -> io::Result<LibraryGame> {
        let dir = fs::canonicalize(folder)?;
        let exe = exe.replace('\\', "/");
        if let Some(game) = self
            .games
            .iter()
            .find(|g| g.folder == dir && g.exe.eq_ignore_ascii_case(&exe))
        {
            return Ok(game.clone());
        }
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        let game = LibraryGame {
            id: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            title: title.into(),
            folder: dir,
            exe,
            codepage: cp,
            added_at: now(),
            ..LibraryGame::default()
        };
        self.games.push(game.clone());
        self.save()?;
        Ok(game)
    }
    /// A game a disc's `setup` installs into `target`; its program is chosen
    /// once the setup has run.
    /// Without a `target` it installs into managed storage (`imports/<id>`).
    fn add_install(
        &mut self,
        target: Option<&Path>,
        setup: &str,
        cp: u32,
        title: &str,
    ) -> io::Result<LibraryGame> {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let target = match target {
            Some(target) => target.to_owned(),
            None => self.path.parent().unwrap().join("imports").join(&id),
        };
        fs::create_dir_all(&target)?;
        let dir = fs::canonicalize(&target)?;
        let game = LibraryGame {
            id,
            title: title.into(),
            folder: dir.clone(),
            codepage: cp,
            added_at: now(),
            setup: Some(setup.into()),
            program_files: Some(dir),
            ..LibraryGame::default()
        };
        self.games.push(game.clone());
        self.save()?;
        Ok(game)
    }
    fn remove(&mut self, id: &str) -> io::Result<LibraryGame> {
        let i = self.game(id)?;
        let game = self.games.remove(i);
        self.save()?;
        Ok(game)
    }
    fn restore(&mut self, mut game: LibraryGame, index: usize) -> io::Result<()> {
        if self.games.iter().any(|g| g.id == game.id) {
            return Err(io::Error::other("game id already exists"));
        }
        normalize_game(&mut game);
        self.games.insert(index.min(self.games.len()), game);
        self.save()
    }
    /// Puts the games in the order of `ids`, which must name each game once.
    fn reorder(&mut self, ids: &[String]) -> io::Result<()> {
        let rank: BTreeMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        if rank.len() != ids.len()
            || ids.len() != self.games.len()
            || self.games.iter().any(|g| !rank.contains_key(g.id.as_str()))
        {
            return Err(io::Error::other("order must name every game once"));
        }
        self.games.sort_by_key(|g| rank[g.id.as_str()]);
        self.save()
    }
    /// Folder, executable and id snapshots, so discovery can run unlocked.
    fn known(&self) -> Known {
        self.games
            .iter()
            .map(|g| (g.folder.clone(), g.exe.clone(), g.id.clone()))
            .collect()
    }
}
type Known = Vec<(PathBuf, String, String)>;
fn inspect(known: &Known, folder: &Path) -> io::Result<FolderInspection> {
    let folder = fs::canonicalize(folder)?;
    let mut exes = Vec::new();
    let mut registry_files = Vec::new();
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_file() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".reg") || lower.ends_with(".bat") || lower.ends_with(".cmd") {
            registry_files.push(name);
            continue;
        }
        if !lower.ends_with(".exe") {
            continue;
        }
        // An unreadable executable is not a candidate; it must not hide the others.
        let (Ok(metadata), Ok(codepage)) = (entry.metadata(), adv_shell::detect_codepage(&path))
        else {
            continue;
        };
        exes.push(ExeCandidate {
            in_library: known
                .iter()
                .find(|(dir, exe, _)| *dir == folder && exe.eq_ignore_ascii_case(&name))
                .map(|(_, _, id)| id.clone()),
            path: name,
            size: metadata.len(),
            arch: machine(&path),
            codepage,
        });
    }
    exes.sort_by_key(|e| std::cmp::Reverse(e.size));
    registry_files.sort_by_key(|name| registry_file_rank(name));
    let suggested = exes.iter().position(|e| {
        e.arch == "x86"
            && !prefixed(&e.path, &["setup", "install", "unins", "config"])
            && !packaged(&folder.join(&e.path))
    });
    let name = folder.file_name().unwrap_or_default();
    let title = name.to_string_lossy().into();
    let existing = known.iter().find(|(known, _, _)| *known == folder);
    let in_library = existing.map(|(_, _, id)| id.clone());
    Ok(FolderInspection {
        folder,
        title,
        exes,
        suggested,
        in_library,
        registry_files,
    })
}
fn scan(known: &Known, root: PathBuf, depth: u8) -> io::Result<Vec<ScanFound>> {
    if !(1..=3).contains(&depth) {
        return Err(io::Error::other("depth must be between 1 and 3"));
    }
    let root = fs::canonicalize(root)?;
    let mut queue = VecDeque::from([(root.clone(), 0)]);
    let mut found = Vec::new();
    while let Some((folder, level)) = queue.pop_front() {
        // Only the root's errors are reported; an unreadable subfolder is skipped.
        let info = match inspect(known, &folder) {
            Ok(info) => info,
            Err(e) if level == 0 => return Err(e),
            Err(_) => continue,
        };
        if !info.exes.is_empty() {
            let exes = &info.exes;
            let installer = exes
                .iter()
                .any(|e| prefixed(&e.path, &["setup", "install"]));
            let setup = info.suggested.is_none() && installer;
            let relative = folder.strip_prefix(&root).unwrap().to_string_lossy();
            let exe = info.suggested.map(|i| info.exes[i].clone());
            let in_library = exe.as_ref().and_then(|e| e.in_library.clone());
            found.push(ScanFound {
                relative: relative.replace('\\', "/"),
                folder: info.folder,
                depth: level,
                title: info.title,
                exe,
                exes: info.exes,
                setup,
                in_library,
            });
            if found.len() == 500 {
                break;
            }
        }
        if level < depth {
            for entry in fs::read_dir(folder)?.flatten() {
                let visible = !entry.file_name().to_string_lossy().starts_with('.');
                if entry.file_type().is_ok_and(|t| t.is_dir()) && visible {
                    queue.push_back((entry.path(), level + 1));
                }
            }
        }
    }
    Ok(found)
}
/// Allocate elapsed whole seconds to local calendar days, including DST boundaries.
fn add_daily<T: TimeZone>(daily: &mut BTreeMap<String, u64>, ended: u64, seconds: u32, tz: &T) {
    let end = tz.timestamp_opt((ended / 1000) as i64, 0).single().unwrap();
    let mut start = end.clone() - Duration::seconds(i64::from(seconds));
    while start < end {
        let day = start.date_naive();
        let mut midnight = day.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap();
        // Some local days begin after midnight when clocks advance there.
        let next = loop {
            if let Some(next) = tz.from_local_datetime(&midnight).earliest() {
                break next;
            }
            midnight += Duration::minutes(1);
        };
        let until = next.min(end.clone());
        *daily.entry(day.to_string()).or_default() += (until.clone() - start).num_seconds() as u64;
        start = until;
    }
}

#[tauri::command]
#[cfg(desktop)]
pub fn reveal_path(path: PathBuf) -> Reply<()> {
    if !path.exists() {
        return Err("path does not exist".into());
    }
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[cfg(any(target_os = "ios", test))]
fn mobile_reveal_path() -> Reply<()> {
    Err("revealing paths is not supported on mobile".into())
}

// spec: AND-APP-9
#[test]
fn mobile_reveal_is_unsupported() {
    assert_eq!(
        mobile_reveal_path(),
        Err("revealing paths is not supported on mobile".into())
    );
}

/// Opens shared storage in Files. Managed copies use reveal_game_files.
#[tauri::command]
#[cfg(target_os = "android")]
pub async fn reveal_path(app: AppHandle, path: PathBuf) -> Reply<()> {
    let folder = if path.is_dir() {
        path.as_path()
    } else {
        path.parent().unwrap_or(&path)
    };
    if !folder.is_dir() {
        return Err("path does not exist".into());
    }
    crate::android::reveal_folder(&app, folder).await
}

#[tauri::command]
#[cfg(target_os = "ios")]
pub fn reveal_path(path: PathBuf) -> Reply<()> {
    let _ = path;
    mobile_reveal_path()
}

/// Android external-storage volume root of `path`, and whether it is removable.
pub(crate) fn android_external_volume(path: &Path) -> Option<(&Path, bool)> {
    for ancestor in path.ancestors() {
        match ancestor.parent() {
            Some(p) if p == Path::new("/storage/emulated") => {
                let id = ancestor.file_name()?.to_str()?;
                return (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                    .then_some((ancestor, false));
            }
            Some(p) if p == Path::new("/storage") => {
                let name = ancestor.file_name()?.to_str()?;
                if name != "emulated" && name != "self" {
                    return Some((ancestor, true));
                }
            }
            _ => {}
        }
    }
    None
}

fn offline(folder: &Path) -> Option<Offline> {
    if folder.exists() {
        return None;
    }
    let disk = folder
        .ancestors()
        .find(|p| p.parent() == Some(Path::new("/Volumes")))
        .or_else(|| android_external_volume(folder).and_then(|(root, rem)| rem.then_some(root)));
    Some(if disk.is_some_and(|p| !p.exists()) {
        "disk"
    } else {
        "missing"
    })
}
fn prefixed(path: &str, prefixes: &[&str]) -> bool {
    let lower = path.to_ascii_lowercase();
    prefixes.iter().any(|p| lower.starts_with(p))
}
/// Installers, patchers and tool bundles: NSIS (`.ndata`), a payload appended
/// after the sections that outweighs them, or a console program.
fn packaged(path: &Path) -> bool {
    let read = || -> Option<bool> {
        let mut file = fs::File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        let mut head = vec![0; 4096];
        let n = file.read(&mut head).ok()?;
        head.truncate(n);
        let u16_at = |at: usize| Some(u16::from_le_bytes(head.get(at..at + 2)?.try_into().ok()?));
        let u32_at = |at: usize| Some(u32::from_le_bytes(head.get(at..at + 4)?.try_into().ok()?));
        let pe = u32_at(60)? as usize;
        let (sections, optional) = (u16_at(pe + 6)? as usize, u16_at(pe + 20)? as usize);
        let console = u16_at(pe + 92)? == 3;
        let mut end = 0u64;
        let mut nsis = false;
        for i in 0..sections {
            let at = pe + 24 + optional + i * 40;
            nsis |= head.get(at..at + 8)? == b".ndata\0\0";
            end = end.max(u64::from(u32_at(at + 16)?) + u64::from(u32_at(at + 20)?));
        }
        // LiveMaker appends its game archive ("vf" ... "lv") to the program.
        let mut overlay = [0; 2];
        let archive = file.seek(SeekFrom::Start(end)).is_ok()
            && file.read_exact(&mut overlay).is_ok()
            && overlay == *b"vf";
        let bundle = sections > 0 && len - end.min(len) > len / 2 && !archive;
        Some(console || nsis || bundle)
    };
    read().unwrap_or(false)
}
fn machine(path: &Path) -> &'static str {
    let read = || {
        let mut file = fs::File::open(path).ok()?;
        let mut dos = [0; 64];
        file.read_exact(&mut dos).ok()?;
        let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
        file.seek(SeekFrom::Start(u64::from(offset))).ok()?;
        let mut pe = [0; 6];
        file.read_exact(&mut pe).ok()?;
        (&dos[..2] == b"MZ" && &pe[..4] == b"PE\0\0").then(|| u16::from_le_bytes([pe[4], pe[5]]))
    };
    match read() {
        Some(0x14c) => "x86",
        Some(0x8664) => "x64",
        _ => "other",
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExeCandidate {
    path: String,
    size: u64,
    arch: &'static str,
    codepage: u32,
    in_library: Option<String>,
}
fn registry_file_rank(name: &str) -> (u8, u8, String) {
    let lower = name.to_ascii_lowercase();
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let stem_lower = stem.to_ascii_lowercase();
    let named_registry =
        stem_lower.contains("reg") || stem.contains("注册表") || stem.contains("レジストリ");
    let is_script = lower.ends_with(".bat") || lower.ends_with(".cmd");
    // Prefer registry-named scripts (e.g. `注册表恢复.bat`), then registry `.reg`,
    // then other `.bat`/`.cmd` scripts, then other `.reg` files.
    (u8::from(!named_registry), u8::from(!is_script), lower)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInspection {
    folder: PathBuf,
    title: String,
    exes: Vec<ExeCandidate>,
    suggested: Option<usize>,
    in_library: Option<String>,
    registry_files: Vec<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanFound {
    folder: PathBuf,
    relative: String,
    depth: u8,
    title: String,
    /// The suggested main program.
    exe: Option<ExeCandidate>,
    /// Every executable in the folder, so the user can pick another.
    exes: Vec<ExeCandidate>,
    setup: bool,
    in_library: Option<String>,
}
#[derive(Deserialize)]
pub struct GamePatch {
    title: Option<String>,
    codepage: Option<u32>,
    favorite: Option<bool>,
    nsfw: Option<bool>,
    folder: Option<PathBuf>,
    exe: Option<String>,
}

#[tauri::command]
#[cfg(desktop)]
pub async fn library_pick_folder(app: AppHandle) -> Reply<Option<PathBuf>> {
    tauri::async_runtime::spawn_blocking(move || {
        let folder = app.dialog().file().blocking_pick_folder();
        folder
            .map(|p| p.into_path().map_err(|e| e.to_string()))
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
#[cfg(mobile)]
pub async fn library_pick_folder(app: AppHandle) -> Reply<Option<PathBuf>> {
    use tauri_plugin_storage::StorageExt;
    let storage = app.storage();
    let folder = storage.pick_folder().await?;
    let Some(uri) = folder.uri else {
        return Ok(None);
    };
    if let Some(path) = folder.path {
        return Ok(Some(path));
    }
    storage
        .copy_folder(uri, None, None, None)
        .await
        .map(|copy| Some(copy.path))
}

#[tauri::command]
pub async fn library_list(_app: AppHandle, state: Store<'_>) -> Reply<Vec<LibraryGame>> {
    #[cfg(target_os = "ios")]
    {
        use tauri_plugin_storage::StorageExt;
        _app.storage().restore_folders().await?;
    }
    let mut library = state.lock().unwrap();
    if library.prune(now()) {
        library.save().map_err(|e| e.to_string())?;
    }
    let mut games: Vec<_> = library.games.iter().cloned().map(present_game).collect();
    for game in &mut games {
        game.offline = offline(&game.folder);
    }
    Ok(games)
}
// Discovery reads whole executables to detect their code page. Synchronous
// commands run on the main thread, which also runs the game, so these run on
// a blocking worker and release the library lock first.
#[tauri::command]
pub async fn library_inspect(state: Store<'_>, folder: PathBuf) -> Reply<FolderInspection> {
    let known = state.lock().unwrap().known();
    let inspected = tauri::async_runtime::spawn_blocking(move || inspect(&known, &folder));
    reply(inspected.await.map_err(|e| e.to_string())?)
}
#[tauri::command]
pub async fn library_scan(state: Store<'_>, root: PathBuf, depth: u8) -> Reply<Vec<ScanFound>> {
    let known = state.lock().unwrap().known();
    let found = tauri::async_runtime::spawn_blocking(move || scan(&known, root, depth));
    reply(found.await.map_err(|e| e.to_string())?)
}
#[tauri::command]
pub fn library_add(
    state: Store<'_>,
    folder: PathBuf,
    exe: String,
    codepage: u32,
    title: String,
) -> Reply<LibraryGame> {
    reply(state.lock().unwrap().add(&folder, &exe, codepage, &title)).map(present_game)
}
#[tauri::command]
pub fn library_update(state: Store<'_>, id: String, patch: GamePatch) -> Reply<LibraryGame> {
    let mut library = state.lock().unwrap();
    let folder = reply(patch.folder.map(fs::canonicalize).transpose())?;
    let i = library.game(&id).map_err(|e| e.to_string())?;
    let old_folder = library.games[i].folder.clone();
    let game = &mut library.games[i];
    game.title = patch.title.unwrap_or_else(|| game.title.clone());
    game.codepage = patch.codepage.unwrap_or(game.codepage);
    game.favorite = patch.favorite.unwrap_or(game.favorite);
    game.nsfw = patch.nsfw.unwrap_or(game.nsfw);
    if let Some(next_folder) = folder {
        if next_folder != old_folder {
            let imports_root = library.data_dir().join("imports");
            let imports_canon =
                fs::canonicalize(&imports_root).unwrap_or_else(|_| imports_root.clone());
            let old_in_imports = old_folder.parent().is_some_and(|p| p == imports_canon);
            let still_used = library.games.iter().enumerate().any(|(idx, other)| {
                idx != i
                    && (other.folder.starts_with(&old_folder)
                        || other
                            .program_files
                            .as_ref()
                            .is_some_and(|pf| pf.starts_with(&old_folder)))
            });
            if old_in_imports
                && !still_used
                && fs::read_dir(&old_folder)
                    .ok()
                    .is_some_and(|mut it| it.next().is_none())
                && fs::remove_dir(&old_folder).is_ok()
                && library.games[i].program_files.as_deref() == Some(old_folder.as_path())
            {
                library.games[i].program_files = None;
            }
        }
        library.games[i].folder = next_folder;
    }
    let game = &mut library.games[i];
    if let Some(exe) = patch.exe {
        game.exe = exe.replace('\\', "/");
        // Choosing the installed program completes the install.
        if !game.exe.is_empty() {
            game.setup = None;
        }
    }
    let game = present_game(game.clone());
    library.save().map_err(|e| e.to_string())?;
    Ok(game)
}
/// A disc image before anything runs from it: its label and size, the
/// programs its player would start, and the code page of the first.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscInspection {
    label: String,
    bytes: u64,
    programs: Vec<String>,
    codepage: u32,
}
#[tauri::command]
pub async fn library_inspect_disc(path: PathBuf) -> Reply<DiscInspection> {
    let inspect = move || -> io::Result<DiscInspection> {
        let disc = adv_shell::Disc::open(&path, 'D')?;
        let programs = disc.programs();
        let codepage = match programs.first() {
            Some(program) => disc.codepage(program)?,
            None => 932,
        };
        Ok(DiscInspection {
            label: disc.label().to_owned(),
            bytes: fs::metadata(&path)?.len(),
            programs,
            codepage,
        })
    };
    let found = tauri::async_runtime::spawn_blocking(move || reply(inspect()));
    found.await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn library_add_install(
    state: Store<'_>,
    target: Option<PathBuf>,
    setup: String,
    codepage: u32,
    title: String,
) -> Reply<LibraryGame> {
    reply(
        state
            .lock()
            .unwrap()
            .add_install(target.as_deref(), &setup, codepage, &title),
    )
    .map(present_game)
}
#[tauri::command]
pub fn library_remove(state: Store<'_>, id: String) -> Reply<LibraryGame> {
    reply(state.lock().unwrap().remove(&id)).map(present_game)
}
#[tauri::command]
pub fn library_reorder(state: Store<'_>, ids: Vec<String>) -> Reply<()> {
    reply(state.lock().unwrap().reorder(&ids))
}
#[tauri::command]
pub fn library_restore(state: Store<'_>, game: LibraryGame, index: usize) -> Reply<()> {
    reply(state.lock().unwrap().restore(game, index))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> (PathBuf, Library) {
        let dir = std::env::temp_dir().join(format!("adv-library-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let lib = Library::load(dir.join("app")).unwrap();
        (dir, lib)
    }
    // spec: HOST-VNDB-1
    #[test]
    fn metadata_round_trip_preserves_display_names_and_game_state() {
        let (dir, mut lib) = fixture("vndb");
        let game = lib.add(&dir, "game.exe", 932, "My title").unwrap();
        let vn = crate::vndb::Novel {
            id: "v17".into(),
            title: "Database title".into(),
            image: Some(crate::vndb::Image {
                thumbnail: String::new(),
                sexual: 2.0,
            }),
            cover: Some("data:image/png;base64,AA==".into()),
            ..Default::default()
        };
        lib.set_metadata(&game.id, Some(vn.clone()), None).unwrap();
        let mut loaded = Library::load(dir.join("app")).unwrap();
        assert_eq!(loaded.games[0].title, "My title");
        assert!(loaded.games[0].nsfw);
        loaded.games[0].nsfw = false;
        let stored_cover = loaded.games[0]
            .vndb
            .as_ref()
            .unwrap()
            .cover
            .clone()
            .unwrap();
        assert!(crate::covers::valid_name(&stored_cover));
        let cover_path = crate::covers::directory(&dir.join("app")).join(&stored_cover);
        assert_eq!(fs::read(&cover_path).unwrap(), &[0]);
        let presented = present_game(loaded.games[0].clone());
        assert_eq!(
            presented.vndb.as_ref().unwrap().cover.as_deref(),
            Some(crate::covers::url_for(&stored_cover).as_str())
        );
        // Legacy inline data URI in library.json migrates out to covers/ on load.
        let mut raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join("app/library.json")).unwrap()).unwrap();
        raw["games"][0]["vndb"]["cover"] =
            serde_json::Value::String("data:image/png;base64,AA==".into());
        fs::write(
            dir.join("app/library.json"),
            serde_json::to_vec(&raw).unwrap(),
        )
        .unwrap();
        let migrated = Library::load(dir.join("app")).unwrap();
        assert_eq!(
            migrated.games[0].vndb.as_ref().unwrap().cover.as_deref(),
            Some(stored_cover.as_str())
        );
        assert!(
            !fs::read_to_string(dir.join("app/library.json"))
                .unwrap()
                .contains("data:image/")
        );
        loaded
            .set_metadata(&game.id, Some(vn), Some("Custom name".into()))
            .unwrap();
        assert_eq!(loaded.games[0].title, "Custom name");
        assert!(!loaded.games[0].nsfw);
        assert!(loaded.games[0].vndb.is_some());
        // Unlinking keeps the display name and game files, and drops the unreferenced cover file.
        loaded.set_metadata(&game.id, None, None).unwrap();
        assert!(loaded.games[0].vndb.is_none());
        assert!(!cover_path.exists());
        assert_eq!(loaded.games[0].title, "Custom name");
        assert_eq!(loaded.games[0].folder, game.folder);
        assert_eq!(loaded.games[0].exe, game.exe);
        fs::remove_dir_all(dir).unwrap();
    }

    // spec: HOST-LIB-3, HOST-LIB-6
    #[test]
    fn disabling_tracking_keeps_last_run_but_not_play_statistics() {
        let (dir, mut lib) = fixture("tracking");
        pe(&dir, "game.exe", 0x14c, 100);
        let game = lib.add(&dir, "game.exe", 932, "Game").unwrap();
        lib.track_play_time = false;
        lib.save().unwrap();
        let mut lib = Library::load(dir.join("app")).unwrap();
        assert!(!lib.track_play_time);
        let run = LastRun {
            ended: now(),
            summary: None,
            error: None,
        };
        lib.finished(&game.id, 600, run.clone()).unwrap();
        let game = &lib.games[0];
        assert_eq!(game.play_seconds, 0);
        assert!(game.sessions.is_empty() && game.daily.is_empty());
        assert_eq!(game.last_run.as_ref(), Some(&run));
        assert_eq!(game.last_played, Some(run.ended));
        lib.track_play_time = true;
        lib.finished(&lib.games[0].id.clone(), 60, run).unwrap();
        assert_eq!(lib.games[0].play_seconds, 60);
        fs::remove_dir_all(dir).unwrap();
    }

    // spec: HOST-LIB-2
    #[test]
    fn suggestion_skips_installers_bundles_and_console_programs() {
        let dir =
            std::env::temp_dir().join(format!("adv-launcher-packaged-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // One section at file offset 512 of `raw` bytes, subsystem `sub`.
        let exe = |name: &str, section: &[u8; 8], raw: u32, size: usize, sub: u16| {
            let mut bytes = vec![0; size];
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            bytes[68..70].copy_from_slice(&0x14cu16.to_le_bytes());
            bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
            bytes[84..86].copy_from_slice(&224u16.to_le_bytes());
            bytes[156..158].copy_from_slice(&sub.to_le_bytes());
            let at = 64 + 24 + 224;
            bytes[at..at + 8].copy_from_slice(section);
            bytes[at + 16..at + 20].copy_from_slice(&raw.to_le_bytes());
            bytes[at + 20..at + 24].copy_from_slice(&512u32.to_le_bytes());
            fs::write(dir.join(name), bytes).unwrap();
        };
        exe("game.exe", b".text\0\0\0", 3000, 3512, 2);
        exe("patch.exe", b".ndata\0\0", 512, 1024 * 8, 2);
        exe("tool.exe", b".text\0\0\0", 512, 1024 * 6, 2);
        exe("console.exe", b".text\0\0\0", 4000, 4512, 3);
        let found = inspect(&Vec::new(), &dir).unwrap();
        let suggested = found.suggested.map(|i| found.exes[i].path.as_str());
        assert_eq!(suggested, Some("game.exe"));
        // A program carrying its engine's archive is still the game.
        fs::remove_file(dir.join("game.exe")).unwrap();
        exe("live.exe", b".text\0\0\0", 512, 1024 * 8, 2);
        let mut live = fs::read(dir.join("live.exe")).unwrap();
        live[1024..1026].copy_from_slice(b"vf");
        fs::write(dir.join("live.exe"), live).unwrap();
        let found = inspect(&Vec::new(), &dir).unwrap();
        let suggested = found.suggested.map(|i| found.exes[i].path.as_str());
        assert_eq!(suggested, Some("live.exe"));
        fs::remove_dir_all(dir).unwrap();
    }
    fn pe(dir: &Path, name: &str, machine: u16, size: usize) {
        let mut bytes = vec![0; size];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
        fs::write(dir.join(name), bytes).unwrap();
    }
    // spec: HOST-LIB-1, HOST-LIB-3
    #[test]
    fn storage_and_play_session_lifecycle() {
        let (dir, mut lib) = fixture("storage");
        pe(&dir, "game.exe", 0x14c, 80);
        let game = lib.add(&dir, "game.exe", 936, "Game").unwrap();
        let duplicate = lib.add(&dir.join("."), "GAME.EXE", 932, "X").unwrap();
        assert_eq!(duplicate.id, game.id);
        let (exe, data, cp) = lib.launch(&game.id).unwrap();
        assert_eq!(exe, fs::canonicalize(&dir).unwrap().join("game.exe"));
        assert_eq!(data, dir.join("app/games").join(&game.id));
        assert_eq!(cp, 936);
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("save"), b"save").unwrap();
        for (ended, seconds) in [(1, 10), (MONTH + 1, 20), (MONTH + 2, 30)] {
            lib.finished(
                &game.id,
                seconds,
                LastRun {
                    ended,
                    summary: None,
                    error: None,
                },
            )
            .unwrap();
        }
        let app_dir = dir.join("app");
        let saved = fs::read_to_string(&lib.path).unwrap();
        assert!(!saved.contains("offline"));
        let loaded = Library::load(app_dir.clone()).unwrap();
        assert_eq!(loaded.games[0].last_played, Some(MONTH + 2));
        assert_eq!(loaded.games[0].play_seconds, 60);
        assert_eq!(loaded.games[0].sessions.len(), 2);
        let removed = lib.remove(&game.id).unwrap();
        assert!(Library::load(app_dir.clone()).unwrap().games.is_empty());
        assert!(exe.exists() && data.join("save").exists());
        lib.restore(removed.clone(), 99).unwrap();
        assert!(lib.restore(removed, 0).is_err());
        assert_eq!(Library::load(app_dir.clone()).unwrap().games[0].id, game.id);
        fs::write(&lib.path, b"broken").unwrap();
        assert!(Library::load(app_dir.clone()).unwrap().games.is_empty());
        let entries = fs::read_dir(app_dir).unwrap();
        for entry in entries.map(|e| e.unwrap()) {
            if entry.path().is_file() {
                let name = entry.file_name();
                assert!(name.to_string_lossy().starts_with("library.json.bad-"));
                assert_eq!(fs::read(entry.path()).unwrap(), b"broken");
            }
        }
        assert_eq!(offline(&dir), None);
        assert_eq!(offline(&dir.join("missing")), Some("missing"));
        let volume = Path::new("/Volumes").join(dir.file_name().unwrap());
        assert_eq!(offline(&volume.join("game")), Some("disk"));
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-10
    // spec: WINE-SES-12
    #[test]
    fn a_pending_install_keeps_its_setup_and_program_files_until_reloaded() {
        let (dir, mut lib) = fixture("install");
        let target = dir.join("Installed Game");
        let game = lib
            .add_install(Some(&target), r"D:\SETUP.EXE", 932, "Disc title") // privacy: allow authored guest path
            .unwrap();
        assert!(target.is_dir());
        assert_eq!(game.exe, "");
        let loaded = Library::load(dir.join("app")).unwrap();
        let (setup, program_files) = loaded.installation(&game.id).unwrap();
        assert_eq!(setup.as_deref(), Some(r"D:\SETUP.EXE")); // privacy: allow authored guest path
        assert_eq!(program_files, Some(fs::canonicalize(&target).unwrap()));
        let beside = lib
            .add_install(None, r"D:\SETUP.EXE", 932, "Other") // privacy: allow authored guest path
            .unwrap();
        assert!(
            beside
                .folder
                .starts_with(fs::canonicalize(dir.join("app/imports")).unwrap())
        );
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-7
    #[test]
    fn load_migrates_legacy_installed_folders_into_managed_imports() {
        let dir = std::env::temp_dir().join(format!(
            "adv-library-{}-migrate-installed",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let app = dir.join("app");
        let legacy_game = app.join("installed/a1b2c3d4e5f60708");
        let legacy_orphan = app.join("installed/deadbeef00001111");
        fs::create_dir_all(legacy_game.join("Brand")).unwrap();
        fs::write(legacy_game.join("Brand/game.exe"), b"exe").unwrap();
        fs::create_dir_all(&legacy_orphan).unwrap();
        fs::write(legacy_orphan.join("leftover.dat"), b"data").unwrap();
        let legacy_canon = fs::canonicalize(&legacy_game).unwrap();
        let lib = Library {
            version: 1,
            track_play_time: true,
            games: vec![LibraryGame {
                id: "a1b2c3d4e5f60708".into(),
                title: "Installed".into(),
                folder: legacy_canon.clone(),
                exe: "Brand/game.exe".into(),
                codepage: 932,
                program_files: Some(legacy_canon),
                ..LibraryGame::default()
            }],
            path: app.join("library.json"),
        };
        lib.save().unwrap();

        let loaded = Library::load(app.clone()).unwrap();
        let expected = fs::canonicalize(app.join("imports/a1b2c3d4e5f60708")).unwrap();
        assert_eq!(loaded.games[0].folder, expected);
        assert_eq!(loaded.games[0].program_files, Some(expected));
        assert!(app.join("imports/deadbeef00001111/leftover.dat").is_file());
        assert!(!app.join("installed").exists());

        let reloaded = Library::load(app).unwrap();
        assert_eq!(reloaded.games[0].folder, loaded.games[0].folder);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reorder_persists_a_full_permutation_only() {
        let (dir, mut lib) = fixture("reorder");
        let ids: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                fs::create_dir_all(dir.join(name)).unwrap();
                lib.add(&dir.join(name), "game.exe", 932, name).unwrap().id
            })
            .collect();
        let order = vec![ids[2].clone(), ids[0].clone(), ids[1].clone()];
        lib.reorder(&order).unwrap();
        let ids_of = |lib: &Library| lib.games.iter().map(|g| g.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids_of(&Library::load(dir.join("app")).unwrap()), order);
        for bad in [
            vec![ids[0].clone(), ids[1].clone()],
            vec![ids[0].clone(), ids[0].clone(), ids[1].clone()],
            vec![ids[0].clone(), ids[1].clone(), "other".into()],
        ] {
            assert!(lib.reorder(&bad).is_err());
        }
        assert_eq!(ids_of(&lib), order);
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-2
    #[test]
    fn candidates_suggestion_and_scan() {
        let (dir, mut lib) = fixture("candidates");
        pe(&dir, "Setup.exe", 0x14c, 300);
        pe(&dir, "large.exe", 0x8664, 200);
        pe(&dir, "game_chs.EXE", 0x14c, 100);
        pe(&dir, "config.exe", 0x14c, 90);
        pe(&dir, "odd.exe", 0xaa64, 80);
        fs::write(dir.join("invalid.exe"), b"invalid").unwrap();
        fs::create_dir(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/hidden.exe"), b"invalid").unwrap();
        let game = lib.add(&dir, "game_chs.EXE", 936, "Game").unwrap();
        #[cfg(unix)]
        {
            // An unreadable executable is skipped rather than failing the folder.
            use std::os::unix::fs::PermissionsExt;
            pe(&dir, "locked.exe", 0x14c, 500);
            fs::set_permissions(dir.join("locked.exe"), fs::Permissions::from_mode(0o0)).unwrap();
        }
        let found = inspect(&lib.known(), &dir).unwrap();
        let arch: Vec<_> = found.exes.iter().map(|e| e.arch).collect();
        assert_eq!(arch, ["x86", "x64", "x86", "x86", "other", "other"]);
        assert_eq!(found.suggested, Some(2));
        assert_eq!(found.exes[2].codepage, 936);
        assert_eq!(found.in_library, Some(game.id));
        // A scan offers every candidate, not only the suggestion.
        let row = &scan(&lib.known(), dir.clone(), 1).unwrap()[0];
        assert_eq!(row.exes.len(), found.exes.len());
        assert_eq!(row.exe.as_ref().map(|e| &e.path), Some(&found.exes[2].path));
        fs::remove_file(dir.join("game_chs.EXE")).unwrap();
        assert!(inspect(&lib.known(), &dir).unwrap().suggested.is_none());
        assert!(scan(&lib.known(), dir.clone(), 1).unwrap()[0].setup);
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-2
    #[test]
    fn scan_finds_nested_games_below_folders_with_executables() {
        let (dir, lib) = fixture("nested-games");
        pe(&dir, "Setup.exe", 0x14c, 300);
        for folder in ["game", "game/another", ".hidden"] {
            fs::create_dir(dir.join(folder)).unwrap();
            pe(&dir.join(folder), "play.exe", 0x14c, 100);
        }
        for (depth, expected) in [(1, vec!["", "game"]), (2, vec!["", "game", "game/another"])] {
            let found = scan(&lib.known(), dir.clone(), depth).unwrap();
            assert_eq!(
                found
                    .iter()
                    .map(|row| row.relative.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-1, HOST-LIB-3
    #[test]
    fn daily_splits_elapsed_seconds_at_local_midnight() {
        let tz = chrono::FixedOffset::east_opt(-5 * 3600).unwrap();
        let end = tz.with_ymd_and_hms(2026, 10, 14, 0, 0, 10).unwrap();
        let mut daily = BTreeMap::new();
        add_daily(&mut daily, end.timestamp_millis() as u64 + 500, 30, &tz);
        assert_eq!(daily["2026-10-13"], 20);
        assert_eq!(daily["2026-10-14"], 10);
        add_daily(&mut daily, end.timestamp_millis() as u64, 90_000, &tz);
        assert_eq!(daily.values().sum::<u64>(), 90_030);
    }
    // spec: HOST-LIB-1, HOST-LIB-3
    #[test]
    fn old_library_backfills_daily_once_and_defaults_last_run() {
        let (dir, mut lib) = fixture("migration");
        let game = lib.add(&dir, "game.exe", 932, "Game").unwrap();
        let end = Local.with_ymd_and_hms(2026, 10, 13, 12, 0, 0).unwrap();
        lib.games[0].sessions.push(PlaySession {
            ended: end.timestamp_millis() as u64,
            seconds: 15,
        });
        lib.save().unwrap();
        assert!(
            Library::load(dir.join("app")).unwrap().games[0]
                .daily
                .is_empty()
        );
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&lib.path).unwrap()).unwrap();
        let fields = json["games"][0].as_object_mut().unwrap();
        fields.remove("daily");
        fields.remove("lastRun");
        fs::write(&lib.path, serde_json::to_vec(&json).unwrap()).unwrap();
        let mut loaded = Library::load(dir.join("app")).unwrap();
        assert_eq!(loaded.games[0].daily["2026-10-13"], 15);
        assert!(loaded.games[0].last_run.is_none());
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(&lib.path).unwrap()).unwrap();
        assert_eq!(stored["games"][0]["daily"]["2026-10-13"], 15);
        loaded.prune(end.timestamp_millis() as u64 + MONTH + 1);
        loaded.save().unwrap();
        let reloaded = Library::load(dir.join("app")).unwrap();
        assert_eq!(reloaded.games[0].daily["2026-10-13"], 15);
        assert!(reloaded.games[0].sessions.is_empty());
        assert_eq!(reloaded.games[0].id, game.id);
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-3
    #[test]
    fn last_run_round_trips_summary_and_error() {
        let (dir, mut lib) = fixture("last-run");
        let game = lib.add(&dir, "game.exe", 932, "Game").unwrap();
        for run in [
            LastRun {
                ended: now(),
                summary: Some(adv_report::RunSummary::default()),
                error: None,
            },
            LastRun {
                ended: now(),
                summary: None,
                error: Some("execution failed".into()),
            },
        ] {
            lib.finished(&game.id, 1, run.clone()).unwrap();
            let loaded = Library::load(dir.join("app")).unwrap();
            assert_eq!(loaded.games[0].last_run, Some(run));
        }
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: HOST-LIB-2
    #[test]
    fn discovery_matches_the_selected_executable() {
        let (dir, mut lib) = fixture("exe-matching");
        pe(&dir, "other.exe", 0x14c, 80);
        pe(&dir, "game.exe", 0x14c, 100);
        let other = lib.add(&dir, "OTHER.EXE", 932, "Other").unwrap();
        let found = inspect(&lib.known(), &dir.join(".")).unwrap();
        assert_eq!(found.in_library, Some(other.id.clone()));
        assert_eq!(found.exes[1].in_library, Some(other.id));
        assert!(found.exes[0].in_library.is_none());
        assert!(
            scan(&lib.known(), dir.clone(), 1).unwrap()[0]
                .in_library
                .is_none()
        );
        let game = lib.add(&dir, "GAME.EXE", 932, "Game").unwrap();
        assert_eq!(
            scan(&lib.known(), dir.clone(), 1).unwrap()[0].in_library,
            Some(game.id)
        );
        fs::remove_dir_all(dir).unwrap();
    }
    // spec: AND-APP-9
    #[test]
    fn android_volumes_classify_primary_and_disconnected_removable_media() {
        let p = Path::new;
        assert_eq!(
            android_external_volume(p("/storage/emulated/0/Games/Novel")),
            Some((p("/storage/emulated/0"), false))
        );
        assert_eq!(
            android_external_volume(p("/storage/9C33-6BBD/Games/Novel")),
            Some((p("/storage/9C33-6BBD"), true))
        );
        for invalid in [
            "/storage",
            "/storage/emulated",
            "/storage/emulated/bad/Games",
            "/storage/self/primary/Games",
            "/data/user/0/org.hitode.launcher/files/imports/1",
        ] {
            assert_eq!(android_external_volume(p(invalid)), None);
        }
        assert_eq!(
            offline(p("/storage/emulated/0/Games/Novel")),
            Some("missing")
        );
        assert_eq!(offline(p("/storage/9C33-6BBD/Games/Novel")), Some("disk"));
    }

    // spec: HOST-LIB-11
    #[test]
    fn inspection_lists_and_ranks_top_level_registry_files() {
        let (dir, lib) = fixture("registry-files");
        pe(&dir, "game.exe", 0x14c, 100);
        fs::write(dir.join("notes.txt"), b"ignore").unwrap();
        fs::write(dir.join("other.REG"), b"").unwrap();
        fs::write(dir.join("launch.bat"), b"").unwrap();
        fs::write(dir.join("注册表.reg"), b"").unwrap();
        fs::write(dir.join("注册表恢复.BAT"), b"").unwrap();
        fs::create_dir(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/deep.reg"), b"").unwrap();
        let found = inspect(&lib.known(), &dir).unwrap();
        assert_eq!(
            found.registry_files,
            vec!["注册表恢复.BAT", "注册表.reg", "launch.bat", "other.REG"]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

fn tracking_default() -> bool {
    true
}

#[tauri::command]
pub fn set_tracking(state: Store<'_>, enabled: bool) -> Reply<()> {
    let mut library = state.lock().unwrap();
    library.track_play_time = enabled;
    reply(library.save())
}
