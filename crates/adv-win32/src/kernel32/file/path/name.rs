//! Existing VFS names without inventing filesystem short-name aliases.
use super::*;

fn unavailable(env: &mut Env<'_>, api: &str, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, api, reason);
    set_last_error(env, 50); // ERROR_NOT_SUPPORTED
    0
}

fn input(env: &mut Env<'_>, pointer: u32, wide: bool, api: &str) -> Result<String, u32> {
    if pointer == 0 {
        return Err(err::INVALID_PARAMETER);
    }
    if wide {
        let units = WideStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        String::from_utf16(&units).map_err(|_| 1113)
    } else {
        if !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
            unavailable(
                env,
                api,
                "ANSI path names require guest ACP 932/936/1252/65001",
            );
            return Err(50);
        }
        let bytes = AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| 998u32)?;
        Ok(env.ext.get::<Win32>().codec.decode(&bytes))
    }
}

fn existing(vfs: &adv_os::Vfs, path: &str) -> bool {
    vfs.lookup(path).is_some() || vfs.is_optical_path(path) && vfs.optical_entry(path).is_ok()
}

fn short_name_candidate(component: &str) -> bool {
    let mut parts = component.split('.');
    let base = parts.next().unwrap();
    let extension = parts.next().unwrap_or("");
    // Native name expansion skips components that are already long names.
    // The shape check counts UTF-16 units, including spaces, rather than ANSI
    // bytes; leading/multiple dots and a base longer than eight also skip it.
    !base.is_empty()
        && base.encode_utf16().count() <= 8
        && extension.encode_utf16().count() <= 3
        && parts.next().is_none()
}

fn resolve(env: &mut Env<'_>, path: &str, long: bool, api: &str) -> Result<String, u32> {
    if path.starts_with(r"\\") {
        unavailable(
            env,
            api,
            "UNC and device namespace path-name conversion is unavailable",
        );
        return Err(50);
    }
    if path.is_empty()
        || path
            .chars()
            .any(|c| matches!(c, '*' | '?' | '<' | '>' | '|' | '"'))
    {
        return Err(123); // ERROR_INVALID_NAME
    }
    let drive_prefix =
        if path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic() {
            2
        } else {
            0
        };
    if path[drive_prefix..].contains(':') {
        unavailable(
            env,
            api,
            "alternate data stream path-name conversion is unavailable",
        );
        return Err(50);
    }
    let full = env.ext.get::<Win32>().vfs.normalize(path);
    // Synthetic system files already expose one immutable long name. They
    // have no enumerable host parents or additional short-name aliases.
    if matches!(system_file(env, &full), Ok(Some(_))) {
        return Ok(path.to_owned());
    }
    let vfs = &env.ext.get::<Win32>().vfs;
    let final_exists = existing(vfs, &full);
    let mut result = path[..drive_prefix].to_owned();
    let mut start = drive_prefix;
    let ends = path[drive_prefix..]
        .char_indices()
        .filter_map(|(index, character)| {
            matches!(character, '\\' | '/').then_some((drive_prefix + index, Some(character)))
        })
        .chain(std::iter::once((path.len(), None)));
    for (end, separator) in ends {
        let component = &path[start..end];
        if component.is_empty() || matches!(component, "." | "..") {
            result.push_str(component);
        } else {
            if component.ends_with([' ', '.']) {
                unavailable(
                    env,
                    api,
                    "trailing dots or spaces in path components are unavailable",
                );
                return Err(50);
            }
            let parent = vfs.normalize(&path[..start]);
            let child = vfs.normalize(&path[..end]);
            if !existing(vfs, &child) {
                if final_exists {
                    unavailable(env, api, "virtual mount ancestors cannot be enumerated");
                    return Err(50);
                }
                return Err(if end == path.len() && vfs.is_dir(&parent) {
                    2
                } else {
                    3
                });
            }
            if separator.is_some() && !vfs.is_dir(&child) {
                return Err(3);
            }
            if long && short_name_candidate(component) {
                match vfs.list(&parent) {
                    Some(entries) => {
                        let Some(entry) = entries
                            .into_iter()
                            .find(|entry| entry.name.eq_ignore_ascii_case(component))
                        else {
                            unavailable(
                                env,
                                api,
                                "a backing path has no matching VFS long-name directory entry",
                            );
                            return Err(50);
                        };
                        result.push_str(&entry.name);
                    }
                    None if !existing(vfs, &parent) => {
                        // A virtual mount's name is not a host directory entry.
                        // Preserve that supplied prefix and canonicalize its
                        // enumerable children through the merged VFS view.
                        result.push_str(component);
                    }
                    None => return Err(err::ACCESS_DENIED),
                }
            } else {
                // Keep already-long components unchanged. Short-name queries
                // also retain this usable spelling on a volume with no aliases.
                result.push_str(component);
            }
        }
        if let Some(separator) = separator {
            result.push(separator);
            start = end + 1;
        }
    }
    if !final_exists {
        return Err(3);
    }
    Ok(result)
}

fn convert(env: &mut Env<'_>, pointer: u32, output: u32, cap: u32, wide: bool, long: bool) -> u32 {
    let api = match (long, wide) {
        (false, false) => "kernel32.dll!GetShortPathNameA",
        (false, true) => "kernel32.dll!GetShortPathNameW",
        (true, false) => "kernel32.dll!GetLongPathNameA",
        (true, true) => "kernel32.dll!GetLongPathNameW",
    };
    if output == 0 && cap != 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    let path = input(env, pointer, wide, api).and_then(|path| resolve(env, &path, long, api));
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            set_last_error(env, error);
            return 0;
        }
    };
    if !wide {
        let codec = &env.ext.get::<Win32>().codec;
        if codec.decode(&codec.encode(&path)) != path {
            adv_abi::warn_unavailable(
                env,
                api,
                "resolved path cannot be represented in the guest ANSI code page",
            );
            set_last_error(env, 1113); // ERROR_NO_UNICODE_TRANSLATION
            return 0;
        }
    }
    match path_string(env, &path, cap, output, None, wide) {
        Ok(length) => length,
        Err(_) => {
            set_last_error(env, 998); // ERROR_NOACCESS
            0
        }
    }
}

#[win32(kernel32, name = "GetShortPathNameA", level = implement)]
fn short_a(env: &mut Env<'_>, path: AnsiStr, output: AnsiStr, cap: u32) -> u32 {
    convert(env, path.0.addr(), output.0.addr(), cap, false, false)
}
#[win32(kernel32, name = "GetShortPathNameW", level = implement)]
fn short_w(env: &mut Env<'_>, path: WideStr, output: WideStr, cap: u32) -> u32 {
    convert(env, path.0.addr(), output.0.addr(), cap, true, false)
}
#[win32(kernel32, name = "GetLongPathNameA", level = implement)]
fn long_a(env: &mut Env<'_>, path: AnsiStr, output: AnsiStr, cap: u32) -> u32 {
    convert(env, path.0.addr(), output.0.addr(), cap, false, true)
}
#[win32(kernel32, name = "GetLongPathNameW", level = implement)]
fn long_w(env: &mut Env<'_>, path: WideStr, output: WideStr, cap: u32) -> u32 {
    convert(env, path.0.addr(), output.0.addr(), cap, true, true)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [short_a_ABI, short_w_ABI, long_a_ABI, long_w_ABI] {
        registry.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel32::file::locking::tests::{args, env, fixture, returned};
    use adv_cpu::JitCpu;
    use adv_machine::Machine;

    fn put(machine: &mut Machine<JitCpu>, pointer: u32, path: &str, wide: bool) {
        let bytes = if wide {
            path.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect()
        } else {
            let mut bytes = machine.ext.get::<Win32>().codec.encode(path);
            bytes.push(0);
            bytes
        };
        machine.cpu.write(pointer, &bytes).unwrap();
    }

    fn call(
        machine: &mut Machine<JitCpu>,
        path: &str,
        wide: bool,
        long: bool,
        cap: u32,
        output: u32,
    ) -> u32 {
        put(machine, 0x410200, path, wide);
        args(machine, &[0x410200, output, cap]);
        let handler = match (long, wide) {
            (false, false) => short_a_ABI.handler,
            (false, true) => short_w_ABI.handler,
            (true, false) => long_a_ABI.handler,
            (true, true) => long_w_ABI.handler,
        };
        returned(handler(&mut env(machine)))
    }

    fn read(machine: &mut Machine<JitCpu>, pointer: u32, wide: bool) -> String {
        if wide {
            String::from_utf16(&wstr(&machine.cpu, pointer)).unwrap()
        } else {
            machine
                .ext
                .get::<Win32>()
                .codec
                .decode(&cstr(&machine.cpu, pointer))
        }
    }

    #[test]
    fn names_preserve_relative_structure_and_return_usable_paths_without_aliases() {
        let (mut machine, root) = fixture("path-names");
        std::fs::create_dir(root.join("game/Data Folder")).unwrap();
        std::fs::write(root.join("game/Data Folder/Long Filename.bin"), b"data").unwrap();
        let mut registry = Registry::default();
        register(&mut registry);
        for name in [
            "GetShortPathNameA",
            "GetShortPathNameW",
            "GetLongPathNameA",
            "GetLongPathNameW",
        ] {
            assert_eq!(
                registry.get(K32, name).unwrap().conv,
                adv_abi::Conv::Stdcall { argc: 3 }
            );
            assert!(registry.dynamic_available(K32, name));
        }
        assert!(registry.signature_conflicts().is_empty());
        for wide in [false, true] {
            let path = "./data folder/Long Filename.bin";
            set_last_error(&mut env(&mut machine), 0x1234);
            assert_eq!(
                call(&mut machine, path, wide, false, 0, 0),
                path.len() as u32 + 1
            );
            machine.cpu.write(0x410100, &[0xab; 96]).unwrap();
            assert_eq!(
                call(&mut machine, path, wide, false, path.len() as u32, 0x410100),
                path.len() as u32 + 1
            );
            let mut bytes = [0; 96];
            machine.cpu.read(0x410100, &mut bytes).unwrap();
            assert_eq!(bytes, [0xab; 96]);
            assert_eq!(
                call(&mut machine, path, wide, false, 128, 0x410100),
                path.len() as u32
            );
            assert_eq!(read(&mut machine, 0x410100, wide), path);
            assert_eq!(crate::util::last_error(&env(&mut machine)), 0x1234);
            let usable = read(&mut machine, 0x410100, wide);
            assert!(machine.ext.get::<Win32>().vfs.lookup(&usable).is_some());
            let path = ".\\data folder\\..\\data folder\\LONG FILENAME.BIN";
            let expected = path;
            assert_eq!(
                call(&mut machine, path, wide, true, 128, 0x410200),
                expected.len() as u32
            );
            assert_eq!(read(&mut machine, 0x410200, wide), expected);
            assert_eq!(
                call(
                    &mut machine,
                    "C:\\Game\\data folder\\LONG FILENAME.BIN",
                    wide,
                    true,
                    128,
                    0x410100
                ),
                37
            );
            assert_eq!(
                read(&mut machine, 0x410100, wide),
                "C:\\Game\\data folder\\LONG FILENAME.BIN"
            );
        }
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_count_guest_encoding_units_and_use_overlay_directory_spelling() {
        let (mut machine, root) = fixture("path-names-encoding");
        std::fs::create_dir(root.join("game/Mix Dir")).unwrap();
        std::fs::write(root.join("game/Mix Dir/表データ.dat"), b"base").unwrap();
        std::fs::create_dir_all(root.join("data/overlay/mix dir")).unwrap();
        std::fs::write(root.join("data/overlay/mix dir/表データ.dat"), b"overlay").unwrap();
        let path = "MIX DIR\\表データ.dat";
        let expected = "mix dir\\表データ.dat";
        for wide in [false, true] {
            let length = if wide {
                expected.encode_utf16().count()
            } else {
                machine.ext.get::<Win32>().codec.encode(expected).len()
            } as u32;
            assert_eq!(call(&mut machine, path, wide, true, 0, 0), length + 1);
            assert_eq!(
                call(&mut machine, path, wide, true, length + 1, 0x410100),
                length
            );
            assert_eq!(read(&mut machine, 0x410100, wide), expected);
        }
        std::fs::write(root.join("game/Mix Dir/😀.dat"), b"unicode").unwrap();
        let path = "MIX DIR\\😀.dat";
        let expected = "mix dir\\😀.dat";
        assert_eq!(
            call(&mut machine, path, true, true, 128, 0x410100),
            expected.encode_utf16().count() as u32
        );
        assert_eq!(read(&mut machine, 0x410100, true), expected);
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_names_only_expand_short_shaped_components() {
        let (mut machine, root) = fixture("path-names-native-shape");
        // Authored native GetLongPathNameW oracle: only short-shaped components
        // are enumerated; already-long names retain the exact supplied spelling.
        let pairs = [
            ("Mixed.bin", "MIXED.BIN", "Mixed.bin"),
            ("LongFilename.bin", "longfilename.BIN", "longfilename.BIN"),
            ("Mixed.long", "MIXED.LONG", "MIXED.LONG"),
            ("A B.txt", "a b.TXT", "A B.txt"),
            ("simple", "SIMPLE", "simple"),
            ("Abcdefgh.bin", "ABCDEFGH.BIN", "Abcdefgh.bin"),
            ("Abcdefghi.bi", "ABCDEFGHI.BI", "ABCDEFGHI.BI"),
            ("Abcdefghi", "ABCDEFGHI", "ABCDEFGHI"),
            ("Ab.Cd.ef", "AB.CD.EF", "AB.CD.EF"),
            (".git", ".GIT", ".GIT"),
            ("表表表表a.bin", "表表表表A.BIN", "表表表表a.bin"),
        ];
        for (stored, _, _) in pairs {
            std::fs::write(root.join("game").join(stored), b"authored").unwrap();
        }
        for wide in [false, true] {
            for (_, input, expected) in pairs {
                let length = if wide {
                    expected.encode_utf16().count()
                } else {
                    machine.ext.get::<Win32>().codec.encode(expected).len()
                } as u32;
                assert_eq!(call(&mut machine, input, wide, true, 128, 0x410100), length);
                assert_eq!(read(&mut machine, 0x410100, wide), expected);
            }
        }
        for (stored, input, expected) in [
            ("😀😀😀a.bin", "😀😀😀A.BIN", "😀😀😀a.bin"),
            ("😀😀😀😀a.bin", "😀😀😀😀A.BIN", "😀😀😀😀A.BIN"),
        ] {
            std::fs::write(root.join("game").join(stored), b"authored").unwrap();
            assert_eq!(
                call(&mut machine, input, true, true, 128, 0x410100),
                expected.encode_utf16().count() as u32
            );
            assert_eq!(read(&mut machine, 0x410100, true), expected);
        }
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_paths_and_outputs_fail_without_changes() {
        let (mut machine, root) = fixture("path-names-validation");
        std::fs::write(root.join("game/data.bin"), b"data").unwrap();
        machine.cpu.map(0x420000, 4096, Prot::READ).unwrap();
        for long in [false, true] {
            for wide in [false, true] {
                for (path, output, cap, error) in [
                    ("missing.bin", 0x410100, 128, 2),
                    ("missing\\file.bin", 0x410100, 128, 3),
                    ("", 0x410100, 128, 123),
                    ("data.bin", 0, 128, 87),
                    ("data.bin", 0x410fff, 128, 998),
                    ("data.bin", 0x420000, 128, 998),
                    ("data.bin", u32::MAX - 3, 128, 998),
                    (r"\\.\C:\data.bin", 0x410100, 128, 50),
                    ("data.bin:stream", 0x410100, 128, 50),
                ] {
                    machine.cpu.write(0x410100, &[0xab; 96]).unwrap();
                    assert_eq!(call(&mut machine, path, wide, long, cap, output), 0);
                    assert_eq!(crate::util::last_error(&env(&mut machine)), error, "{path}");
                    let mut bytes = [0; 96];
                    machine.cpu.read(0x410100, &mut bytes).unwrap();
                    assert_eq!(bytes, [0xab; 96]);
                }
            }
        }
        assert_eq!(
            convert(&mut env(&mut machine), 0, 0x410100, 128, false, false),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
        machine.cpu.write(0x410200, &[0x00, 0xd8, 0, 0]).unwrap();
        assert_eq!(
            convert(&mut env(&mut machine), 0x410200, 0x410100, 128, true, false),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 1113);
        drop(machine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
