//! Host font configuration is shared by native text queries and drawing.
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
    directories: Option<Vec<PathBuf>>,
    windows: PathBuf,
    resources: Vec<Vec<u8>>,
    overrides: crate::FontOverrides,
    locale: u32,
    saved: Vec<SavedSetting>,
    catalog: Rc<FontReplacements>,
}

/// Discovery is retained by the device's process-wide directory catalog.
#[derive(Default)]
pub(super) struct Catalog {
    fonts: BTreeMap<(String, PathBuf), Rc<FontReplacements>>,
}
struct SavedSetting {
    key: String,
    name: String,
    value: Option<(u32, Vec<u8>)>,
}
fn text(directories: Option<&[PathBuf]>, windows: &Path) -> Text {
    match directories {
        Some(directories) => Text::with_font_directories(directories),
        None => Text::default(),
    }
    .with_windows_fonts(windows)
}

impl Catalog {
    pub fn prepare(
        &mut self,
        config: &Config<'_>,
        _drive: &Path,
        options: &SessionOptions,
    ) -> Result<Fonts, String> {
        let directories: Option<Vec<PathBuf>> = (config.fonts != HOST_FONTS).then(|| {
            config
                .fonts
                .split(';')
                .filter_map(|path| path.strip_prefix(r"\??\unix"))
                .map(|path| PathBuf::from(path.replace('\\', "/")))
                .collect()
        });
        // Wine's own fonts stand in for the Windows ones the host lacks, as
        // its font engine did before the native bodies replaced it.
        let windows = config.dist.join("share/wine/fonts");
        let key = (config.fonts.to_owned(), windows.clone());
        let catalog = if let Some(catalog) = self.fonts.get(&key) {
            catalog.clone()
        } else {
            let text = text(directories.as_deref(), &windows);
            let catalog = Rc::new(text.replacement_fonts().map_err(|e| e.to_string())?);
            self.fonts.insert(key, catalog.clone());
            catalog
        };
        Ok(Fonts {
            directories,
            windows,
            resources: crate::font::read_resources(&options.font_files)?,
            overrides: options.fonts.clone(),
            locale: config.locale,
            saved: Vec::new(),
            catalog,
        })
    }
}
impl Fonts {
    pub fn configure(&self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        let mut text = text(self.directories.as_deref(), &self.windows);
        for bytes in &self.resources {
            text.add_file_resource(bytes, true)
                .map_err(|e| e.to_string())?;
        }
        let mut overrides = self.overrides.clone();
        for (aliases, targets) in &self.catalog.families {
            if let Some(target) = targets.first() {
                for alias in aliases {
                    overrides
                        .families
                        .entry(alias.clone())
                        .or_insert_with(|| target.clone());
                }
            }
        }
        text.set_overrides(overrides);
        super::super::fonts::install(machine, text, self.locale);
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
        Ok(())
    }
    pub fn install(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        self.configure(machine)
    }
    pub fn apply(&mut self, machine: &mut Machine<Cpu>) -> Result<(), String> {
        if self.overrides.families.is_empty() && self.overrides.antialiased.is_none() {
            return Ok(());
        }
        machine
            .visit_context(None, |env| {
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
                    let user = adv_nt::environment::user_key(env)?;
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
