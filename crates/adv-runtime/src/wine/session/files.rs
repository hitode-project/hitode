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

pub(super) fn prepare(
    config: &Config<'_>,
    options: &SessionOptions,
    prefix: &Path,
) -> Result<(Mounts, char), String> {
    let game_drive = ('D'..='Z')
        .find(|letter| {
            !options
                .cdroms
                .iter()
                .any(|(mounted, _)| mounted.to_ascii_uppercase() == *letter)
        })
        .ok_or("no drive letter remains for the program directory")?;
    let mut cfg = adv_os::OsConfig::new(
        config.exe.parent().ok_or("the program has no directory")?,
        config
            .exe
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the program name is not Unicode")?,
        prefix,
    );
    cfg.guest_root = format!("{game_drive}:\\");
    cfg.system_dir = config
        .drive
        .map_or_else(|| prefix.join("drive_c"), Path::to_owned);
    cfg.codepage = match config.locale {
        0x411 => 932,
        0x804 => 936,
        _ => 1252,
    };
    let mut vfs = adv_os::Vfs::new(&cfg);
    for (letter, image) in &options.cdroms {
        vfs.mount_iso(*letter, image)
            .map_err(|e| format!("CD-ROM {letter}: {e}"))?;
    }
    let vfs = Rc::new(vfs);
    let install = options
        .install_dir
        .as_ref()
        .map(|path| {
            std::fs::create_dir_all(path)
                .and_then(|_| path.canonicalize())
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let root = if options.cdroms.is_empty() {
        None
    } else {
        let root =
            std::path::absolute(prefix.join(format!(".hitode-media-{}", std::process::id())))
                .map_err(|e| e.to_string())?;
        std::fs::create_dir(&root).map_err(|e| e.to_string())?;
        Some(root.canonicalize().map_err(|e| e.to_string())?)
    };
    let mut mounts = Mounts::new(install, root.clone());
    for (letter, _) in vfs.optical_drives() {
        let path = root
            .as_ref()
            .expect("mounted media directory")
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
    let game = std::path::absolute(config.exe.parent().ok_or("the program has no directory")?)
        .map_err(|e| e.to_string())?;
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
    Ok((mounts, game_drive))
}
