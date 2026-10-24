//! Wine loads the same installed files the portable text backend discovers.
//! Link them into Windows' Fonts directory: Wine's custom Path loader appends
//! names past its allocated directory string. Standard discovery also handles
//! nested host directories without a fixed path-list buffer.
use super::*;
use adv_device::text::{FontReplacements, Text};

pub(super) struct Fonts {
    enabled: bool,
    replacements: Vec<(Vec<String>, Vec<String>)>,
}

impl Fonts {
    pub fn prepare(config: &Config<'_>, drive: &Path) -> Result<Self, String> {
        if config.fonts.is_empty() {
            return Ok(Self {
                enabled: false,
                replacements: Vec::new(),
            });
        }
        let directories: Vec<_> = config
            .fonts
            .split(';')
            .filter_map(|path| path.strip_prefix(r"\??\unix"))
            .map(|path| PathBuf::from(path.replace('\\', "/")))
            .collect();
        #[cfg(not(target_os = "ios"))]
        let text = if config.fonts == HOST_FONTS {
            Text::default()
        } else {
            Text::with_font_directories(&directories)
        };
        #[cfg(target_os = "ios")]
        let text = Text::with_font_directories(&directories);
        let FontReplacements { files, families } =
            text.replacement_fonts().map_err(|e| e.to_string())?;
        let directory = drive.join("windows/fonts");
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("hitode-host-")
            {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
        }
        for (index, file) in files.iter().enumerate() {
            let extension = file.extension().unwrap_or_default().to_string_lossy();
            link(
                file,
                &directory.join(format!("hitode-host-{index:04}.{extension}")),
            )?;
        }
        Ok(Self {
            enabled: true,
            replacements: families,
        })
    }

    /// Install before wineboot first enters user32 or win32u. Children see the
    /// same registry and installed files, including on a reused prefix.
    pub fn install(&self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        machine
            .visit_context(None, |env| {
                let user = adv_nt::environment::user_key(env)?;
                adv_nt::registry::set_volatile_string(
                    env,
                    &format!(r"{user}\Software\Wine\Fonts"),
                    "Path",
                    "",
                )?;
                let key = format!(r"{user}\Software\Wine\Fonts\Replacements");
                for (aliases, targets) in &self.replacements {
                    for alias in aliases {
                        adv_nt::registry::set_volatile_multi_string(env, &key, alias, targets)?;
                    }
                }
                Ok::<_, adv_nt::status::Error>(())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("font replacements: {e:?}"))
    }
}
