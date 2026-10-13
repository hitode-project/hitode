//! Recursive directory utilities over the existing observable NT file boundary.
use crate::{Win32, kernel32::file, util::set_last_error};
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, MemFault, Prot};
use adv_machine::{Env, Outcome};

type Done = file::native::operations::Done<()>;

fn prefixes(env: &Env<'_>, path: &str) -> Vec<String> {
    let full = env.ext.get::<Win32>().vfs.normalize(path);
    let start = if full.starts_with("\\\\") {
        // The UNC server and share are roots, never directories to create.
        full.char_indices()
            .filter(|(_, c)| *c == '\\')
            .nth(3)
            .map_or(full.len(), |(at, _)| at + 1)
    } else {
        3
    };
    let mut result: Vec<_> = full
        .char_indices()
        .filter_map(|(at, c)| (c == '\\' && at > start).then(|| full[..at].to_owned()))
        .collect();
    let final_path = full.trim_end_matches('\\');
    if final_path.len() > start && result.last().is_none_or(|p| p != final_path) {
        result.push(final_path.to_owned());
    }
    result
}

fn recursive(env: &mut Env<'_>, paths: Vec<String>, index: usize, then: Done) -> Outcome {
    let Some(path) = paths.get(index).cloned() else {
        return then(env, Ok(()));
    };
    let queried = path.clone();
    file::file_attributes_path_then(
        env,
        &queried,
        "Shell recursive directory",
        Box::new(move |env, attributes| match attributes {
            Ok(bits) if bits & 0x10 != 0 => recursive(env, paths, index + 1, then),
            Ok(_) => then(env, Err(if index + 1 < paths.len() { 3 } else { 183 })),
            Err(2 | 3) => file::native::directory::create(
                env,
                path,
                Box::new(move |env, created| match created {
                    Ok(()) => recursive(env, paths, index + 1, then),
                    Err(status) => then(env, Err(file::native::directory::error(status))),
                }),
            ),
            Err(code) => then(env, Err(code)),
        }),
    )
}

fn ensure(env: &mut Env<'_>, text: String) -> Outcome {
    if text.is_empty() {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    if env.ext.get::<Win32>().codec.encode(&text).len() >= 260 {
        set_last_error(env, 206);
        return Outcome::Return(0);
    }
    // The final component is a filename unless followed by a backslash.
    // Decode ANSI first so a DBCS trail byte cannot become a path separator.
    let Some(end) = text.rfind('\\') else {
        return Outcome::Return(1);
    };
    let paths = prefixes(env, &text[..=end]);
    recursive(
        env,
        paths,
        0,
        Box::new(|env, result| {
            if let Err(error) = result {
                set_last_error(env, error);
                Outcome::Return(0)
            } else {
                Outcome::Return(1)
            }
        }),
    )
}

#[win32(imagehlp, name="MakeSureDirectoryPathExists", level=implement)]
fn image_ensure(env: &mut Env<'_>, text: AnsiStr) -> Result<Outcome, MemFault> {
    let bytes = text.read(env.cpu, 32768)?;
    let text = env.ext.get::<Win32>().codec.decode(&bytes);
    Ok(ensure(env, text))
}
#[win32(dbghelp, name="MakeSureDirectoryPathExists", level=implement)]
fn debug_ensure(env: &mut Env<'_>, text: AnsiStr) -> Result<Outcome, MemFault> {
    image_ensure(env, text)
}

fn create(env: &mut Env<'_>, path: String, security: u32) -> Outcome {
    let absolute = path.starts_with("\\\\")
        || (path.as_bytes().get(1) == Some(&b':')
            && path
                .as_bytes()
                .get(2)
                .is_some_and(|c| matches!(c, b'\\' | b'/')));
    let fail = |env: &mut Env<'_>, code| {
        set_last_error(env, code);
        Outcome::Return(code)
    };
    if !absolute {
        return fail(env, 161);
    }
    if path.encode_utf16().count() >= 260 {
        return fail(env, 206);
    }
    if security != 0 {
        if check_range(env.cpu, security, 12, Prot::READ).is_err() {
            return fail(env, 998);
        }
        if env.cpu.read_u32(security).ok() != Some(12) {
            return fail(env, 87);
        }
        if env.cpu.read_u32(security + 4).ok() != Some(0) {
            adv_abi::warn_unavailable(
                env,
                "SHCreateDirectoryEx",
                "security descriptors are outside the guest security profile",
            );
            return fail(env, 50);
        }
    }
    let paths = prefixes(env, &path);
    // Preserve the existing-directory result before attempting recursive parents.
    file::native::directory::create(
        env,
        path,
        Box::new(move |env, result| match result {
            Ok(()) => {
                set_last_error(env, 0);
                Outcome::Return(0)
            }
            Err(status) if file::native::directory::error(status) == 3 => recursive(
                env,
                paths,
                0,
                Box::new(|env, result| {
                    let code = result.err().unwrap_or(0);
                    set_last_error(env, code);
                    Outcome::Return(code)
                }),
            ),
            Err(status) => {
                let code = file::native::directory::error(status);
                set_last_error(env, code);
                Outcome::Return(code)
            }
        }),
    )
}
#[win32(shell32,name="SHCreateDirectoryExA",level=implement)]
fn create_a(
    env: &mut Env<'_>,
    _owner: Handle,
    path: AnsiStr,
    security: GuestPtr<()>,
) -> Result<Outcome, MemFault> {
    let bytes = path.read(env.cpu, 32768)?;
    let text = env.ext.get::<Win32>().codec.decode(&bytes);
    Ok(create(env, text, security.addr()))
}
#[win32(shell32,name="SHCreateDirectoryExW",level=implement)]
fn create_w(
    env: &mut Env<'_>,
    _owner: Handle,
    path: WideStr,
    security: GuestPtr<()>,
) -> Result<Outcome, MemFault> {
    let text = String::from_utf16_lossy(&path.read(env.cpu, 32768)?);
    Ok(create(env, text, security.addr()))
}
#[win32(shell32,name="SHCreateDirectory",level=implement)]
fn create_plain(env: &mut Env<'_>, _owner: Handle, path: WideStr) -> Result<Outcome, MemFault> {
    let text = String::from_utf16_lossy(&path.read(env.cpu, 32768)?);
    Ok(create(env, text, 0))
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        image_ensure_ABI,
        debug_ensure_ABI,
        create_a_ABI,
        create_w_ABI,
        create_plain_ABI,
    ] {
        reg.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture, env};
    use adv_cpu::backend::Guest;
    use adv_machine::Resume;
    #[test]
    fn recursive_directory_fronts_resume_through_a_modified_native_create_entry() {
        let mut f = Fixture::new("recursive-directories-hook");
        Win32::prepare_process_infrastructure(&mut f.m).unwrap();
        let target = crate::native::entry(&mut env(&mut f.m), "NtCreateFile").unwrap();
        // Authored return STATUS_ACCESS_DENIED; no native implementation bytes.
        f.m.cpu
            .write(target, &[0xb8, 0x22, 0, 0, 0xc0, 0xc2, 44, 0])
            .unwrap();
        let text = "C:\\Game\\blocked\\";
        let bytes: Vec<_> = text.bytes().chain([0]).collect();
        f.m.cpu.write(DATA, &bytes).unwrap();
        for (module, api, args, expected) in [
            ("imagehlp.dll", "MakeSureDirectoryPathExists", vec![DATA], 0),
            ("shell32.dll", "SHCreateDirectoryExA", vec![0, DATA, 0], 5),
        ] {
            let Outcome::Call {
                target: actual,
                args,
                then,
                ..
            } = f.invoke(module, api, &args)
            else {
                panic!("modified native entry was bypassed");
            };
            assert_eq!(actual, target);
            assert_eq!(args.len(), 11);
            assert!(
                matches!(then(&mut env(&mut f.m), Resume::Returned(0xc0000022)), Outcome::Return(value) if value == expected)
            );
            assert!(!f.m.ext.get::<Win32>().vfs.is_dir("C:\\Game\\blocked"));
        }
        f.cleanup();
    }
    #[test]
    fn recursive_utilities_distinguish_files_trailing_separators_and_absolute_shell_paths() {
        let mut f = Fixture::new("recursive-directories");
        let ansi = |f: &mut Fixture, value: &str| {
            let mut bytes = f.m.ext.get::<Win32>().codec.encode(value);
            bytes.push(0);
            f.m.cpu.write(DATA, &bytes).unwrap();
        };
        for module in ["imagehlp.dll", "dbghelp.dll"] {
            ansi(&mut f, "parent\\child\\file.dat");
            assert_eq!(f.call(module, "MakeSureDirectoryPathExists", &[DATA]), 1);
            let vfs = &f.m.ext.get::<Win32>().vfs;
            assert!(vfs.is_dir("parent\\child"));
            assert!(vfs.lookup("parent\\child\\file.dat").is_none());
            ansi(&mut f, "日本\\表\\");
            assert_eq!(f.call(module, "MakeSureDirectoryPathExists", &[DATA]), 1);
            assert!(f.m.ext.get::<Win32>().vfs.is_dir("日本\\表"));
            ansi(&mut f, "");
            assert_eq!(f.call(module, "MakeSureDirectoryPathExists", &[DATA]), 0);
        }
        std::fs::write(f.root.join("game/owned-file"), b"fixture").unwrap();
        ansi(&mut f, "owned-file\\child\\");
        assert_eq!(
            f.call("imagehlp.dll", "MakeSureDirectoryPathExists", &[DATA]),
            0
        );
        for (api, wide) in [
            ("SHCreateDirectoryExA", false),
            ("SHCreateDirectoryExW", true),
            ("SHCreateDirectory", true),
        ] {
            let value = format!("C:\\Game\\{api}\\nested");
            if wide {
                f.m.cpu
                    .write(
                        DATA,
                        &value
                            .encode_utf16()
                            .chain([0])
                            .flat_map(u16::to_le_bytes)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
            } else {
                ansi(&mut f, &value);
            }
            let args = if api == "SHCreateDirectory" {
                &[0, DATA][..]
            } else {
                &[0, DATA, 0][..]
            };
            assert_eq!(f.call("shell32.dll", api, args), 0);
            assert!(f.m.ext.get::<Win32>().vfs.is_dir(&value));
            assert_eq!(f.call("shell32.dll", api, args), 183);
        }
        ansi(&mut f, "relative\\dir");
        assert_eq!(
            f.call("shell32.dll", "SHCreateDirectoryExA", &[0, DATA, 0]),
            161
        );
        assert!(f.m.ext.get::<Win32>().vfs.lookup("relative").is_none());
        f.cleanup();
    }
}
