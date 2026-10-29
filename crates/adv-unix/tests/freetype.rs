//! Differential FreeType and GDI calls on Wine, without a window or device.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::dll::{Export, ModuleImage};
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{Conv, Env, HostFn, Outcome, Resolver, RunResult};
use std::path::PathBuf;

struct Platform;
impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if module == "freetype-test.dll" && name == "FontPath" {
            let mut function = adv_unix::Platform.host_fn("libc.so.6", "getenv")?;
            function.conv = Conv::Stdcall { argc: 0 };
            function.handler = |_| Outcome::Return(0x601000);
            return Some(function);
        }
        let mut function = match (module, name) {
            ("freetype-test.dll", "OpenFreeType") => {
                adv_unix::Platform.host_fn("libc.so.6", "dlopen")?
            }
            ("freetype-test.dll", "FreeTypeSymbol") => {
                adv_unix::Platform.host_fn("libc.so.6", "dlsym")?
            }
            ("test-libc", name) => return adv_unix::Platform.host_fn("libc.so.6", name),
            ("file-view-freetype", name) => {
                return adv_unix::Platform.host_fn("libfreetype.so.6", name);
            }
            _ => adv_unix::Platform.host_fn(module, name)?,
        };
        if module == "freetype-test.dll" {
            function.conv = Conv::Stdcall { argc: 2 };
        }
        if module == "libfreetype.so.6" && name == "FT_New_Memory_Face" {
            function.handler = file_view_face;
        }
        if name == "dlopen" || name == "OpenFreeType" {
            function.handler = open;
        }
        Some(function)
    }
    fn provides(&self, module: &str) -> bool {
        module == "freetype-test.dll"
            || module == "test-libc"
            || module == "file-view-freetype"
            || adv_unix::Platform.provides(module)
    }
}

// spec: WINE-UNIX-6, A64-MEM-13
// Exercise the byte oracle with file-backed host input, then remove and reuse
// the guest view before glyph loads. FreeType must retain its own input view.
fn file_view_face(env: &mut Env<'_>) -> Outcome {
    use adv_nt::memory::{self, Memory, PAGE_READONLY};
    use std::io::Write;
    use std::os::fd::AsFd;
    let mut args: [u32; 5] = std::array::from_fn(|i| env.arg(i as u32));
    if args[1] == 0 || args[2] == 0 || (args[2] as i32) < 0 {
        return (adv_unix::Platform
            .host_fn("libfreetype.so.6", "FT_New_Memory_Face")
            .unwrap()
            .handler)(env);
    }
    let mut bytes = vec![0; args[2] as usize];
    env.cpu.read(args[1], &mut bytes).unwrap();
    let size = args[2].next_multiple_of(4096);
    let path = std::env::temp_dir().join(format!("adv-freetype-view-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file.write_all(&bytes).unwrap();
    file.set_len(size.into()).unwrap();
    let base = memory::map_shared_view(
        env.cpu,
        &mut env.sys.vm,
        env.ext.get_or_default::<Memory>(),
        file.as_fd(),
        0,
        size,
        PAGE_READONLY,
    )
    .unwrap();
    assert!(env.cpu.read_only_file_view(base, bytes.len()).is_some());
    env.ext
        .get_or_default::<Opened>()
        .0
        .push(("file-view-input".into(), base));
    args[1] = base;
    let binding = env
        .sys
        .thunks
        .resolve(env.cpu, "file-view-freetype", "FT_New_Memory_Face")
        .unwrap();
    Outcome::call_aligned(env, binding, args, move |env, returned| {
        memory::remove_view(
            env.cpu,
            &mut env.sys.vm,
            env.ext.get_or_default::<Memory>(),
            base,
        );
        env.cpu.map(base, size, adv_cpu::backend::Prot::RW).unwrap();
        env.cpu.fill_memory(base, 0, size as usize).unwrap();
        env.cpu
            .protect(base, size, adv_cpu::backend::Prot(0))
            .unwrap();
        env.cpu.unmap(base, size).unwrap();
        Outcome::Return(returned.value())
    })
}

#[derive(Default)]
struct Opened(Vec<(String, u32)>);
fn open(env: &mut Env<'_>) -> Outcome {
    let name = env.cpu.read_cstr(env.arg(0), 4096).unwrap();
    let requested = String::from_utf8(name.clone()).unwrap();
    let mut path = name;
    if requested == "libfreetype.so.6"
        && std::env::var("HITODE_FREETYPE_TEST").as_deref() == Ok("i386")
    {
        path = PathBuf::from(std::env::var_os("ADV_WINE_DIST").unwrap())
            .join("lib/wine/i386-unix/libfreetype.so.6")
            .into_os_string()
            .into_encoded_bytes();
    }
    path.push(0);
    // Reuse a test-only scratch page for the argument, before dlopen starts.
    let address = 0x600000;
    env.cpu.write(address, &path).unwrap();
    let binding = env
        .sys
        .thunks
        .resolve(env.cpu, "test-libc", "dlopen")
        .unwrap();
    let flags = env.arg(1);
    Outcome::call_aligned(env, binding, [address, flags], move |env, returned| {
        env.ext
            .get_or_default::<Opened>()
            .0
            .push((requested, returned.value()));
        Outcome::Return(returned.value())
    })
}

// spec: WINE-UNIX-6
#[test]
fn host_freetype_matches_i386() {
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINE_PROBES").is_none()
    {
        eprintln!("skipped: ADV_WINE_DIST and ADV_WINE_PROBES are required");
        return;
    }
    let mode = std::env::var("HITODE_FREETYPE_TEST").unwrap_or_default();
    if mode.is_empty() {
        let directory =
            std::env::temp_dir().join(format!("adv-freetype-diff-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let original = PathBuf::from(std::env::var_os("ADV_WINE_DIST").unwrap());
        let host_dist = directory.join("dist");
        let unix = host_dist.join("lib/wine/i386-unix");
        std::fs::create_dir_all(&unix).unwrap();
        std::os::unix::fs::symlink(original.join("share"), host_dist.join("share")).unwrap();
        std::os::unix::fs::symlink(
            original.join("lib/wine/i386-windows"),
            host_dist.join("lib/wine/i386-windows"),
        )
        .unwrap();
        for entry in std::fs::read_dir(original.join("lib/wine/i386-unix")).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() != "libfreetype.so.6" {
                std::os::unix::fs::symlink(entry.path(), unix.join(entry.file_name())).unwrap();
            }
        }
        for mode in ["i386", "host"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "host_freetype_matches_i386", "--nocapture"])
                .env(
                    "ADV_WINE_DIST",
                    if mode == "host" {
                        &host_dist
                    } else {
                        &original
                    },
                )
                .env("FREETYPE_PROPERTIES", "truetype:interpreter-version=35")
                .env("HITODE_FREETYPE_TEST", mode)
                .env("HITODE_FREETYPE_OUTPUT", directory.join(mode))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let expected = std::fs::read(directory.join("i386")).unwrap();
        let actual = std::fs::read(directory.join("host")).unwrap();
        assert_eq!(actual.len(), expected.len());
        if let Some(at) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
            panic!(
                "FreeType byte {at}: host={} i386={}; artifacts: {}",
                actual[at],
                expected[at],
                directory.display()
            );
        }
        println!("{} identical FreeType/GDI output bytes", actual.len());
        std::fs::remove_dir_all(directory).unwrap();
        return;
    }
    let (result, output) = common::run_probe_inspect(
        "freetype.exe",
        false,
        "null",
        Platform,
        |machine, prefix| {
            adv_unix::install(machine);
            machine
                .cpu
                .map(0x600000, 0x2000, adv_cpu::backend::Prot::RW)
                .unwrap();
            let mut path = prefix
                .join("drive_c/freetype.ttf")
                .into_os_string()
                .into_encoded_bytes();
            path.push(0);
            machine.cpu.write(0x601000, &path).unwrap();
            let image = ModuleImage::build(
                machine,
                "freetype-test.dll",
                &[
                    Export {
                        name: "OpenFreeType",
                        ordinal: 1,
                    },
                    Export {
                        name: "FreeTypeSymbol",
                        ordinal: 2,
                    },
                    Export {
                        name: "FontPath",
                        ordinal: 3,
                    },
                ],
                &[],
            )
            .unwrap();
            adv_unix::builtin::install(&prefix.join("drive_c"), &image).unwrap();
            std::fs::copy(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../adv-device/tests/fixtures/font-resource-japanese.ttf"
                ),
                prefix.join("drive_c/freetype.ttf"),
            )
            .unwrap();
        },
        |machine| {
            let opens = &machine.ext.get_or_default::<Opened>().0;
            if mode == "host" {
                assert!(opens.iter().any(|(name, _)| name == "file-view-input"));
            }
            let ft: Vec<_> = opens
                .iter()
                .filter(|(name, _)| name == "libfreetype.so.6")
                .collect();
            assert!(ft.len() >= 2, "direct and win32u dlopen: {opens:?}");
            for (_, handle) in ft {
                assert_eq!(*handle == 0xffff_ff01, mode == "host");
            }
            let elf = &machine
                .ext
                .get_or_default::<adv_unix::loader::Libraries>()
                .0;
            assert_eq!(
                elf.iter().any(|library| library
                    .path
                    .file_name()
                    .is_some_and(|name| name == "libfreetype.so.6")),
                mode == "i386"
            );
            let prefix = std::env::temp_dir()
                .join(format!("adv-nt-freetype.exe-{}/prefix", std::process::id()));
            std::fs::copy(
                prefix.join("drive_c/freetype.bin"),
                std::env::var_os("HITODE_FREETYPE_OUTPUT").unwrap(),
            )
            .unwrap();
        },
    )
    .unwrap();
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}\n{output}"
    );
    assert!(output.contains("FreeType=2.13.2"), "{output}");
    println!("{mode}: {output}");
}
