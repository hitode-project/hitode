//! Host cancellation must not publish a guest crash into process-family reports.
use super::*;

#[test]
fn host_time_limit_keeps_live_guest_status_and_api_eligibility() {
    let mut pe = vec![0u8; 1024];
    let put16 = |pe: &mut [u8], offset: usize, value: u16| {
        pe[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    };
    let put32 = |pe: &mut [u8], offset: usize, value: u32| {
        pe[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put16(&mut pe, 0, 0x5a4d);
    put32(&mut pe, 0x3c, 0x80);
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    for (offset, value) in [
        (0x84, 0x14c),
        (0x86, 1),
        (0x94, 224),
        (0x96, 0x102),
        (0x98, 0x10b),
    ] {
        put16(&mut pe, offset, value);
    }
    for (offset, value) in [
        (0x9c, 0x200),
        (0xa8, 0x1000),
        (0xac, 0x1000),
        (0xb4, 0x0040_0000),
        (0xb8, 4096),
        (0xbc, 512),
        (0xd0, 0x2000),
        (0xd4, 0x200),
        (0xe0, 0x100000),
        (0xe4, 4096),
        (0xe8, 0x100000),
        (0xec, 4096),
        (0xf4, 16),
        (0x180, 2),
        (0x184, 0x1000),
        (0x188, 0x200),
        (0x18c, 0x200),
        (0x19c, 0x60000020),
    ] {
        put32(&mut pe, offset, value);
    }
    put16(&mut pe, 0xc0, 4);
    put16(&mut pe, 0xc8, 4);
    put16(&mut pe, 0xdc, 3);
    pe[0x178..0x17d].copy_from_slice(b".text");
    pe[0x200..0x202].copy_from_slice(&[0xeb, 0xfe]); // jmp $

    let directory = std::env::temp_dir().join(format!("adv-shell-deadline-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let exe = directory.join("loop.exe");
    std::fs::write(&exe, pe).unwrap();
    for backend in ["interpreter", "jit"] {
        let report = directory.join(format!("{backend}.json"));
        let arguments = vec![
            exe.to_str().unwrap().to_owned(),
            "--data-dir".into(),
            directory.join(backend).to_str().unwrap().to_owned(),
            "--headless".into(),
            "--backend".into(),
            backend.into(),
            "--seconds".into(),
            "0.05".into(),
            "--report".into(),
            report.to_str().unwrap().to_owned(),
        ];
        let options = Options::parse_args(arguments.into_iter()).unwrap().unwrap();
        assert_eq!(run(options).unwrap(), 0);
        let text = std::fs::read_to_string(report).unwrap();
        assert!(text.contains("\"stop_reason\": \"time_limit\""), "{text}");
        assert!(text.contains("\"guest_exit_code\": null"), "{text}");
        assert!(text.contains("\"acceptance_eligible\": true"), "{text}");
        assert!(!text.contains("execution budget exhausted"), "{text}");
        assert!(
            !text.contains("3221225659"),
            "host stop became NOT_SUPPORTED: {text}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn bridge_report_has_an_explicit_zero_and_no_sample_metadata() {
    let c = adv_runtime::Cliffs::default();
    let report = cliffs_report(&c);
    assert!(report.contains(r#""tier1_bridge":{"entries":0,"instructions":0,"details":{}}"#));
    assert!(report.contains(r#""tier1_census":null"#));
}
