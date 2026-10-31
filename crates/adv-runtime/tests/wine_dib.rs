//! Offscreen GDI reaches the pinned Unix DIB bodies and preserves Wine's pixels.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_machine::{Env, HostFn, Outcome, Resolver};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

#[derive(Default)]
struct Calls(std::collections::HashMap<String, u32>);
struct Platform;
impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        let mut f = adv_unix::Platform.host_fn(module, name)?;
        if module == "hitode.native" && name.starts_with("win32u.so!") {
            f.handler = counted;
        }
        Some(f)
    }
    fn provides(&self, module: &str) -> bool {
        adv_unix::Platform.provides(module)
    }
}
fn counted(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let body = adv_unix::Platform
        .host_fn("hitode.native", &name)
        .unwrap()
        .handler;
    let declines = adv_nt::native::declines(env.ext);
    let outcome = body(env);
    if adv_nt::native::declines(env.ext) == declines {
        *env.ext.get_or_default::<Calls>().0.entry(name).or_default() += 1;
    }
    outcome
}

// spec: NAT-ID-2, NAT-BODY-4
#[test]
fn offscreen_gdi_reaches_dib_bodies() {
    if wine::wine().is_none() {
        return;
    }
    // Each run needs its own process because Wine's server holds global state.
    let mut outputs = Vec::new();
    for name in ["dib_native_child", "dib_wine_child"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--ignored", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let line = text.lines().find(|l| l.starts_with("blend=")).unwrap();
        assert!(line.starts_with("blend=1 text=1 "), "{line}");
        outputs.push(line.to_owned());
    }
    assert_eq!(outputs[0], outputs[1]);
}
fn probe(native: bool) {
    use adv_machine::RunResult;
    let Some((result, output)) = common::run_probe_inspect(
        "dib_bodies.exe",
        false,
        "null",
        Platform,
        |machine, prefix| {
            std::fs::copy(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../adv-device/tests/fixtures/font-resource.ttf"),
                prefix.join("drive_c/private.ttf"),
            )
            .unwrap();
            if native {
                *machine = adv_machine::Machine::new(
                    adv_cpu::Cpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
                    Platform,
                    adv_machine::Clock::virtual_time(),
                )
                .unwrap();
            }
            adv_unix::install(machine);
        },
        |machine| {
            let images = machine.ext.get::<adv_nt::identity::Images>();
            let module = images.0.values().find(|m| m.name == "win32u.so").unwrap();
            let table = module.table().unwrap();
            for name in ["blend_rects_8888", "solid_rects_32", "draw_glyph_8888"] {
                let entry = module.base + table.named(name).unwrap().rva;
                assert!(
                    machine.sys.redirects.contains_key(&entry),
                    "{name} is unbound"
                );
            }
            if native {
                let calls = &machine.ext.get::<Calls>().0;
                for family in ["blend_rects_", "solid_rects_", "draw_glyph_"] {
                    assert!(
                        calls
                            .iter()
                            .any(|(name, &count)| name.contains(family) && count > 0),
                        "{family} did not reach a body: {calls:?}"
                    );
                }
            }
        },
    ) else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    print!("{output}");
}
// spec: NAT-BODY-4
#[test]
#[ignore = "subprocess of offscreen_gdi_reaches_dib_bodies"]
fn dib_native_child() {
    probe(true);
}
// spec: NAT-BODY-4
#[test]
#[ignore = "subprocess of offscreen_gdi_reaches_dib_bodies"]
fn dib_wine_child() {
    probe(false);
}
