//! National language support data, after Wine's `unix/env.c`: the locale
//! and code page tables are files in the prefix, shared through named
//! sections and mapped read-only.

use adv_machine::{Env, Outcome};

use crate::file;
use crate::object::{Attributes, write_u32};
use crate::section;
use crate::server;
use crate::status::{self, Error, Status};

/// The LCID of the process's system and user locales.
pub struct Locale(pub u32);

const SYSTEM_DIRECTORY: &str = r"\??\C:\windows\system32\";
const SORTING_DIRECTORY: &str = r"\??\C:\windows\globalization\sorting\";

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 1;
const FILE_SYNCHRONOUS_IO_ALERT: u32 = 0x10;
const SECTION_MAP_READ: u32 = 0x0004;
const SEC_COMMIT: u32 = 0x800_0000;
const OBJ_PERMANENT: u32 = 0x10;
const OBJ_OPENIF: u32 = 0x80;

const NLS_SECTION_SORTKEYS: u32 = 9;
const NLS_SECTION_CASEMAP: u32 = 10;
const NLS_SECTION_CODEPAGE: u32 = 11;
const NLS_SECTION_NORMALIZE: u32 = 12;

const UNSUCCESSFUL: Status = 0xc000_0001;
const INVALID_PARAMETER_1: Status = 0xc000_00ef;

/// NtInitializeNlsFiles(*base, *lcid, *size)
pub fn initialize(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[base, lcid, _] = args else {
        unreachable!()
    };
    status::finish((|| {
        let file = open_data_file(env, &format!("{SYSTEM_DIRECTORY}locale.nls"))?;
        let mapping = create_section(env, None, file);
        server::close(env, file)?;
        let (mapping, _) = mapping?;
        let view = section::map_read_only(env, mapping);
        server::close(env, mapping)?;
        write_u32(env, base, view?.0)?;
        let locale = env
            .ext
            .try_get::<DefaultLocales>()
            .map_or(env.ext.get::<Locale>().0, |locales| locales.0[0]);
        write_u32(env, lcid, locale)?;
        Ok(())
    })())
}

/// NtQueryDefaultLocale(user, *lcid)
pub fn default_locale(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let locale = env
        .ext
        .try_get::<DefaultLocales>()
        .map_or(env.ext.get::<Locale>().0, |locales| {
            locales.0[usize::from(args[0] as u8 != 0)]
        });
    status::finish(write_u32(env, args[1], locale).map_err(Error::from))
}

/// NtGetNlsSectionPtr(type, id, unknown, *base, *size)
pub fn section(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[kind, id, _, base, size] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut attributes = Attributes {
            security: Vec::new(),
            root: 0,
            attributes: 0,
            name: Some(section_name(kind, id)?.encode_utf16().collect()),
        };
        let mapping = match section::open_mapping(env, SECTION_MAP_READ, &attributes) {
            Ok(mapping) => mapping,
            Err(Error::Status(_)) => {
                let name = file_name(kind, id).ok_or(status::OBJECT_NAME_NOT_FOUND)?;
                let directory = if kind == NLS_SECTION_SORTKEYS {
                    SORTING_DIRECTORY
                } else {
                    SYSTEM_DIRECTORY
                };
                let file = open_data_file(env, &format!("{directory}{name}.nls"))?;
                attributes.attributes = OBJ_OPENIF | OBJ_PERMANENT;
                let mapping = create_section(env, Some(&attributes), file);
                server::close(env, file)?;
                mapping?.0
            }
            Err(error) => return Err(error),
        };
        let view = section::map_read_only(env, mapping);
        server::close(env, mapping)?;
        let (view_base, view_size) = view?;
        write_u32(env, base, view_base)?;
        write_u32(env, size, view_size)?;
        Ok(())
    })())
}

fn open_data_file(env: &mut Env<'_>, name: &str) -> Result<u32, Error> {
    file::open_path(
        env,
        name.encode_utf16().collect(),
        GENERIC_READ,
        FILE_SHARE_READ,
        FILE_SYNCHRONOUS_IO_ALERT,
    )
}

fn create_section(
    env: &mut Env<'_>,
    attributes: Option<&Attributes>,
    file: u32,
) -> Result<(u32, Status), Error> {
    section::create_mapping(
        env,
        SECTION_MAP_READ,
        attributes,
        0,
        crate::memory::PAGE_READONLY,
        SEC_COMMIT,
        file,
    )
}

fn section_name(kind: u32, id: u32) -> Result<String, Status> {
    Ok(match kind {
        NLS_SECTION_SORTKEYS if id != 0 => return Err(INVALID_PARAMETER_1),
        NLS_SECTION_SORTKEYS => r"\NLS\NlsSectionSORTDEFAULT".into(),
        NLS_SECTION_CASEMAP if id != 0 => return Err(UNSUCCESSFUL),
        NLS_SECTION_CASEMAP => r"\NLS\NlsSectionLANG_INTL".into(),
        NLS_SECTION_CODEPAGE => format!(r"\NLS\NlsSectionCP{id:03}"),
        NLS_SECTION_NORMALIZE => format!(r"\NLS\NlsSectionNORM{id:08x}"),
        _ => return Err(INVALID_PARAMETER_1),
    })
}

fn file_name(kind: u32, id: u32) -> Option<String> {
    Some(match (kind, id) {
        (NLS_SECTION_SORTKEYS, _) => "sortdefault".into(),
        (NLS_SECTION_CASEMAP, _) => "l_intl".into(),
        (NLS_SECTION_CODEPAGE, id) => format!("c_{id:03}"),
        (NLS_SECTION_NORMALIZE, 1) => "normnfc".into(),
        (NLS_SECTION_NORMALIZE, 2) => "normnfd".into(),
        (NLS_SECTION_NORMALIZE, 5) => "normnfkc".into(),
        (NLS_SECTION_NORMALIZE, 6) => "normnfkd".into(),
        (NLS_SECTION_NORMALIZE, 13) => "normidna".into(),
        _ => return None,
    })
}

/// NtQueryInstallUILanguage(*language)
pub fn install_ui_language(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let language = env.ext.get::<Locale>().0 as u16;
    status::finish(
        crate::object::write_bytes(env, args[0], &language.to_le_bytes()).map_err(Error::from),
    )
}

struct UiLanguage(u16);

/// NtQueryDefaultUILanguage(*language)
pub fn default_ui_language(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let language = env
        .ext
        .try_get::<UiLanguage>()
        .map_or(env.ext.get::<Locale>().0 as u16, |language| language.0);
    status::finish(
        crate::object::write_bytes(env, args[0], &language.to_le_bytes()).map_err(Error::from),
    )
}

/// NtSetDefaultUILanguage(language)
pub fn set_ui_language(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    env.ext.insert(UiLanguage(args[0] as u16));
    Outcome::Return(status::SUCCESS)
}

struct DefaultLocales([u32; 2]);

/// NtSetDefaultLocale(user, lcid): only the guest's locale changes.
pub fn set_default_locale(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if env.ext.try_get::<DefaultLocales>().is_none() {
        let initial = env.ext.get::<Locale>().0;
        env.ext.insert(DefaultLocales([initial; 2]));
    }
    env.ext.get_mut::<DefaultLocales>().0[usize::from(args[0] as u8 != 0)] = args[1];
    Outcome::Return(status::SUCCESS)
}
