//! Wine loads the same installed files the portable text backend discovers.
//! Link them into Windows' Fonts directory: Wine's custom Path loader appends
//! names past its allocated directory string. Standard discovery also handles
//! nested host directories without a fixed path-list buffer.
//! Explicit family choices adapt a private LOGFONT copy at the win32k boundary,
//! taking precedence over installed families without changing caller buffers.
use super::*;
use adv_device::text::{FontReplacements, Text};

struct Names(BTreeMap<String, String>);

// Wine 11's NtGdiHfontCreate accepts LOGFONTW, ENUMLOGFONTEXW and
// ENUMLOGFONTEXDVW. Each starts with the same LOGFONTW face-name field.
fn create_font(
    env: &mut adv_machine::Env<'_>,
    number: u32,
    function: u32,
    args: &[u32],
) -> Option<adv_machine::Outcome> {
    if number != 0x1233 || args.len() != 5 || ![92, 348, 420].contains(&args[1]) {
        return None;
    }
    let mut bytes = vec![0; args[1] as usize];
    env.cpu.read(args[0], &mut bytes).ok()?;
    let name: Vec<_> = bytes[28..92]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    let name = String::from_utf16(&name).ok()?;
    let vertical = name.starts_with('@');
    let target = env
        .ext
        .get::<Names>()
        .0
        .get(&name.strip_prefix('@').unwrap_or(&name).to_lowercase())?;
    let target = if vertical && !target.starts_with('@') {
        format!("@{target}")
    } else {
        target.clone()
    };
    bytes[28..92].fill(0);
    for (output, unit) in bytes[28..90]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(target.encode_utf16())
    {
        output.copy_from_slice(&unit.to_le_bytes());
    }
    let allocator = super::super::allocator();
    let Some(copy) = (allocator.allocate)(env, args[1]) else {
        return Some(adv_machine::Outcome::Return(0));
    };
    if let Err(error) = env.cpu.write(copy, &bytes) {
        (allocator.free)(env, copy);
        return Some(adv_machine::Outcome::Fail(error.to_string()));
    }
    let mut args = args.to_vec();
    args[0] = copy;
    Some(adv_machine::Outcome::call_aligned(
        env,
        function,
        args,
        move |env, returned| {
            (allocator.free)(env, copy);
            adv_machine::Outcome::Return(returned.value())
        },
    ))
}

pub(super) struct Fonts {
    enabled: bool,
    catalog: Rc<FontReplacements>,
    overrides: crate::FontOverrides,
    saved: Vec<SavedSetting>,
}

/// System discovery is immutable for the host's lifetime. Explicit resources
/// and custom directories are discovered for each session.
#[derive(Default)]
pub(super) struct Catalog {
    system: Option<Rc<FontReplacements>>,
}

pub(super) fn clear_links(drive: &Path) -> Result<(), String> {
    sync_links(drive, &[])
}

fn sync_links(drive: &Path, files: &[PathBuf]) -> Result<(), String> {
    let directory = drive.join("windows/fonts");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut desired = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        let extension = file.extension().unwrap_or_default().to_string_lossy();
        desired.insert(format!("hitode-host-{index:04}.{extension}"), file);
    }
    for entry in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("hitode-host-") && !desired.contains_key(name.as_ref()) {
            std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    for (name, file) in desired {
        link(
            &std::path::absolute(file).map_err(|e| e.to_string())?,
            &directory.join(name),
        )?;
    }
    Ok(())
}

struct SavedSetting {
    key: String,
    name: String,
    value: Option<(u32, Vec<u8>)>,
}

impl Catalog {
    pub fn prepare(
        &mut self,
        config: &Config<'_>,
        drive: &Path,
        options: &SessionOptions,
    ) -> Result<Fonts, String> {
        if config.fonts.is_empty()
            && options.font_files.is_empty()
            && options.fonts.families.is_empty()
            && options.fonts.antialiased.is_none()
        {
            sync_links(drive, &[])?;
            return Ok(Fonts {
                enabled: false,
                catalog: Rc::new(FontReplacements {
                    files: Vec::new(),
                    families: Vec::new(),
                }),
                overrides: options.fonts.clone(),
                saved: Vec::new(),
            });
        }
        let system = config.fonts == HOST_FONTS && options.font_files.is_empty();
        let catalog = if system && let Some(catalog) = &self.system {
            catalog.clone()
        } else {
            let catalog = Rc::new(discover(config, options)?);
            if system {
                self.system = Some(catalog.clone());
            }
            catalog
        };
        let files: Vec<_> = catalog
            .files
            .iter()
            .chain(&options.font_files)
            .cloned()
            .collect();
        sync_links(drive, &files)?;
        Ok(Fonts {
            enabled: true,
            catalog,
            overrides: options.fonts.clone(),
            saved: Vec::new(),
        })
    }
}

fn discover(config: &Config<'_>, options: &SessionOptions) -> Result<FontReplacements, String> {
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
    text.replacement_fonts().map_err(|e| e.to_string())
}

impl Fonts {
    pub fn configure(&self, machine: &mut Machine<Cpu>) {
        if !self.overrides.families.is_empty() {
            machine.ext.insert(Names(
                self.overrides
                    .families
                    .iter()
                    .map(|(name, target)| (name.to_lowercase(), target.clone()))
                    .collect(),
            ));
            machine.ext.insert(adv_nt::win32k::CallFilter(create_font));
        }
    }

    /// Install before the game first enters user32 or win32u. Children see the
    /// same registry and installed files, including on a reused prefix.
    pub fn install(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        machine
            .visit_context(None, |env| {
                let user = adv_nt::environment::user_key(env)?;
                let key = format!(r"{user}\Software\Wine\Fonts");
                self.saved.push(SavedSetting {
                    value: adv_nt::registry::platform_value(env, &key, "Path")?,
                    key: key.clone(),
                    name: "Path".into(),
                });
                adv_nt::registry::set_volatile_string(env, &key, "Path", "")?;
                let key = format!(r"{user}\Software\Wine\Fonts\Replacements");
                for (aliases, targets) in &self.catalog.families {
                    for alias in aliases {
                        self.saved.push(SavedSetting {
                            value: adv_nt::registry::platform_value(env, &key, alias)?,
                            key: key.clone(),
                            name: alias.clone(),
                        });
                        adv_nt::registry::set_volatile_multi_string(env, &key, alias, targets)?;
                    }
                }
                Ok::<_, adv_nt::status::Error>(())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("font replacements: {e:?}"))
    }
    pub fn apply(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        self.configure(machine);
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
