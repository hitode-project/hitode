//! LoadLibraryEx's resource-only path. The process Modules table owns every
//! independent raw view; VFS contributes only its neutral weak content index.
use super::*;
use std::fs::File;

pub(super) fn load(env: &mut Env<'_>, name: String) -> Outcome {
    let normalized = env.ext.get::<Win32>().vfs.normalize(&name);
    let existing = adv_os::modules(env.ext).by_name(&name).or_else(|| {
        adv_os::modules(env.ext)
            .iter()
            .find(|(_, module)| {
                module.executable_visible()
                    && env
                        .ext
                        .get::<Win32>()
                        .vfs
                        .normalize(&module.path)
                        .eq_ignore_ascii_case(&normalized)
            })
            .map(|(id, _)| id)
    });
    if let Some(id) = existing {
        let handle = adv_os::modules(env.ext).get(id).handle();
        return match adv_os::modules_mut(env.ext).retain(env.cpu, id) {
            Ok(()) => Outcome::Return(handle),
            Err(error) => Outcome::Fail(error.to_string()),
        };
    }
    let opened = open(env, &name);
    let (path, section, len) = match opened {
        Ok(source) => source,
        Err(error) => {
            set_last_error(env, error);
            return Outcome::Return(0);
        }
    };
    let modules = adv_os::modules_mut(env.ext);
    match modules.map_data_file(env.cpu, env.sys, &path, section, len) {
        Ok(id) => Outcome::Return(modules.get(id).handle()),
        Err(error) => {
            log::debug!("LoadLibraryEx DATAFILE({name}): {error}");
            set_last_error(env, super::load_error_code(&error));
            Outcome::Return(0)
        }
    }
}

fn open(
    env: &mut Env<'_>,
    name: &str,
) -> Result<(String, std::rc::Rc<adv_os::vfs::FileSection>, u32), u32> {
    let with_extension = if adv_os::module_key(name) != name.to_ascii_lowercase()
        && !name
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(name)
            .contains('.')
    {
        format!("{name}.dll")
    } else {
        name.into()
    };
    let path = env.ext.get::<Win32>().vfs.normalize(&with_extension);
    let virtual_file = env
        .ext
        .get_or_default::<adv_os::system_files::SystemFiles>()
        .open(env.cpu, env.sys, &path)
        .map_err(|_| err::ACCESS_DENIED)?;
    if let Some(bytes) = virtual_file {
        let length = u32::try_from(bytes.len()).map_err(|_| err::NOT_ENOUGH_MEMORY)?;
        let identity = (u64::MAX, bytes.as_ptr() as usize as u64);
        let section = env.ext.get::<Win32>().vfs.sections.memory(bytes);
        compatible_open(env, &section, identity)?;
        return Ok((path, section, length));
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    let (path, host) = vfs
        .lookup(&path)
        .map(|host| (path.clone(), host))
        .or_else(|| {
            (!with_extension.contains(['\\', '/']))
                .then(|| {
                    let bundled = vfs.normalize(&format!("dll\\{with_extension}"));
                    vfs.lookup(&bundled).map(|host| (bundled, host))
                })
                .flatten()
        })
        .ok_or(err::FILE_NOT_FOUND)?;
    let source = File::open(host).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => err::FILE_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => err::ACCESS_DENIED,
        _ => 1117,
    })?;
    let length = source.metadata().map_err(|_| 1117u32)?.len();
    let length = u32::try_from(length).map_err(|_| err::NOT_ENOUGH_MEMORY)?;
    let identity = adv_os::vfs::file_identity(&source).map_err(|_| 1117u32)?;
    let section = vfs
        .sections
        .transfer(identity, source, u64::from(length), false)
        .map_err(|_| 1117u32)?;
    compatible_open(env, &section, identity)?;
    Ok((path, section, length))
}

fn compatible_open(
    env: &Env<'_>,
    section: &std::rc::Rc<adv_os::vfs::FileSection>,
    identity: adv_os::vfs::FileIdentity,
) -> Result<(), u32> {
    let files = &env.ext.get::<Win32>().vfs.sections;
    for (_, file) in env.sys.objects.all_hosts::<crate::FileObj>() {
        let same_file = file.file.cached_identity().is_some_and(|cached| {
            cached == identity
                || files
                    .lookup(cached)
                    .is_some_and(|other| std::rc::Rc::ptr_eq(&other, section))
        });
        if same_file && (file.share & 1 == 0 || file.access & (0x40000000 | 0x10000000) != 0) {
            return Err(32); // Transient DATAFILE open, not a retained mapped FD lease.
        }
    }
    Ok(())
}
