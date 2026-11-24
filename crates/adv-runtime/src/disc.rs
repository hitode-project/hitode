//! What a disc offers before anything runs from it: the programs Explorer
//! would show its player, and their files, read through the same reader the
//! guest's drive uses.
use std::io::{self, Read};
use std::path::Path;

/// A disc image opened as drive `letter`.
pub struct Disc {
    vfs: adv_os::Vfs,
    letter: char,
}

impl Disc {
    pub fn open(image: &Path, letter: char) -> io::Result<Self> {
        let mut cfg = adv_os::OsConfig::new("", "", "");
        cfg.guest_root = r"C:\".into();
        let mut vfs = adv_os::Vfs::new(&cfg);
        vfs.mount_disc(letter, image)?;
        Ok(Self { vfs, letter })
    }

    /// The disc's volume label.
    pub fn label(&self) -> &str {
        self.vfs
            .optical_drive(self.letter)
            .map_or("", |drive| drive.label.as_str())
    }

    /// A file's contents; `path` is a DOS path on this drive.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let mut file = self.vfs.open_optical_file(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// The programs a player starts from the disc: AutoPlay's `open` first,
    /// then the root's other executables, as DOS paths.
    pub fn programs(&self) -> Vec<String> {
        let root = format!(r"{}:\", self.letter);
        let entries = self.vfs.list(&root).unwrap_or_default();
        let mut programs = Vec::new();
        let autorun = entries
            .iter()
            .find(|entry| !entry.is_dir && entry.name.eq_ignore_ascii_case("autorun.inf"));
        if let Some(open) = autorun
            .and_then(|entry| self.read(&format!("{root}{}", entry.name)).ok())
            .and_then(|text| autorun_open(&text))
        {
            let path = format!("{root}{}", open.trim_start_matches(['\\', '/']));
            if self
                .vfs
                .optical_entry(&path)
                .is_ok_and(|entry| !entry.is_dir)
            {
                programs.push(path);
            }
        }
        for entry in &entries {
            let exe = entry
                .name
                .rsplit_once('.')
                .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("exe"));
            let path = format!("{root}{}", entry.name);
            if !entry.is_dir
                && exe
                && !programs
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(&path))
            {
                programs.push(path);
            }
        }
        programs
    }

    /// The ANSI code page `program` on this disc runs with.
    pub fn codepage(&self, program: &str) -> io::Result<u32> {
        let image = self.read(program)?;
        let name = program.rsplit(['\\', '/']).next().unwrap_or(program);
        Ok(crate::codepage::detect(name, &image))
    }
}

/// The program in `[autorun]`'s `open=` line, without its arguments.
fn autorun_open(text: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(text);
    let mut section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.eq_ignore_ascii_case("[autorun]");
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !section || !key.trim().eq_ignore_ascii_case("open") {
            continue;
        }
        let value = value.trim();
        let program = match value.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or_default(),
            None => value.split_whitespace().next().unwrap_or_default(),
        };
        return (!program.is_empty()).then(|| program.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    // spec: WINE-SES-12
    #[test]
    fn autorun_names_the_program_without_its_arguments() {
        let open = |text: &str| super::autorun_open(text.as_bytes());
        assert_eq!(
            open("[AutoRun]\r\nopen=SETUP.EXE /auto\r\nicon=a.ico\r\n").as_deref(),
            Some("SETUP.EXE")
        );
        assert_eq!(
            open("[autorun]\nOPEN = \"Bin\\Menu Launcher.exe\" -x\n").as_deref(),
            Some(r"Bin\Menu Launcher.exe")
        );
        assert_eq!(open("[other]\nopen=x.exe\n"), None);
    }
}
