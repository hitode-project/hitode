//! Adapt the existing optical VFS to Wine's neutral read-only volume contract.
//! No ISO parsing or file data is duplicated here. The family retains the VFS,
//! its media owners and its shared lazy section pages through NT mount owners.
use super::*;
use adv_nt::file::volume::{Entry, Info, Mounts, ReadFile, Volume};
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

/// Drive and mount layout prepared for a Wine session.
pub(super) struct Prepared {
    pub mounts: Mounts,
    pub directory: String,
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

pub(super) fn prepare(
    config: &Config<'_>,
    options: &SessionOptions,
    prefix: &Path,
) -> Result<Prepared, String> {
    prepare_with_replacement(config, options, prefix, None)
}

pub(super) fn prepare_with_replacement(
    config: &Config<'_>,
    options: &SessionOptions,
    prefix: &Path,
    extra_replacement: Option<(&Path, Vec<u8>)>,
) -> Result<Prepared, String> {
    let game_drive = ('D'..='Z')
        .find(|letter| {
            !options
                .cdroms
                .iter()
                .any(|(mounted, _)| mounted.to_ascii_uppercase() == *letter)
        })
        .ok_or("no drive letter remains for the program directory")?;
    let game = std::path::absolute(config.exe.parent().ok_or("the program has no directory")?)
        .map_err(|e| e.to_string())?;
    let use_install = options.install_dir.is_none() && needs_subdirectory_launch(config.exe);
    let directory = if use_install {
        r"C:\Program Files\".to_owned()
    } else {
        format!(r"{game_drive}:\")
    };
    let mut cfg = adv_os::OsConfig::new(
        config.exe.parent().ok_or("the program has no directory")?,
        config
            .exe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the program name is not Unicode")?,
        prefix,
    );
    cfg.guest_root = directory.clone();
    cfg.system_dir = config
        .drive
        .map_or_else(|| prefix.join("drive_c"), Path::to_owned);
    cfg.codepage = match config.locale {
        0x411 => 932,
        0x804 => 936,
        0x404 => 950,
        _ => 1252,
    };
    let mut vfs = adv_os::Vfs::new(&cfg);
    for (letter, image) in &options.cdroms {
        vfs.mount_disc(*letter, image)
            .map_err(|e| format!("CD-ROM {letter}: {e}"))?;
    }
    let vfs = Rc::new(vfs);
    let install = if let Some(path) = &options.install_dir {
        std::fs::create_dir_all(path)
            .and_then(|_| path.canonicalize())
            .map(Some)
            .map_err(|e| e.to_string())?
    } else if use_install {
        Some(game.canonicalize().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let configuration = crate::reallive::configuration(&game);
    let root =
        if options.cdroms.is_empty() && configuration.is_none() && extra_replacement.is_none() {
            None
        } else {
            let root =
                std::path::absolute(prefix.join(format!(".hitode-files-{}", std::process::id())))
                    .map_err(|e| e.to_string())?;
            std::fs::create_dir(&root).map_err(|e| e.to_string())?;
            Some(root.canonicalize().map_err(|e| e.to_string())?)
        };
    let mut mounts = Mounts::new(install, root.clone());
    for (letter, _) in vfs.optical_drives() {
        let path = root
            .as_ref()
            .expect("session files directory")
            .join(letter.to_string());
        mounts
            .attach(
                letter,
                path,
                Rc::new(OpticalVolume {
                    vfs: vfs.clone(),
                    letter,
                }),
            )
            .map_err(|e| e.to_string())?;
    }
    if let Some((original, text)) = configuration {
        let directory = root
            .as_ref()
            .expect("session files directory")
            .join("replaced");
        let file = directory.join(original.file_name().expect("a file name"));
        std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::write(&file, text))
            .and_then(|()| mounts.replace(&original, file))
            .map_err(|e| format!("{}: {e}", original.display()))?;
    }
    if let Some((original, bytes)) = extra_replacement {
        let directory = root
            .as_ref()
            .expect("session files directory")
            .join("replaced-extra");
        let file = directory.join(original.file_name().expect("a file name"));
        std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::write(&file, bytes))
            .and_then(|()| mounts.replace(original, file))
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
    link(
        &game,
        &devices.join(format!("{}:", game_drive.to_ascii_lowercase())),
    )?;
    let mut letters = game_drive.to_string();
    for (letter, root) in mounts.roots() {
        link(
            root,
            &devices.join(format!("{}:", letter.to_ascii_lowercase())),
        )?;
        letters.push(letter);
    }
    std::fs::write(managed, letters).map_err(|e| e.to_string())?;
    Ok(Prepared { mounts, directory })
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
            exe,
            width: 640,
            height: 480,
            jit: true,
            debug: "",
            stdout: None,
        }
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
        assert_eq!(prepared.directory, r"D:\"); // privacy: allow authored guest drive path
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
        assert_eq!(prepared.directory, r"C:\Program Files\");
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
        assert_eq!(prepared.directory, r"D:\"); // privacy: allow authored guest drive path
        assert_eq!(
            prepared.mounts.nt_name(&ultra.canonicalize().unwrap()),
            None
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
