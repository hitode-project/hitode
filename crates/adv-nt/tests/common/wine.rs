// The Wine installation and probes a test runs against (docs/building.md).
// This is the only reader of `ADV_WINE_DIST`; `tools/wine/probes.py --check`
// rejects any other. Include it as
// `mod wine { include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../adv-nt/tests/common/wine.rs")); }`.

use std::path::{Path, PathBuf};

/// The stripped installation `ADV_WINE_DIST` names, or `None` when it is
/// unset: a machine without Wine skips these tests. One that is set must be
/// complete, or the test fails rather than skipping.
pub fn dist() -> Option<PathBuf> {
    let Some(dist) = std::env::var_os("ADV_WINE_DIST").map(PathBuf::from) else {
        eprintln!("skipped: ADV_WINE_DIST is not set");
        return None;
    };
    for required in [
        "lib/wine/i386-windows/ntdll.dll",
        "lib/wine/i386-unix/win32u.so",
        "share/hitode/identity/ntdll.dll.tsv",
        "share/hitode/prefix/system.reg",
    ] {
        assert!(
            dist.join(required).is_file(),
            "ADV_WINE_DIST={} has no {required}: it must name the stripped installation, \
             with its identity tables and prefix template",
            dist.display()
        );
    }
    Some(dist)
}

/// The installation and the probes built from the current sources.
pub struct Wine {
    pub dist: PathBuf,
    pub probes: PathBuf,
}

/// As [`dist`], with the probes `tools/wine/probes.py` builds from
/// `crates/*/tests/probe`, so none is missing or older than its source.
pub fn wine() -> Option<Wine> {
    let dist = dist()?;
    Some(Wine {
        dist,
        probes: probes().to_owned(),
    })
}

fn probes() -> &'static Path {
    static PROBES: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PROBES.get_or_init(|| {
        // A device or simulator cannot run the cross compiler: its runner
        // builds the probes on the host and passes their directory.
        if cfg!(any(target_os = "ios", target_os = "android")) {
            return std::env::var_os("ADV_WINE_PROBES")
                .map(PathBuf::from)
                .expect("the device runner passes ADV_WINE_PROBES from tools/wine/probes.py");
        }
        let tool = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/wine/probes.py");
        let output = std::process::Command::new("python3")
            .arg(&tool)
            .output()
            .unwrap_or_else(|error| panic!("python3 {}: {error}", tool.display()));
        assert!(
            output.status.success(),
            "building the Wine probes failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
    })
}
