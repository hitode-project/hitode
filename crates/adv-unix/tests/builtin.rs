//! Wine loads a test-only host module in place of its own version.dll.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::dll::{Export, Import, ModuleImage};
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_machine::{HostFn, Resolver, RunResult};
use adv_pe::{ImportModule, ImportName};

struct Platform(Registry);
impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        adv_unix::Platform
            .host_fn(module, name)
            .or_else(|| self.0.get(module, name))
    }
    fn provides(&self, module: &str) -> bool {
        adv_unix::Platform.provides(module) || self.0.provides(module)
    }
}

fn compute(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(env.arg(0) * 1000 + env.arg(1) * 100 + env.arg(2) * 10 + env.arg(3))
}

fn imported(env: &mut Env<'_>) -> Outcome {
    let import = Import::bind(env, "kernel32.dll", "lstrlenA").unwrap();
    let text = env.arg(0);
    import.call(env, [text], |_, result| {
        Outcome::Return(result.value() + 10)
    })
}

// spec: WINE-DLL-1, WINE-DLL-2, WINE-DLL-3, WINE-SYS-7
#[test]
fn wine_loads_host_exports_and_calls_hookable_imports() {
    let mut registry = Registry::default();
    registry.add(
        "version.dll",
        "HostCompute",
        Conv::Stdcall { argc: 4 },
        compute,
    );
    registry.add(
        "version.dll",
        "HostImport",
        Conv::Stdcall { argc: 1 },
        imported,
    );
    let Some((result, output)) = common::run_probe_with_prefix(
        "builtin.exe",
        false,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            let image = ModuleImage::build(
                machine,
                "version.dll",
                &[
                    Export {
                        name: "HostCompute",
                        ordinal: 7,
                    },
                    Export {
                        name: "HostImport",
                        ordinal: 11,
                    },
                ],
                &[ImportModule {
                    dll: "kernel32.dll".into(),
                    imports: vec![adv_pe::Import {
                        name: ImportName::Name {
                            hint: 0,
                            name: "lstrlenA".into(),
                        },
                        iat_rva: 0,
                    }],
                }],
            )
            .unwrap();
            let original = prefix.join("drive_c/windows/system32/version.dll");
            let target = std::fs::read_link(&original).unwrap();
            let before = std::fs::read(&target).unwrap();
            adv_unix::builtin::install(prefix, &image).unwrap();
            assert!(!original.is_symlink());
            let installed = std::fs::read(&original).unwrap();
            assert_eq!(&installed[0x40..0x51], b"Wine builtin DLL\0");
            assert_eq!(std::fs::read(&target).unwrap(), before);
            use std::os::unix::fs::MetadataExt;
            let inode = std::fs::metadata(&original).unwrap().ino();
            adv_unix::builtin::install(prefix, &image).unwrap();
            assert_eq!(std::fs::metadata(&original).unwrap().ino(), inode);
        },
    ) else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["names=1 ordinal=1 host=1234 import=14 iat=41 entry=61 rebased=1"]
    );
}
