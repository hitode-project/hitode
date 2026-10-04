//! The CLI's post-construction code-page selection refreshes the shared ANSI owner.
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_runtime::boot::Runtime;
use adv_win32::Win32;
#[path = "support/cpu.rs"]
mod backend;
#[path = "../../adv-win32/tests/support/data_fixture.rs"]
mod fixture;
#[test]
fn cli_codepage_changes_keep_data_cell_and_buffer_identity_and_report_failures() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-data-codepage");
    for backend in ["interpreter", "jit", "unicorn"] {
        let directory = root.join(format!("{}-{backend}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let exe = directory.join("表.exe");
        std::fs::write(&exe, fixture::image("msvcr90.dll", &["_acmdln", "_wcmdln"])).unwrap();
        let mut runtime = Runtime::with_cpu(
            &exe,
            directory.join("overlay"),
            backend::Cpu::new(backend).unwrap(),
            16,
            12,
        )
        .unwrap();
        let ansi_cell = runtime.machine.cpu.read_u32(fixture::IAT).unwrap();
        let wide_cell = runtime.machine.cpu.read_u32(fixture::IAT + 4).unwrap();
        let ansi = runtime.machine.cpu.read_u32(ansi_cell).unwrap();
        let wide = runtime.machine.cpu.read_u32(wide_cell).unwrap();
        let line = format!("\"{}\"", runtime.machine.ext.get::<Win32>().exe_path());
        assert_eq!(
            runtime.machine.cpu.read_cstr(ansi, 32768).unwrap(),
            adv_os::Codec::new(932).encode(&line)
        );
        let mut before_wide = vec![0; (line.encode_utf16().count() + 1) * 2];
        runtime.machine.cpu.read(wide, &mut before_wide).unwrap();
        runtime.set_codepage(936).unwrap();
        assert_eq!(runtime.machine.ext.get::<Win32>().cfg.os.codepage, 936);
        assert_eq!(
            runtime.machine.cpu.read_cstr(ansi, 32768).unwrap(),
            adv_os::Codec::new(936).encode(&line)
        );
        assert_eq!(runtime.machine.cpu.read_u32(ansi_cell).unwrap(), ansi);
        assert_eq!(runtime.machine.cpu.read_u32(wide_cell).unwrap(), wide);
        let mut after_wide = vec![0; before_wide.len()];
        runtime.machine.cpu.read(wide, &mut after_wide).unwrap();
        assert_eq!(after_wide, before_wide);
        runtime.machine.cpu.write(ansi, b"guest edit\0").unwrap();
        runtime.machine.cpu.write_u32(ansi_cell, 0x1234).unwrap();
        runtime.set_codepage(936).unwrap();
        assert_eq!(
            runtime.machine.cpu.read_cstr(ansi, 32768).unwrap(),
            b"guest edit"
        );
        assert_eq!(runtime.machine.cpu.read_u32(ansi_cell).unwrap(), 0x1234);
        runtime
            .machine
            .cpu
            .protect(ansi & !4095, 4096, Prot::READ)
            .unwrap();
        assert!(runtime.set_codepage(932).is_err());
        assert_eq!(runtime.machine.ext.get::<Win32>().cfg.os.codepage, 936);
        assert_eq!(
            runtime.machine.cpu.read_cstr(ansi, 32768).unwrap(),
            b"guest edit"
        );
    }
}
