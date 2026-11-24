//! Registry restoration via native `.reg` import, and scripts run as programs
//! in Wine's `cmd.exe` with their console piped.
//!
//! It needs `ADV_WINE_DIST`, as the other Wine tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_os::nls::Codec;
use adv_runtime::wine::session::{
    Config, Host, Image, Program, RegistryReport, SessionOptions, Stop,
};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

/// Runs `script` as a program, `input` its console input, and returns what
/// it wrote to the registry, its exit and its console output.
fn run_script(
    host: &mut Host,
    config: &Config<'_>,
    script: &Path,
    input: &[u8],
) -> (RegistryReport, Stop, String) {
    let (mut output, output_write) = std::io::pipe().unwrap();
    let (input_read, mut input_write) = std::io::pipe().unwrap();
    input_write.write_all(input).unwrap();
    let options = SessionOptions {
        program: Some(Program {
            output: Some(output_write.as_raw_fd()),
            input: Some(input_read.as_raw_fd()),
            ..Program::new(Image::Host(script.to_owned()))
        }),
        ..SessionOptions::default()
    };
    let watch = host.watch_registry(config, &options).unwrap().unwrap();
    let mut session = host
        .start(
            config,
            adv_sync::WakeEvent::new().unwrap(),
            &options,
            |_| Ok(()),
        )
        .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    drop(output_write);
    let mut bytes = Vec::new();
    output.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf16_lossy(&Codec::new(936).to_utf16(936, &bytes));
    (host.settle_registry(config, &watch).unwrap(), stop, text)
}

// spec: WINE-SES-11
#[test]
fn restores_reg_and_batch_files_into_the_shared_prefix() {
    let Some(dist) = wine::dist() else {
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-registry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let game = root.join("game");
    let prefix = root.join("prefix");
    std::fs::create_dir_all(&game).unwrap();
    let exe = game.join("game.exe");
    std::fs::write(&exe, b"").unwrap();

    let mut host = Host::default();
    let config = Config {
        dist: &dist,
        prefix: &prefix,
        drive: None,
        locale: 0x804, // zh-CN (CP936)
        fonts: "",
        exe: Some(&exe),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };

    // 1. Native .reg import (UTF-16LE v5) with WOW6432Node normalization.
    let reg_v5 = game.join("sample_v5.reg");
    let v5_text = concat!(
        "Windows Registry Editor Version 5.00\r\n",
        "\r\n",
        "[HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\HitodeRegTest\\V5]\r\n",
        "\"InstallPath\"=\"D:\\\\\"\r\n", // privacy: allow authored guest drive path
        "\"Version\"=dword:0000002a\r\n",
        "[HKEY_CURRENT_USER\\Software\\HitodeRegTest\\V5]\r\n",
        "\"Title\"=\"测试标题\"\r\n",
    );
    let mut v5_bytes = vec![0xFF, 0xFE];
    for unit in v5_text.encode_utf16() {
        v5_bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&reg_v5, v5_bytes).unwrap();

    let report = host.import_registry(&config, &reg_v5).unwrap();
    assert_eq!(report.entries, 3, "unexpected report: {report:?}");
    assert_eq!(report.normalized_wow64, 2, "unexpected report: {report:?}");

    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    let user_reg = std::fs::read_to_string(prefix.join("user.reg")).unwrap();
    assert!(
        system_reg.contains(r"[Software\\HitodeRegTest\\V5]"),
        "normalized 32-bit key missing from system.reg:\n{system_reg}"
    );
    assert!(system_reg.contains(r#""InstallPath"="D:\\""#)); // privacy: allow authored guest drive path
    assert!(system_reg.contains(r#""Version"=dword:0000002a"#));
    assert!(
        user_reg.contains(r"[Software\\HitodeRegTest\\V5]"),
        "HKCU key missing from user.reg"
    );

    // 2. Batch script using reg.exe, %~dp0, UAC boilerplate, pause, and WOW6432Node.
    let bat_reg_add = game.join("注册表恢复_add.bat");
    let bat_text = concat!(
        "@echo off\r\n",
        "%1 mshta vbscript:CreateObject(\"Shell.Application\").ShellExecute(\"cmd.exe\",\"/c %~s0 ::\",\"\",\"runas\",1)(window.close)&&exit\r\n",
        ">nul 2>&1 \"%SYSTEMROOT%\\system32\\cacls.exe\" \"%SYSTEMROOT%\\system32\\config\\system\"\r\n",
        "if '%errorlevel%' NEQ '0' (goto UACPrompt) else (goto gotAdmin)\r\n",
        ":UACPrompt\r\n",
        "exit /b 1\r\n",
        ":gotAdmin\r\n",
        "reg add \"HKLM\\SOFTWARE\\WOW6432Node\\HitodeRegTest\\BatAdd\" /v \"InstallDir\" /t REG_SZ /d \"%~dp0\\\" /f\r\n",
        "reg add \"HKCU\\Software\\HitodeRegTest\\BatAdd\" /v \"Ready\" /t REG_DWORD /d 1 /f\r\n",
        "pause\r\n",
    );
    let codec = Codec::new(936);
    let bat_units: Vec<u16> = bat_text.encode_utf16().collect();
    std::fs::write(&bat_reg_add, codec.from_utf16(936, &bat_units).0).unwrap();

    let (report, stop, text) = run_script(&mut host, &config, &bat_reg_add, b"");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(report.entries >= 2, "expected >=2 entries, got {report:?}");
    assert!(
        report.normalized_wow64 >= 1,
        "expected WOW6432Node mirror, got {report:?}"
    );

    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    let user_reg = std::fs::read_to_string(prefix.join("user.reg")).unwrap();
    assert!(
        system_reg.contains(r"[Software\\HitodeRegTest\\BatAdd]"),
        "mirrored BatAdd key missing from system.reg"
    );
    assert!(
        system_reg.contains(r#""InstallDir"="D:\\""#), // privacy: allow authored guest drive path
        "expected guest drive root from %~dp0 in system.reg"
    );
    assert!(user_reg.contains(r"[Software\\HitodeRegTest\\BatAdd]"));

    // 3. Batch script generating a temporary .reg with %cd:\=\\% and running regedit /s.
    let bat_regedit = game.join("注册表恢复_regedit.bat");
    let regedit_bat = concat!(
        "@echo off\r\n",
        "cd /d \"%~dp0\"\r\n",
        "set \"p=%cd:\\=\\\\%\"\r\n",
        "echo Windows Registry Editor Version 5.00>tmp_restore.reg\r\n",
        "echo.>>tmp_restore.reg\r\n",
        "echo [HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\HitodeRegTest\\BatRegedit]>>tmp_restore.reg\r\n",
        "echo \"Dir\"=\"%p%\">>tmp_restore.reg\r\n",
        "regedit /s tmp_restore.reg\r\n",
        "del tmp_restore.reg\r\n",
        "pause >nul\r\n",
    );
    let units: Vec<u16> = regedit_bat.encode_utf16().collect();
    std::fs::write(&bat_regedit, codec.from_utf16(936, &units).0).unwrap();

    let (report, stop, text) = run_script(&mut host, &config, &bat_regedit, b"");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(report.entries >= 1, "expected >=1 entry, got {report:?}");
    assert!(
        report.normalized_wow64 >= 1,
        "expected WOW6432Node mirror, got {report:?}"
    );
    assert!(
        !game.join("tmp_restore.reg").exists(),
        "batch cleanup should have deleted tmp_restore.reg"
    );

    let system_reg = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system_reg.contains(r"[Software\\HitodeRegTest\\BatRegedit]"),
        "mirrored BatRegedit key missing from system.reg"
    );
    assert!(
        system_reg.contains(r#""Dir"="D:\\""#), // privacy: allow authored guest drive path
        "expected guest drive root from %cd% in system.reg"
    );

    // 4. An interactive script answered through its console input, from a
    // folder of its own.
    let tools = root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let menu = tools.join("menu.bat");
    std::fs::write(
        &menu,
        concat!(
            "@echo off\r\n",
            "set /p name=Name: \r\n",
            "echo got=%name% in %~dp0\r\n",
            "choice /c yn /m Continue\r\n",
            "echo choice=%errorlevel%\r\n",
            ":menu\r\n",
            "set /p pick=Pick: \r\n",
            "if \"%pick%\"==\"2\" goto done\r\n",
            "echo again=%pick%\r\n",
            "goto menu\r\n",
            ":done\r\n",
            "reg add \"HKCU\\Software\\HitodeRegTest\\Menu\" /v \"Name\" /d \"%name%\" /f >nul\r\n",
            "pause\r\n",
            "echo done\r\n",
        ),
    )
    .unwrap();
    let (report, stop, text) = run_script(&mut host, &config, &menu, b"probe\r\ny1\r\n2\r\n");
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    let folder = r"got=probe in E:\"; // privacy: allow authored guest drive path
    for line in [folder, "choice=1", "again=1", "done"] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    assert!(!text.contains("Press any key"), "{text}");
    assert_eq!(report.entries, 1, "{report:?}");
    let user_reg = std::fs::read_to_string(prefix.join("user.reg")).unwrap();
    assert!(user_reg.contains(r#""Name"="probe""#), "{user_reg}");

    let _ = std::fs::remove_dir_all(&root);
}
