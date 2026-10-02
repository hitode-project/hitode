use std::process::Command;

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
        assert!(String::from_utf8_lossy(&output.stderr).contains("--codepage must be 932 or 936"));
    }
}

#[test]
fn accepts_documented_codepages() {
    for codepage in ["932", "936"] {
        let output = Command::new(env!("CARGO_BIN_EXE_adv-run"))
            .args(["--codepage", codepage, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--codepage 932|936"));
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
