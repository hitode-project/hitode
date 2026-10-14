use std::process::Command;

#[test]
fn api_modes_are_explicit_and_validated_before_loading() {
    for mode in ["strict", "discovery"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .args(["--api-mode", mode, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--api-mode strict|discovery"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
        .args(["missing.exe", "--api-mode", "pretend-success"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--api-mode must be strict or discovery")
    );
}

#[test]
fn rejects_unsupported_codepages_before_loading_guest_files() {
    for codepage in ["0", "1252", "65001", "-1", "GBK"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .args(["--exe", "missing-guest.exe", "--codepage", codepage])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "invalid codepage {codepage} was accepted"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--codepage must be auto, 932 or 936")
        );
    }
}

#[test]
fn accepts_documented_codepages() {
    for codepage in ["auto", "932", "936"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .args(["--codepage", codepage, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--codepage auto|932|936"));
    }
}

#[test]
fn rejects_finite_seconds_that_overflow_duration() {
    for seconds in ["1e30", "1.7976931348623157e308"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .args(["--exe", "missing-guest.exe", "--seconds", seconds])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--seconds is too large to represent as a duration")
        );
    }
}
