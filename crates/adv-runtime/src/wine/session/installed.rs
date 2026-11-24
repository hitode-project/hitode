//! Where setups put their programs: the folders the prefix's uninstall
//! entries name, found on the host (WINE-SES-14). A setup told to install
//! outside `C:\Program Files` leaves its game there, and its entry says where.
use super::*;
use adv_nt::registry::Client as RegistryClient;

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;

/// A program a setup registered for removal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Its `DisplayName`, or else its entry's name.
    pub name: String,
    /// The host folder it lies in.
    pub folder: PathBuf,
}

pub(super) fn installed(
    host: &mut Host,
    config: &Config<'_>,
    options: &SessionOptions,
) -> Result<Vec<Installed>, String> {
    let (prefix, _) = host.prefix(config)?;
    let drive_c = files::drive_c(config, &prefix);
    let chosen = options.install_dir.as_ref().map(|folder| {
        let folder = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        adv_nt::file::volume::Install::chosen(folder, &drive_c)
    });
    let entries = host.serve(config, |socket, shared, time| {
        let (mut connection, _) = adv_nt::server::Connection::connect(socket, shared, time)
            .map_err(|e| format!("registry connection: {e}"))?;
        let mut client = RegistryClient::new(&mut connection, shared, time);
        let user = client.user_key().map_err(|e| format!("user key: {e:?}"))?;
        let mut entries = Vec::new();
        for root in [
            format!(r"\Registry\Machine\{UNINSTALL}"),
            format!(r"{user}\{UNINSTALL}"),
        ] {
            // A prefix no setup has written to has no such key.
            let Ok(tree) = client.snapshot(&root) else {
                continue;
            };
            for (key, values) in tree {
                if key.is_empty() || key.contains('\\') {
                    continue;
                }
                let text = |name: &str| {
                    values
                        .iter()
                        .find(|(value, _)| value.eq_ignore_ascii_case(name))
                        .filter(|(_, (kind, _))| matches!(*kind, REG_SZ | REG_EXPAND_SZ))
                        .map(|(_, (_, bytes))| utf16(bytes))
                        .filter(|text| !text.is_empty())
                };
                let location = text("InstallLocation")
                    .or_else(|| text("DisplayIcon").and_then(|icon| directory(&icon)))
                    .or_else(|| text("UninstallString").and_then(|command| directory(&command)));
                if let Some(location) = location {
                    entries.push((text("DisplayName").unwrap_or(key), location));
                }
            }
        }
        Ok(entries)
    })?;
    let mut installed: Vec<Installed> = entries
        .into_iter()
        .filter_map(|(name, location)| {
            let folder = host_folder(&location, &drive_c, chosen.as_ref())?;
            Some(Installed { name, folder })
        })
        .collect();
    installed.dedup_by(|a, b| a.folder == b.folder);
    Ok(installed)
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units).trim().to_owned()
}

/// The folder of the program an icon or uninstall command names. An
/// InstallShield uninstaller in the Windows folder names its log with `-f`.
fn directory(command: &str) -> Option<String> {
    let path = match command.to_ascii_lowercase().find("-f") {
        Some(at) if command[..at].to_ascii_lowercase().contains(r"\windows\") => &command[at + 2..],
        _ => command,
    };
    let path = path.trim_start();
    let path = match path.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        // An icon index follows a comma; arguments follow the program.
        None => {
            let end = path.to_ascii_lowercase().find(".exe").map(|at| at + 4);
            &path[..end.unwrap_or(path.len())]
        }
    };
    let path = path.split(',').next()?;
    Some(path[..path.rfind('\\')?].to_owned())
}

/// The host folder behind a DOS folder on drive C:, matched without case as
/// Windows names it. Drive C: itself, its Windows folder and `C:\Program
/// Files` are where everything lies, so they name no program.
fn host_folder(
    dos: &str,
    drive_c: &Path,
    chosen: Option<&adv_nt::file::volume::Install>,
) -> Option<PathBuf> {
    let mut dos = dos.trim().trim_end_matches('\\').to_owned();
    const PROGRAM_FILES: &str = "%ProgramFiles%";
    if dos
        .get(..PROGRAM_FILES.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(PROGRAM_FILES))
    {
        dos = format!(r"C:\Program Files{}", &dos[PROGRAM_FILES.len()..]);
    }
    if !dos
        .get(..3)
        .is_some_and(|drive| drive.eq_ignore_ascii_case(r"C:\"))
    {
        return None;
    }
    let mut parts: Vec<&str> = dos[3..]
        .split('\\')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() || parts[0].eq_ignore_ascii_case("windows") {
        return None;
    }
    let mut folder = drive_c.to_owned();
    if let Some(chosen) = chosen.filter(|chosen| chosen.holds_root(parts[0])) {
        folder = chosen.folder().to_owned();
    } else if parts[0].eq_ignore_ascii_case("Program Files") {
        parts.remove(0);
        if let Some(chosen) = chosen {
            folder = chosen.folder().to_owned();
        } else {
            folder.push("Program Files");
        }
        if parts.is_empty() {
            return None;
        }
    }
    for part in parts {
        let entry = std::fs::read_dir(&folder).ok()?.flatten().find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(part))
        })?;
        folder = entry.path();
    }
    folder.is_dir().then_some(folder)
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-SES-14
    #[test]
    fn uninstall_entries_name_the_folder_their_program_lies_in() {
        // privacy: allow authored guest paths below
        assert_eq!(
            directory(r#""C:\Games\Probe\game.exe",0"#).as_deref(),
            Some(r"C:\Games\Probe")
        ); // privacy: allow authored guest path
        assert_eq!(
            directory(r"C:\Games\Probe\unins000.exe /SILENT").as_deref(),
            Some(r"C:\Games\Probe")
        ); // privacy: allow authored guest path
        assert_eq!(
            directory(r#"C:\WINDOWS\IsUninst.exe -f"C:\Program Files\Probe\Uninst.isu""#)
                .as_deref(),
            Some(r"C:\Program Files\Probe")
        );
        let root = std::env::temp_dir().join(format!("adv-installed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let drive_c = root.join("drive_c");
        let custom = root.join("custom");
        std::fs::create_dir_all(drive_c.join("Games/Probe Game")).unwrap();
        std::fs::create_dir_all(drive_c.join("windows")).unwrap();
        std::fs::create_dir_all(custom.join("Vendor/Game")).unwrap();
        // A setup made `C:\Studio` while the folder was chosen.
        std::fs::create_dir_all(custom.join("Studio/Game")).unwrap();
        std::fs::write(custom.join(".hitode-drive-c"), "Studio\n").unwrap();
        let folder = custom.clone();
        let custom = adv_nt::file::volume::Install::chosen(custom, &drive_c);
        assert_eq!(
            host_folder(r"C:\studio\Game", &drive_c, Some(&custom)), // privacy: allow authored guest path
            Some(folder.join("Studio/Game"))
        );
        assert_eq!(
            host_folder(r"c:\games\PROBE GAME\", &drive_c, Some(&custom)),
            Some(drive_c.join("Games/Probe Game"))
        );
        assert_eq!(
            host_folder(r"C:\Program Files\vendor\game", &drive_c, Some(&custom)),
            Some(folder.join("Vendor/Game"))
        );
        assert_eq!(
            host_folder(r"%ProgramFiles%\Vendor\Game", &drive_c, Some(&custom)),
            Some(folder.join("Vendor/Game"))
        );
        for nothing in [
            r"C:\",
            r"C:\windows",
            r"C:\Program Files",
            r"C:\Games\Missing",
        ] {
            assert_eq!(
                host_folder(nothing, &drive_c, Some(&custom)),
                None,
                "{nothing}"
            );
        }
        assert_eq!(host_folder(r"D:\SETUP", &drive_c, None), None); // privacy: allow authored guest path
        let _ = std::fs::remove_dir_all(&root);
    }
}
