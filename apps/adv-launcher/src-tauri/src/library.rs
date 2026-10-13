//! Desktop game library and executable discovery.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::UNIX_EPOCH,
};
use tauri::{AppHandle, State};
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
    pub folder: PathBuf,
    pub exe: String,
    pub codepage: u32,
    pub added_at: u64,
    pub last_played: Option<u64>,
    pub play_seconds: u64,
    pub favorite: bool,
    pub nsfw: bool,
    pub sessions: Vec<PlaySession>,
    #[serde(skip_deserializing)]
    pub offline: Option<Offline>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaySession {
    ended: u64,
    seconds: u32,
}
type Offline = &'static str;
#[derive(Serialize, Deserialize)]
pub struct Library {
    version: u32,
    games: Vec<LibraryGame>,
    #[serde(skip)]
    path: PathBuf,
}
impl Library {
    pub fn load(dir: PathBuf) -> io::Result<Self> {
        let path = dir.join("library.json");
        let mut lib = Self {
            version: 1,
            games: vec![],
            path,
        };
        match fs::read(&lib.path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(saved) => lib.games = saved.games,
                Err(e) => {
                    let bad = dir.join(format!("library.json.bad-{}", now()));
                    fs::rename(&lib.path, bad)?;
                    eprintln!("invalid game library moved aside: {e}");
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(lib)
    }
    fn save(&self) -> io::Result<()> {
        fs::create_dir_all(self.path.parent().unwrap())?;
        let mut value = serde_json::to_value(self)?;
        for game in value["games"].as_array_mut().unwrap() {
            game.as_object_mut().unwrap().remove("offline");
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&value)?).and_then(|()| fs::rename(tmp, &self.path))
    }
    fn game(&self, id: &str) -> io::Result<usize> {
        let index = self.games.iter().position(|g| g.id == id);
        index.ok_or_else(|| io::Error::other("game not found"))
    }
    pub fn launch(&self, id: &str) -> io::Result<(PathBuf, PathBuf, u32)> {
        let game = &self.games[self.game(id)?];
        let data = self.path.parent().unwrap().join("games").join(id);
        Ok((game.folder.join(&game.exe), data, game.codepage))
    }
    pub fn finished(&mut self, id: &str, ended: u64, seconds: u32) -> io::Result<()> {
        let i = self.game(id)?;
        let game = &mut self.games[i];
        game.sessions.push(PlaySession { ended, seconds });
        game.last_played = Some(ended);
        game.play_seconds += u64::from(seconds);
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
    fn remove(&mut self, id: &str) -> io::Result<LibraryGame> {
        let i = self.game(id)?;
        let game = self.games.remove(i);
        self.save()?;
        Ok(game)
    }
    fn restore(&mut self, game: LibraryGame, index: usize) -> io::Result<()> {
        if self.games.iter().any(|g| g.id == game.id) {
            return Err(io::Error::other("game id already exists"));
        }
        self.games.insert(index.min(self.games.len()), game);
        self.save()
    }
    /// Folders and ids already in the library, so discovery can run unlocked.
    fn known(&self) -> Known {
        self.games
            .iter()
            .map(|g| (g.folder.clone(), g.id.clone()))
            .collect()
    }
}
type Known = Vec<(PathBuf, String)>;
fn inspect(known: &Known, folder: &Path) -> io::Result<FolderInspection> {
    let folder = fs::canonicalize(folder)?;
    let mut exes = Vec::new();
    for entry in fs::read_dir(&folder)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_file() || !name.to_ascii_lowercase().ends_with(".exe") {
            continue;
        }
        // An unreadable executable is not a candidate; it must not hide the others.
        let (Ok(metadata), Ok(codepage)) = (entry.metadata(), adv_shell::detect_codepage(&path))
        else {
            continue;
        };
        exes.push(ExeCandidate {
            path: name,
            size: metadata.len(),
            arch: machine(&path),
            codepage,
        });
    }
    exes.sort_by_key(|e| std::cmp::Reverse(e.size));
    let suggested = exes.iter().position(|e| {
        e.arch == "x86" && !prefixed(&e.path, &["setup", "install", "unins", "config"])
    });
    let name = folder.file_name().unwrap_or_default();
    let title = name.to_string_lossy().into();
    let existing = known.iter().find(|(known, _)| *known == folder);
    let in_library = existing.map(|(_, id)| id.clone());
    Ok(FolderInspection {
        folder,
        title,
        exes,
        suggested,
        in_library,
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
            found.push(ScanFound {
                relative: relative.replace('\\', "/"),
                folder: info.folder,
                depth: level,
                title: info.title,
                exe: info.suggested.map(|i| info.exes[i].clone()),
                setup,
                in_library: info.in_library,
            });
            if found.len() == 500 {
                break;
            }
        } else if level < depth {
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
fn offline(folder: &Path) -> Option<Offline> {
    if folder.exists() {
        return None;
    }
    let disk = folder
        .ancestors()
        .find(|p| p.parent() == Some(Path::new("/Volumes")));
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
pub struct ExeCandidate {
    path: String,
    size: u64,
    arch: &'static str,
    codepage: u32,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInspection {
    folder: PathBuf,
    title: String,
    exes: Vec<ExeCandidate>,
    suggested: Option<usize>,
    in_library: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanFound {
    folder: PathBuf,
    relative: String,
    depth: u8,
    title: String,
    exe: Option<ExeCandidate>,
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
pub fn library_list(state: Store<'_>) -> Reply<Vec<LibraryGame>> {
    let mut library = state.lock().unwrap();
    if library.prune(now()) {
        library.save().map_err(|e| e.to_string())?;
    }
    let mut games = library.games.clone();
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
    reply(state.lock().unwrap().add(&folder, &exe, codepage, &title))
}
#[tauri::command]
pub fn library_update(state: Store<'_>, id: String, patch: GamePatch) -> Reply<LibraryGame> {
    let mut library = state.lock().unwrap();
    let folder = reply(patch.folder.map(fs::canonicalize).transpose())?;
    let i = library.game(&id).map_err(|e| e.to_string())?;
    let game = &mut library.games[i];
    game.title = patch.title.unwrap_or_else(|| game.title.clone());
    game.codepage = patch.codepage.unwrap_or(game.codepage);
    game.favorite = patch.favorite.unwrap_or(game.favorite);
    game.nsfw = patch.nsfw.unwrap_or(game.nsfw);
    game.folder = folder.unwrap_or_else(|| game.folder.clone());
    if let Some(exe) = patch.exe {
        game.exe = exe.replace('\\', "/");
    }
    let game = game.clone();
    library.save().map_err(|e| e.to_string())?;
    Ok(game)
}
#[tauri::command]
pub fn library_remove(state: Store<'_>, id: String) -> Reply<LibraryGame> {
    reply(state.lock().unwrap().remove(&id))
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
            lib.finished(&game.id, ended, seconds).unwrap();
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
            fs::set_permissions(dir.join("locked.exe"), fs::Permissions::from_mode(0)).unwrap();
        }
        let found = inspect(&lib.known(), &dir).unwrap();
        let arch: Vec<_> = found.exes.iter().map(|e| e.arch).collect();
        assert_eq!(arch, ["x86", "x64", "x86", "x86", "other", "other"]);
        assert_eq!(found.suggested, Some(2));
        assert_eq!(found.exes[2].codepage, 936);
        assert_eq!(found.in_library, Some(game.id));
        fs::remove_file(dir.join("game_chs.EXE")).unwrap();
        assert!(inspect(&lib.known(), &dir).unwrap().suggested.is_none());
        assert!(scan(&lib.known(), dir.clone(), 1).unwrap()[0].setup);
        fs::remove_dir_all(dir).unwrap();
    }
}
