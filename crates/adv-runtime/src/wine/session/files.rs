//! Adapt the existing optical VFS to Wine's neutral read-only volume contract.
//! No ISO parsing or file data is duplicated here. The family retains the VFS,
//! its media owners and its shared lazy section pages through NT mount owners.
use super::*;
use adv_nt::file::volume::{Entry, Info, Install, Mounts, ReadFile, Volume};
use adv_os::vfs::{DirEntry, FileSection, OpticalFile};
use std::io;

struct OpticalVolume {
    vfs: Rc<adv_os::Vfs>,
    letter: char,
}
struct OpticalReader {
    file: OpticalFile,
    pages: Rc<FileSection>,
}
impl ReadFile for OpticalReader {
    fn read_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read_at(offset, bytes)
    }
    fn page(&self, index: u64) -> Rc<adv_cpu::backend::FilePage> {
        self.pages.page(index)
    }
}
/// The ANSI code page of a session's `locale`.
pub(super) fn codepage(locale: u32) -> u32 {
    match locale {
        0x411 => 932,
        0x804 => 936,
        0x404 => 950,
        _ => 1252,
    }
}
/// The medium of drive `letter` read from `image`, with names in `codepage`.
pub(super) fn disc(letter: char, image: &Path, codepage: u32) -> Result<Rc<dyn Volume>, String> {
    let mut cfg = adv_os::OsConfig::new("", "", "");
    cfg.guest_root = r"C:\".into();
    cfg.codepage = codepage;
    let mut vfs = adv_os::Vfs::new(&cfg);
    vfs.mount_disc(letter, image)
        .map_err(|e| format!("CD-ROM {letter}: {e}"))?;
    Ok(Rc::new(OpticalVolume {
        vfs: Rc::new(vfs),
        letter,
    }))
}
fn entry(entry: DirEntry) -> Entry {
    Entry {
        name: entry.name,
        directory: entry.is_dir,
        size: entry.size,
        allocated: entry.size.next_multiple_of(2048),
        created: entry.created,
        accessed: entry.accessed,
        modified: entry.modified,
        attributes: entry.attributes,
    }
}
impl OpticalVolume {
    fn path(&self, path: &str) -> String {
        format!("{}:\\{path}", self.letter)
    }
}
impl Volume for OpticalVolume {
    fn info(&self) -> Info {
        let info = self.vfs.optical_drive(self.letter).expect("mounted medium");
        Info {
            label: info.label.clone(),
            serial: info.serial,
            sector_size: info.bytes_per_sector,
            total_bytes: info.total_bytes,
            filesystem: info.filesystem.into(),
            flags: info.filesystem_flags,
            max_name: info.max_component_length,
        }
    }
    fn entry(&self, path: &str) -> io::Result<Entry> {
        self.vfs.optical_entry(&self.path(path)).map(entry)
    }
    fn list(&self, path: &str) -> io::Result<Vec<Entry>> {
        self.vfs
            .list(&self.path(path))
            .ok_or_else(|| io::ErrorKind::NotADirectory.into())
            .map(|entries| entries.into_iter().map(entry).collect())
    }
    fn open(&self, path: &str) -> io::Result<Rc<dyn ReadFile>> {
        let stream = self.vfs.open_optical_file(&self.path(path))?;
        let file = stream.optical_file().expect("optical stream").clone();
        let pages = self.vfs.sections.optical(file.clone());
        Ok(Rc::new(OpticalReader { file, pages }))
    }
}

/// Drive and mount layout prepared for a Wine session, and its main image.
pub(super) struct Prepared {
    pub mounts: Mounts,
    /// The DOS path of the main image, its command line and the directory it
    /// starts in.
    pub image: String,
    pub command_line: String,
    pub start: String,
    /// The main image on the host, when it has one.
    pub host_image: Option<PathBuf>,
}

/// Whether `exe` carries an appended virtual filesystem bundle whose path
/// matching strips `directory_len + 1` characters, assuming the program
/// directory never ends with a backslash (which fails at a bare drive root,
/// where `RtlDosPathNameToNtPathName_U` returns a trailing backslash).
fn needs_subdirectory_launch(exe: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(exe) else {
        return false;
    };
    let mut header = Vec::new();
    if file
        .by_ref()
        .take(64 * 1024)
        .read_to_end(&mut header)
        .is_err()
    {
        return false;
    }
    let Ok(pe) = adv_pe::Pe::parse_headers(&header) else {
        return false;
    };
    if pe.sections.iter().any(|section| {
        section.name == ".ultra"
            && pe.entry_rva >= section.virtual_address
            && pe.entry_rva < section.virtual_address.saturating_add(section.mem_size())
    }) {
        return true;
    }
    let raw_end = pe
        .sections
        .iter()
        .map(|section| u64::from(section.raw_offset) + u64::from(section.raw_size))
        .max()
        .unwrap_or(0);
    if raw_end == 0 || file.seek(SeekFrom::Start(raw_end)).is_err() {
        return false;
    }
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).is_ok() && &magic == b"XOJUMANJ"
}

/// `path` below `root` as a DOS path under `base`, which ends in a backslash.
fn below(path: &Path, root: &Path, base: &str) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut dos = base.to_owned();
    for part in relative.components() {
        if !dos.ends_with('\\') {
            dos.push('\\');
        }
        dos.push_str(part.as_os_str().to_str()?);
    }
    Some(dos)
}

fn with_slash(mut directory: String) -> String {
    if !directory.ends_with('\\') {
        directory.push('\\');
    }
    directory
}

/// A DOS path names a drive or a UNC share; anything else is relative.
fn absolute_dos(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.get(1) == Some(&b':') || path.starts_with(r"\\")
}

/// Wine's command interpreter, which runs scripts.
const CMD: &str = r"C:\windows\system32\cmd.exe"; // privacy: allow authored guest path

/// Where the program folder appears: on drive C: when it lies there, in
/// Program Files for a bundled VFS program, otherwise on the first free drive.
struct Placement {
    /// The host folder and its DOS directory, ending in a backslash.
    folder: Option<(PathBuf, String)>,
    /// The drive the folder takes, if any.
    drive: Option<char>,
    /// The host directory `C:\Program Files` shows instead of the prefix's.
    install: Option<PathBuf>,
}

fn place(
    config: &Config<'_>,
    options: &SessionOptions,
    drive_c: &Path,
    chosen: Option<&Install>,
) -> Result<Placement, String> {
    let Some(exe) = config.exe else {
        return Ok(Placement {
            folder: None,
            drive: None,
            install: None,
        });
    };
    let game = std::path::absolute(exe.parent().ok_or("the program has no directory")?)
        .map_err(|e| e.to_string())?;
    // Containment is decided on resolved paths; the drive links the path as given.
    let resolved = game.canonicalize().unwrap_or_else(|_| game.clone());
    let (directory, drive, install) = if let Some(dos) = below(&resolved, drive_c, r"C:\") {
        (with_slash(dos), None, None)
    } else if let Some(dos) = chosen.and_then(|chosen| chosen.dos(&resolved)) {
        // Installed through `install_dir`: where its setup put it.
        (with_slash(dos), None, None)
    } else if options.install_dir.is_none() && needs_subdirectory_launch(exe) {
        (r"C:\Program Files\".to_owned(), None, Some(resolved))
    } else {
        let letter = free_drives(options, None)
            .next()
            .ok_or("no drive letter remains for the program directory")?;
        (format!(r"{letter}:\"), Some(letter), None)
    };
    Ok(Placement {
        folder: Some((game, directory)),
        drive,
        install,
    })
}

/// The folder the options choose for programs on drive C:, made if missing.
pub(super) fn installation(
    options: &SessionOptions,
    drive_c: &Path,
) -> Result<Option<Install>, String> {
    let Some(folder) = &options.install_dir else {
        return Ok(None);
    };
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let folder = folder.canonicalize().map_err(|e| e.to_string())?;
    Ok(Some(Install::chosen(folder, drive_c)))
}

/// Drive letters from D: that neither a disc nor `taken` uses.
fn free_drives(options: &SessionOptions, taken: Option<char>) -> impl Iterator<Item = char> + '_ {
    ('D'..='Z').filter(move |letter| {
        Some(*letter) != taken
            && !options
                .cdroms
                .iter()
                .any(|(mounted, _)| mounted.to_ascii_uppercase() == *letter)
    })
}

pub(super) fn drive_c(config: &Config<'_>, prefix: &Path) -> PathBuf {
    let drive = config
        .drive
        .map_or_else(|| prefix.join("drive_c"), Path::to_owned);
    drive.canonicalize().unwrap_or(drive)
}

pub(super) fn prepare(
    config: &Config<'_>,
    options: &SessionOptions,
    prefix: &Path,
) -> Result<Prepared, String> {
    let drive_c = drive_c(config, prefix);
    let chosen = installation(options, &drive_c)?;
    let Placement {
        folder,
        drive,
        install,
    } = place(config, options, &drive_c, chosen.as_ref())?;
    let mut links: Vec<(char, PathBuf)> = Vec::new();
    if let (Some(letter), Some((game, _))) = (drive, &folder) {
        links.push((letter, game.clone()));
    }
    let directory = folder
        .as_ref()
        .map_or_else(|| r"C:\".to_owned(), |(_, directory)| directory.clone());
    let (image, host_image) = match options.program.as_ref().map(|program| &program.image) {
        None => {
            let exe = config.exe.ok_or("a session needs a program")?;
            let name = exe
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("the program name is not Unicode")?;
            (format!("{directory}{name}"), Some(exe.to_owned()))
        }
        Some(Image::Guest(path)) if absolute_dos(path) => (path.clone(), None),
        Some(Image::Guest(path)) => (format!("{directory}{path}"), None),
        Some(Image::Host(path)) => {
            let path = std::path::absolute(path).map_err(|e| e.to_string())?;
            let path = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let dos = folder
                .as_ref()
                .and_then(|(game, directory)| {
                    let game = game.canonicalize().unwrap_or_else(|_| game.clone());
                    below(&path, &game, directory)
                })
                .or_else(|| below(&path, &drive_c, r"C:\"))
                .or_else(|| chosen.as_ref().and_then(|chosen| chosen.dos(&path)));
            let dos = match dos {
                Some(dos) => dos,
                None => {
                    let letter = free_drives(options, drive)
                        .next()
                        .ok_or("no drive letter remains for the program")?;
                    links.push((
                        letter,
                        path.parent().ok_or("the program has no directory")?.into(),
                    ));
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("the program name is not Unicode")?;
                    format!(r"{letter}:\{name}")
                }
            };
            (dos, Some(path))
        }
    };
    // A script runs in cmd.exe, as Windows starts one; it reads a host
    // script without the pauses and elevation it carries for a console
    // window (WINE-SES-11).
    let script = options.program.is_some() && super::registry::is_script(Path::new(&image));
    let (command_line, start) = match &options.program {
        None => (
            image.rsplit('\\').next().unwrap_or_default().to_owned(),
            directory.clone(),
        ),
        Some(program) => {
            let mut command_line = format!("\"{image}\"");
            if !program.arguments.is_empty() {
                command_line.push(' ');
                command_line.push_str(&program.arguments);
            }
            if script {
                command_line = format!("\"{CMD}\" /d /c \"{command_line}\"");
            }
            let start = match &program.directory {
                Some(path) if absolute_dos(path) => with_slash(path.clone()),
                Some(path) => with_slash(format!("{directory}{path}")),
                None => {
                    image[..=image.rfind('\\').ok_or("the program has no directory")?].to_owned()
                }
            };
            (command_line, start)
        }
    };

    let codepage = codepage(config.locale);
    let mut cfg = adv_os::OsConfig::new(
        folder
            .as_ref()
            .map_or(drive_c.as_path(), |(game, _)| game.as_path()),
        config
            .exe
            .and_then(|exe| exe.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        prefix,
    );
    cfg.guest_root = directory.clone();
    cfg.system_dir = drive_c.clone();
    cfg.codepage = codepage;
    // Opening the game folder completes the state its installer leaves for
    // a copied folder.
    adv_os::Vfs::new(&cfg);
    let discs = options
        .cdroms
        .iter()
        .map(|(letter, image)| Ok((*letter, disc(*letter, image, codepage)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let mut replacements: Vec<(PathBuf, Vec<u8>)> = folder
        .as_ref()
        .and_then(|(game, _)| crate::reallive::configuration(game))
        .into_iter()
        .collect();
    replacements.extend(options.replacements.iter().cloned());
    if let Some(path) = host_image.as_ref().filter(|_| script) {
        let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        replacements.push((
            path.clone(),
            super::registry::sanitize_batch(&raw, codepage),
        ));
    }
    let (image, host_image) = if script {
        (CMD.to_owned(), None)
    } else {
        (image, host_image)
    };
    let root = if options.cdroms.is_empty() && replacements.is_empty() {
        None
    } else {
        let root =
            std::path::absolute(prefix.join(format!(".hitode-files-{}", std::process::id())))
                .map_err(|e| e.to_string())?;
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        Some(root.canonicalize().map_err(|e| e.to_string())?)
    };
    let mut mounts = Mounts::new(chosen.or(install.map(Install::program)), root.clone());
    for (letter, volume) in discs {
        let path = root
            .as_ref()
            .expect("session files directory")
            .join(letter.to_string());
        mounts
            .attach(letter, path, volume)
            .map_err(|e| e.to_string())?;
    }
    for (index, (original, bytes)) in replacements.into_iter().enumerate() {
        let directory = root
            .as_ref()
            .expect("session files directory")
            .join(format!("replaced/{index}"));
        let file = directory.join(original.file_name().expect("a file name"));
        std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::write(&file, bytes))
            .and_then(|()| mounts.replace(&original, file))
            .map_err(|e| format!("{}: {e}", original.display()))?;
    }
    let devices = prefix.join("dosdevices");
    let managed = prefix.join(".hitode-drives");
    if let Ok(letters) = std::fs::read_to_string(&managed) {
        for letter in letters.chars().filter(char::is_ascii_uppercase) {
            let path = devices.join(format!("{}:", letter.to_ascii_lowercase()));
            if path.is_symlink() {
                adv_wineserver::names_changed();
                std::fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
    }
    let mut letters = String::new();
    for (letter, root) in links.into_iter().chain(mounts.roots()) {
        link(
            &root,
            &devices.join(format!("{}:", letter.to_ascii_lowercase())),
        )?;
        letters.push(letter);
    }
    std::fs::write(managed, letters).map_err(|e| e.to_string())?;
    Ok(Prepared {
        mounts,
        image,
        command_line,
        start,
        host_image,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_pe::scn;
    use adv_pe::write::DllWriter;

    fn write_pe(path: &Path, entry_section: &str, overlay: &[u8]) {
        let mut writer = DllWriter::new("test.exe", 0x0040_0000);
        let text = writer
            .section(
                ".text",
                vec![0xc3; 16],
                16,
                scn::CNT_CODE | scn::MEM_EXECUTE,
            )
            .unwrap();
        let entry = if entry_section == ".text" {
            text
        } else {
            writer
                .section(
                    entry_section,
                    vec![0xc3; 16],
                    16,
                    scn::CNT_CODE | scn::MEM_EXECUTE,
                )
                .unwrap()
        };
        writer.set_entry(entry);
        let mut bytes = writer.finish().unwrap();
        bytes.extend_from_slice(overlay);
        std::fs::write(path, bytes).unwrap();
    }

    fn test_config<'a>(dist: &'a Path, prefix: &'a Path, exe: &'a Path) -> Config<'a> {
        Config {
            dist,
            prefix,
            drive: None,
            locale: 0x409,
            fonts: "",
            exe: Some(exe),
            width: 640,
            height: 480,
            jit: true,
            debug: "",
            stdout: None,
        }
    }

    // spec: WINE-SES-1, WINE-SES-11, WINE-SES-12, WINE-SES-15
    #[test]
    fn programs_keep_their_drive_c_and_install_paths_and_others_take_a_drive() {
        let root = std::env::temp_dir().join(format!("adv-programs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prefix = root.join("prefix");
        let installed = prefix.join("drive_c/Program Files/Vendor/Game");
        let custom = root.join("custom/Vendor/Game");
        let (game, elsewhere) = (root.join("game"), root.join("elsewhere"));
        let devices = prefix.join("dosdevices");
        for dir in [
            &installed,
            &custom,
            &game.join("tools"),
            &elsewhere,
            &devices,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for exe in [
            installed.join("game.exe"),
            custom.join("game.exe"),
            game.join("game.exe"),
            game.join("tools/config.exe"),
            elsewhere.join("patch.exe"),
        ] {
            write_pe(&exe, ".text", b"");
        }
        let start = |exe: Option<&Path>, options: &SessionOptions| {
            let config = Config {
                exe,
                ..test_config(&root, &prefix, &root)
            };
            let prepared = prepare(&config, options, &prefix).unwrap();
            (prepared.image, prepared.command_line, prepared.start)
        };
        let program = |image: Image| SessionOptions {
            program: Some(Program {
                arguments: "/x".into(),
                ..Program::new(image)
            }),
            ..SessionOptions::default()
        };
        assert_eq!(
            start(
                Some(&installed.join("game.exe")),
                &SessionOptions::default()
            ),
            (
                r"C:\Program Files\Vendor\Game\game.exe".into(),
                "game.exe".into(),
                r"C:\Program Files\Vendor\Game\".into()
            )
        );
        let into_custom = SessionOptions {
            install_dir: Some(root.join("custom")),
            ..SessionOptions::default()
        };
        assert_eq!(
            start(Some(&custom.join("game.exe")), &into_custom).0,
            r"C:\Program Files\Vendor\Game\game.exe"
        );
        // One its setup made at the root of drive C: keeps that path (WINE-SES-15).
        let studio = root.join("custom/Studio/Game");
        std::fs::create_dir_all(&studio).unwrap();
        write_pe(&studio.join("game.exe"), ".text", b"");
        std::fs::write(root.join("custom/.hitode-drive-c"), "Studio\n").unwrap();
        assert_eq!(
            start(Some(&studio.join("game.exe")), &into_custom).0,
            r"C:\Studio\Game\game.exe" // privacy: allow authored guest path
        );
        // A tool in the game folder starts in its own directory there.
        assert_eq!(
            start(
                Some(&game.join("game.exe")),
                &program(Image::Host(game.join("tools/config.exe")))
            ),
            (
                r"D:\tools\config.exe".into(), // privacy: allow authored guest path
                r#""D:\tools\config.exe" /x"#.into(), // privacy: allow authored guest path
                r"D:\tools\".into()            // privacy: allow authored guest path
            )
        );
        // A file outside it takes the next letter, after the discs.
        let mut outside = program(Image::Host(elsewhere.join("patch.exe")));
        outside.cdroms = vec![('E', root.join("missing.iso"))];
        let config = Config {
            exe: Some(&game.join("game.exe")),
            ..test_config(&root, &prefix, &root)
        };
        assert!(
            prepare(&config, &outside, &prefix).is_err(),
            "the disc is missing"
        );
        outside.cdroms.clear();
        assert_eq!(
            start(Some(&game.join("game.exe")), &outside).0,
            r"E:\patch.exe" // privacy: allow authored guest path
        );
        // A disc's setup without a game starts on its disc.
        assert_eq!(
            start(None, &program(Image::Guest(r"G:\SETUP.EXE".into()))), // privacy: allow authored guest path
            (
                r"G:\SETUP.EXE".into(),        // privacy: allow authored guest path
                r#""G:\SETUP.EXE" /x"#.into(), // privacy: allow authored guest path
                r"G:\".into()                  // privacy: allow authored guest path
            )
        );
        // A script runs in cmd.exe from its own directory, as Windows starts
        // one; a host script's copy drops its pause.
        std::fs::write(game.join("tools/restore.bat"), "reg add x\r\npause\r\n").unwrap();
        assert_eq!(
            start(
                Some(&game.join("game.exe")),
                &program(Image::Host(game.join("tools/restore.bat")))
            ),
            (
                CMD.into(),
                r#""C:\windows\system32\cmd.exe" /d /c ""D:\tools\restore.bat" /x""#.into(), // privacy: allow authored guest path
                r"D:\tools\".into() // privacy: allow authored guest path
            )
        );
        let prepared = prepare(
            &config,
            &program(Image::Host(game.join("tools/restore.bat"))),
            &prefix,
        )
        .unwrap();
        let replaced = prefix.join(format!(
            ".hitode-files-{}/replaced/0/restore.bat",
            std::process::id()
        ));
        assert_eq!(
            std::fs::read_to_string(replaced).unwrap(),
            "reg add x\r\nrem [hitode] skipped pause\r\n"
        );
        drop(prepared);
        assert_eq!(
            start(None, &program(Image::Guest(r"G:\INSTALL.CMD".into()))).1, // privacy: allow authored guest path
            r#""C:\windows\system32\cmd.exe" /d /c ""G:\INSTALL.CMD" /x""# // privacy: allow authored guest path
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_bundled_vfs_executables_and_reuses_program_files() {
        let root = std::env::temp_dir().join(format!("adv-files-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let prefix = root.join("prefix");
        std::fs::create_dir_all(prefix.join("dosdevices")).unwrap();

        let plain = root.join("plain.exe");
        write_pe(&plain, ".text", &[]);
        assert!(!needs_subdirectory_launch(&plain));
        let prepared = prepare(
            &test_config(&root, &prefix, &plain),
            &SessionOptions::default(),
            &prefix,
        )
        .unwrap();
        assert_eq!(prepared.start, r"D:\"); // privacy: allow authored guest drive path
        assert_eq!(prepared.mounts.nt_name(&plain), None);

        let ultra = root.join("ultra.exe");
        write_pe(&ultra, ".ultra", &[]);
        assert!(needs_subdirectory_launch(&ultra));
        let prepared = prepare(
            &test_config(&root, &prefix, &ultra),
            &SessionOptions::default(),
            &prefix,
        )
        .unwrap();
        assert_eq!(prepared.start, r"C:\Program Files\");
        assert_eq!(
            prepared
                .mounts
                .nt_name(&ultra.canonicalize().unwrap())
                .as_deref(),
            Some(r"\??\C:\Program Files\ultra.exe")
        );

        let overlay = root.join("overlay.exe");
        write_pe(&overlay, ".text", b"XOJUMANJ\0\0\0\0");
        assert!(needs_subdirectory_launch(&overlay));

        let custom_install = root.join("custom-install");
        let options = SessionOptions {
            install_dir: Some(custom_install.clone()),
            ..SessionOptions::default()
        };
        let prepared = prepare(&test_config(&root, &prefix, &ultra), &options, &prefix).unwrap();
        assert_eq!(prepared.start, r"D:\"); // privacy: allow authored guest drive path
        assert_eq!(
            prepared.mounts.nt_name(&ultra.canonicalize().unwrap()),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
