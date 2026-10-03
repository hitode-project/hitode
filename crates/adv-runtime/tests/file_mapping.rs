//! Native x86 data-section lifecycle, coherence, COPY and namespace oracle.
#![cfg(windows)]
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::os::windows::process::CommandExt;
use std::{path::Path, process::Command, time::Duration};
#[path = "support/cpu.rs"]
mod cpu;

#[test]
fn file_mapping_family_matches_native_lifecycle_and_coherence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/file-mapping-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-file-mapping.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = output.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let exe = output.join("fixture_file_mapping.exe");
    let status = Command::new(&exe)
        .current_dir(&native)
        .creation_flags(0x08000000)
        .status()
        .unwrap();
    assert!(status.success(), "native exit {:?}", status.code());
    let reference = std::fs::read(native.join("file-mapping.bin")).unwrap();
    assert_eq!(reference.len(), 162 * 4);
    let serial = std::fs::read(native.join("file-mapping-serial.bin")).unwrap();
    assert_eq!(serial.len(), 20 * 4);
    // Distinct virtual views alias the same physical page. This host's bulk
    // fast-string REP can differ from explicit MOVS element order. Preserve its
    // raw report and use a separate single-element native oracle for this case.
    for (index, (bulk, scalar)) in reference.as_chunks::<4>().0[67..87]
        .iter()
        .zip(serial.as_chunks::<4>().0)
        .enumerate()
    {
        if bulk != scalar {
            eprintln!(
                "native physical-alias REP word {}: bulk={:#x}, serial={:#x}",
                index + 67,
                u32::from_le_bytes(*bulk),
                u32::from_le_bytes(*scalar)
            );
        }
    }
    for backend in ["interpreter", "jit", "unicorn"] {
        let profile = output.join(backend);
        let mut runtime =
            Runtime::with_cpu(&exe, &profile, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(5)));
        assert!(
            matches!(runtime.machine.run_virtual(5_000_000), RunResult::Exited(0)),
            "{backend}: {}",
            runtime.report()
        );
        let actual = std::fs::read(profile.join("overlay/file-mapping.bin")).unwrap();
        assert_eq!(
            std::fs::read(profile.join("overlay/file-mapping-serial.bin")).unwrap(),
            serial,
            "{backend} native serial alias oracle"
        );
        assert_eq!(actual.len(), reference.len());
        for (i, (actual, expected)) in actual
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            let expected = if (67..87).contains(&i) {
                &serial.as_chunks::<4>().0[i - 67]
            } else {
                expected
            };
            assert_eq!(
                u32::from_le_bytes(*actual),
                u32::from_le_bytes(*expected),
                "{backend} result word {i}"
            );
        }
    }
}
