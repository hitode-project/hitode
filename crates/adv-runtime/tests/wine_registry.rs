//! Registry restoration via native `.reg` import and Wine `cmd.exe` batch runs.
//!
//! It needs `ADV_WINE_DIST`, as the other Wine tests do.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_os::nls::Codec;
use adv_runtime::wine::session::{Config, Host, SessionOptions};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
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
    let options = SessionOptions::default();
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

    let report = host.restore_registry(&config, &options, &reg_v5).unwrap();
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

    let report = host
        .restore_registry(&config, &options, &bat_reg_add)
        .unwrap();
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

    let report = host
        .restore_registry(&config, &options, &bat_regedit)
        .unwrap();
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

    let _ = std::fs::remove_dir_all(&root);
}
