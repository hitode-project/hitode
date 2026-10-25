//! Wine loads the same installed files the portable text backend discovers.
//! Link them into Windows' Fonts directory: Wine's custom Path loader appends
//! names past its allocated directory string. Standard discovery also handles
//! nested host directories without a fixed path-list buffer.
use super::*;
use adv_device::text::{FontReplacements, Text};

pub(super) struct Fonts {
    enabled: bool,
    replacements: Vec<(Vec<String>, Vec<String>)>,
    overrides: crate::FontOverrides,
    saved: Vec<SavedSetting>,
}

struct SavedSetting {
    key: String,
    name: String,
    value: Option<(u32, Vec<u8>)>,
}

impl Fonts {
    pub fn prepare(
        config: &Config<'_>,
        drive: &Path,
        options: &SessionOptions,
    ) -> Result<Self, String> {
        if config.fonts.is_empty()
            && options.font_files.is_empty()
            && options.fonts.families.is_empty()
            && options.fonts.antialiased.is_none()
        {
            return Ok(Self {
                enabled: false,
                replacements: Vec::new(),
                overrides: options.fonts.clone(),
                saved: Vec::new(),
            });
        }
        let directories: Vec<_> = config
            .fonts
            .split(';')
            .filter_map(|path| path.strip_prefix(r"\??\unix"))
            .map(|path| PathBuf::from(path.replace('\\', "/")))
            .collect();
        #[cfg(not(target_os = "ios"))]
        let mut text = if config.fonts == HOST_FONTS {
            Text::default()
        } else {
            Text::with_font_directories(&directories)
        };
        #[cfg(target_os = "ios")]
        let mut text = Text::with_font_directories(&directories);
        for bytes in crate::font::read_resources(&options.font_files)? {
            text.add_resource(&bytes).map_err(|e| e.to_string())?;
        }
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
        let files: Vec<_> = files.iter().chain(&options.font_files).collect();
        for (index, file) in files.iter().enumerate() {
            let extension = file.extension().unwrap_or_default().to_string_lossy();
            link(
                &std::path::absolute(file).map_err(|e| e.to_string())?,
                &directory.join(format!("hitode-host-{index:04}.{extension}")),
            )?;
        }
        Ok(Self {
            enabled: true,
            replacements: families,
            overrides: options.fonts.clone(),
            saved: Vec::new(),
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
    pub fn apply(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        if self.overrides.families.is_empty() && self.overrides.antialiased.is_none() {
            return Ok(());
        }
        machine
            .visit_context(None, |env| {
                let user = adv_nt::environment::user_key(env)?;
                let key = concat!(
                    r"\Registry\Machine\Software\Microsoft\Windows NT",
                    r"\CurrentVersion\FontSubstitutes"
                );
                for (alias, target) in &self.overrides.families {
                    self.saved.push(SavedSetting {
                        key: key.into(),
                        name: alias.clone(),
                        value: adv_nt::registry::platform_value(env, key, alias)?,
                    });
                    adv_nt::registry::set_volatile_string(env, key, alias, target)?;
                }
                if let Some(smooth) = self.overrides.antialiased {
                    let key = format!(r"{user}\Control Panel\Desktop");
                    for name in ["FontSmoothing", "FontSmoothingType"] {
                        self.saved.push(SavedSetting {
                            key: key.clone(),
                            name: name.into(),
                            value: adv_nt::registry::platform_value(env, &key, name)?,
                        });
                    }
                    adv_nt::registry::set_volatile_string(
                        env,
                        &key,
                        "FontSmoothing",
                        if smooth { "2" } else { "0" },
                    )?;
                    adv_nt::registry::set_volatile_value(
                        env,
                        &key,
                        "FontSmoothingType",
                        4,
                        &1u32.to_le_bytes(),
                    )?;
                }
                Ok::<_, adv_nt::status::Error>(())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("font choices: {e:?}"))
    }

    pub fn restore(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        if self.saved.is_empty() {
            return Ok(());
        }
        machine
            .visit_context(None, |env| {
                for SavedSetting { key, name, value } in self.saved.drain(..).rev() {
                    match value {
                        Some((kind, bytes)) => adv_nt::registry::set_volatile_value(
                            env,
                            &key,
                            &name,
                            kind as i32,
                            &bytes,
                        )?,
                        None => adv_nt::registry::delete_platform_value(env, &key, &name)?,
                    }
                }
                Ok::<_, adv_nt::status::Error>(())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("restore font choices: {e:?}"))
    }
}
