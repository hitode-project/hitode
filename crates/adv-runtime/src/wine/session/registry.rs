//! Registry restoration for games that ship `.reg` or `.bat` setup files.
//!
//! `.reg` files are parsed directly and written to the live Wine server through
//! `adv_nt::registry::Client`, normalizing 64-bit `WOW6432Node` paths onto
//! Hitode's 32-bit (`win32`) prefix. `.bat` / `.cmd` scripts run under Wine's
//! own `cmd.exe` in a short-lived session sharing the game's drive layout,
//! optical mounts and code page; common UAC elevation and `pause` steps are
//! neutralized in a temporary replacement copy so the original file stays
//! untouched, and any newly written `WOW6432Node` keys are mirrored to the
//! 32-bit `SOFTWARE` tree afterwards.
use super::*;
use adv_nt::registry::{Client as RegistryClient, TreeSnapshot};
use adv_os::nls::Codec;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;

const CMD_EXE: &str = r"C:\windows\system32\cmd.exe";
const WOW64_ROOT: &str = r"\Registry\Machine\Software\WOW6432Node";
const SOFTWARE_ROOT: &str = r"\Registry\Machine\Software";
/// How long a registry batch script may run before timing out.
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);

/// Summary of a completed registry restore operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryReport {
    /// Registry values or keys written, deleted, or observed as changed.
    pub entries: usize,
    /// Entries normalized or mirrored from 64-bit `WOW6432Node` to `SOFTWARE`.
    pub normalized_wow64: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RegAction {
    CreateKey {
        path: String,
        wow64: bool,
    },
    DeleteKey {
        path: String,
    },
    SetValue {
        key: String,
        name: String,
        kind: i32,
        data: Vec<u8>,
        wow64: bool,
    },
    DeleteValue {
        key: String,
        name: String,
    },
}

fn codepage_for_locale(locale: u32) -> u32 {
    match locale {
        0x411 => 932,
        0x804 => 936,
        0x404 => 950,
        _ => 1252,
    }
}

/// Restores registry entries from a `.reg`, `.bat`, or `.cmd` file into the
/// host's shared Wine prefix, using the same drive layout and code page as
/// `config` and `options`.
pub(super) fn restore(
    host: &mut Host,
    config: &Config<'_>,
    options: &SessionOptions,
    file: &Path,
) -> Result<RegistryReport, String> {
    if !file.is_file() {
        return Err(format!("registry file not found: {}", file.display()));
    }
    let extension = file
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "reg" => import_reg(host, config, file),
        "bat" | "cmd" => run_batch(host, config, options, file),
        _ => Err("unsupported registry file (expected .reg or .bat)".into()),
    }
}

fn import_reg(host: &mut Host, config: &Config<'_>, file: &Path) -> Result<RegistryReport, String> {
    let raw = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let codepage = codepage_for_locale(config.locale);
    let actions = parse_reg(&raw, codepage)?;

    host.serve(config, |socket, shared, time| {
        let (mut connection, _) = adv_nt::server::Connection::connect(socket, shared, time)
            .map_err(|e| format!("registry connection: {e}"))?;
        let mut client = RegistryClient::new(&mut connection, shared, time);
        let user_key = client
            .user_key()
            .map_err(|e| format!("resolve user registry hive: {e:?}"))?;

        let mut report = RegistryReport::default();
        let mut keys_with_values = BTreeSet::new();
        for action in &actions {
            if let RegAction::SetValue { key, .. } | RegAction::DeleteValue { key, .. } = action {
                keys_with_values.insert(key.to_ascii_lowercase());
            }
        }

        for action in actions {
            match action {
                RegAction::CreateKey { path, wow64 } => {
                    let Some(nt_path) = resolve_nt_path(&path, &user_key) else {
                        continue;
                    };
                    client
                        .create_key(&nt_path)
                        .map_err(|e| format!("create key {path}: {e:?}"))?;
                    // Count empty keys only; keys with values count their values instead.
                    if !keys_with_values.contains(&path.to_ascii_lowercase()) {
                        report.entries += 1;
                        if wow64 {
                            report.normalized_wow64 += 1;
                        }
                    }
                }
                RegAction::DeleteKey { path } => {
                    let Some(nt_path) = resolve_nt_path(&path, &user_key) else {
                        continue;
                    };
                    if is_protected_root(&nt_path, &user_key) {
                        continue;
                    }
                    if client
                        .delete_key_tree(&nt_path)
                        .map_err(|e| format!("delete key {path}: {e:?}"))?
                    {
                        report.entries += 1;
                    }
                }
                RegAction::SetValue {
                    key,
                    name,
                    kind,
                    data,
                    wow64,
                } => {
                    let Some(nt_path) = resolve_nt_path(&key, &user_key) else {
                        continue;
                    };
                    client
                        .set_value(&nt_path, &name, kind, &data)
                        .map_err(|e| format!("set value {key}\\{name}: {e:?}"))?;
                    report.entries += 1;
                    if wow64 {
                        report.normalized_wow64 += 1;
                    }
                }
                RegAction::DeleteValue { key, name } => {
                    let Some(nt_path) = resolve_nt_path(&key, &user_key) else {
                        continue;
                    };
                    if client
                        .delete_value(&nt_path, &name)
                        .map_err(|e| format!("delete value {key}\\{name}: {e:?}"))?
                    {
                        report.entries += 1;
                    }
                }
            }
        }
        drop(connection);
        Ok(report)
    })
}

fn run_batch(
    host: &mut Host,
    config: &Config<'_>,
    options: &SessionOptions,
    file: &Path,
) -> Result<RegistryReport, String> {
    let exe = config
        .exe
        .ok_or("registry scripts run in a game's folder")?;
    let game_dir = std::path::absolute(exe.parent().ok_or("the program has no directory")?)
        .and_then(|dir| dir.canonicalize())
        .map_err(|e| e.to_string())?;
    let canonical_bat = file.canonicalize().map_err(|e| e.to_string())?;
    let inside_game = canonical_bat
        .strip_prefix(&game_dir)
        .ok()
        .map(Path::to_owned);
    let raw = std::fs::read(&canonical_bat).map_err(|e| format!("{}: {e}", file.display()))?;
    let codepage = codepage_for_locale(config.locale);
    let (prefix, drive) = host.prefix(config)?;
    let directory = files::program_directory(config, options, &prefix)?;
    let mut options = SessionOptions {
        cdroms: options.cdroms.clone(),
        install_dir: options.install_dir.clone(),
        replacements: options.replacements.clone(),
        ..SessionOptions::default()
    };

    struct TempGuard(Option<PathBuf>);
    impl Drop for TempGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let mut _temp_bat = TempGuard(None);
    let guest_bat = if let Some(relative) = inside_game {
        // The guest reads the sanitized copy in place of the script.
        options
            .replacements
            .push((canonical_bat, sanitize_batch(&raw, codepage, None)));
        let relative = relative
            .to_str()
            .ok_or("batch path is not valid Unicode")?
            .replace('/', "\\");
        format!("{directory}{relative}")
    } else {
        let sanitized = sanitize_batch(&raw, codepage, Some(&directory));
        let temp_dir = drive.join("windows/temp");
        std::fs::create_dir_all(&temp_dir).map_err(|e| e.to_string())?;
        let name = format!("hitode-reg-{}.bat", std::process::id());
        let host_temp = temp_dir.join(&name);
        std::fs::write(&host_temp, sanitized).map_err(|e| e.to_string())?;
        _temp_bat = TempGuard(Some(host_temp));
        format!(r"C:\windows\temp\{name}")
    };

    let log_path = prefix.join(format!(".hitode-reg-log-{}", std::process::id()));
    let _log_guard = TempGuard(Some(log_path.clone()));
    let output_write =
        File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    options.program = Some(Program {
        image: Image::Guest(CMD_EXE.into()),
        arguments: format!("/d /c \"{guest_bat}\""),
        // The program directory, as when the script is opened there.
        directory: Some(directory.clone()),
        output: Some(output_write.as_raw_fd()),
    });

    let before = host.serve(config, |socket, shared, time| {
        snapshot_watched(socket, shared, time)
    })?;
    let wake = WakeEvent::new().map_err(|e| e.to_string())?;
    let mut session = host.start(config, wake, &options, |_| Ok(()))?;
    let stop = session.run(Instant::now() + BATCH_TIMEOUT);
    drop(session);
    drop(output_write);

    let mut raw_output = Vec::new();
    if let Ok(file) = File::open(&log_path) {
        let _ = file.take(16 * 1024).read_to_end(&mut raw_output);
    }
    let output_text = decode_text(&raw_output, codepage).trim().to_owned();

    let report = host.serve(config, |socket, shared, time| {
        sync_and_count(socket, shared, time, &before)
    })?;
    match stop {
        Stop::Exited(0) => Ok(report),
        Stop::Exited(_) if report.entries > 0 => Ok(report),
        Stop::Exited(code) => {
            if output_text.is_empty() {
                Err(format!("batch script exited with code {code}"))
            } else {
                let first_line = output_text.lines().next().unwrap_or(&output_text);
                Err(format!(
                    "batch script exited with code {code}: {first_line}"
                ))
            }
        }
        Stop::Paused => {
            if output_text.is_empty() {
                Err("batch script timed out".into())
            } else {
                Err(format!("batch script timed out: {output_text}"))
            }
        }
        Stop::Failed(error) => Err(error),
    }
}

struct WatchedSnapshot {
    wow64: TreeSnapshot,
    hklm_vendor: BTreeMap<String, TreeSnapshot>,
    hkcu_vendor: BTreeMap<String, TreeSnapshot>,
}

const SKIPPED_SOFTWARE_SUBKEYS: &[&str] = &[
    "classes",
    "clients",
    "microsoft",
    "odbc",
    "policies",
    "registeredapplications",
    "wine",
    "wow6432node",
];

fn snapshot_watched(
    socket: &Path,
    shared: &mut Shared,
    time: Time,
) -> Result<WatchedSnapshot, String> {
    let (mut connection, _) = adv_nt::server::Connection::connect(socket, shared, time)
        .map_err(|e| format!("registry snapshot: {e}"))?;
    let mut client = RegistryClient::new(&mut connection, shared, time);
    let user_key = client.user_key().map_err(|e| format!("user key: {e:?}"))?;
    let wow64 = client
        .snapshot(WOW64_ROOT)
        .map_err(|e| format!("snapshot WOW6432Node: {e:?}"))?;
    let hklm_vendor = snapshot_filtered_software(&mut client, SOFTWARE_ROOT)?;
    let hkcu_vendor = snapshot_filtered_software(&mut client, &format!(r"{user_key}\Software"))?;
    drop(connection);
    Ok(WatchedSnapshot {
        wow64,
        hklm_vendor,
        hkcu_vendor,
    })
}

fn snapshot_filtered_software(
    client: &mut RegistryClient<'_>,
    root: &str,
) -> Result<BTreeMap<String, TreeSnapshot>, String> {
    let mut result = BTreeMap::new();
    let subkeys = client
        .subkeys(root)
        .map_err(|e| format!("enum {root}: {e:?}"))?;
    for sub in subkeys {
        if SKIPPED_SOFTWARE_SUBKEYS
            .iter()
            .any(|skip| sub.eq_ignore_ascii_case(skip))
        {
            continue;
        }
        let path = format!("{root}\\{sub}");
        let tree = client
            .snapshot(&path)
            .map_err(|e| format!("snapshot {path}: {e:?}"))?;
        result.insert(sub.to_ascii_lowercase(), tree);
    }
    Ok(result)
}

fn diff_tree_count(before: &TreeSnapshot, after: &TreeSnapshot) -> usize {
    let mut changed = 0usize;
    for (subkey, values) in after {
        let prev = before.get(subkey);
        let mut sub_changed = 0usize;
        for (name, entry) in values {
            if prev.and_then(|p| p.get(name)) != Some(entry) {
                sub_changed += 1;
            }
        }
        if sub_changed > 0 {
            changed += sub_changed;
        } else if prev.is_none() && values.is_empty() {
            changed += 1;
        }
    }
    changed
}

fn diff_vendor_maps(
    before: &BTreeMap<String, TreeSnapshot>,
    after: &BTreeMap<String, TreeSnapshot>,
) -> usize {
    let empty = TreeSnapshot::new();
    let mut changed = 0usize;
    for (vendor, after_tree) in after {
        let before_tree = before.get(vendor).unwrap_or(&empty);
        changed += diff_tree_count(before_tree, after_tree);
    }
    changed
}

fn sync_and_count(
    socket: &Path,
    shared: &mut Shared,
    time: Time,
    before: &WatchedSnapshot,
) -> Result<RegistryReport, String> {
    let (mut connection, _) = adv_nt::server::Connection::connect(socket, shared, time)
        .map_err(|e| format!("registry sync: {e}"))?;
    let mut client = RegistryClient::new(&mut connection, shared, time);
    let user_key = client.user_key().map_err(|e| format!("user key: {e:?}"))?;
    let after_wow64 = client
        .snapshot(WOW64_ROOT)
        .map_err(|e| format!("snapshot WOW6432Node: {e:?}"))?;
    let after_hklm = snapshot_filtered_software(&mut client, SOFTWARE_ROOT)?;
    let after_hkcu = snapshot_filtered_software(&mut client, &format!(r"{user_key}\Software"))?;

    let mut report = RegistryReport::default();
    report.entries += diff_vendor_maps(&before.hklm_vendor, &after_hklm);
    report.entries += diff_vendor_maps(&before.hkcu_vendor, &after_hkcu);

    // Mirror any new or modified WOW6432Node entries onto 32-bit HKLM\SOFTWARE.
    for (subkey, values) in &after_wow64 {
        if subkey.is_empty() {
            continue;
        }
        let target = format!("{SOFTWARE_ROOT}\\{subkey}");
        let prev = before.wow64.get(subkey);
        let mut wrote_any = false;
        for (name, (kind, data)) in values {
            if prev.and_then(|p| p.get(name)) == Some(&(*kind, data.clone())) {
                continue;
            }
            client
                .set_value(&target, name, *kind as i32, data)
                .map_err(|e| format!("mirror WOW6432Node\\{subkey}\\{name}: {e:?}"))?;
            wrote_any = true;
            report.normalized_wow64 += 1;
            // Count toward entries if not already counted via HKLM\Software.
            let top = subkey
                .split('\\')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let already_in_hklm = after_hklm
                .get(&top)
                .and_then(|tree| {
                    let rest = subkey.split_once('\\').map_or("", |(_, r)| r);
                    tree.get(rest)
                })
                .and_then(|vals| vals.get(name))
                == Some(&(*kind, data.clone()));
            if !already_in_hklm {
                report.entries += 1;
            }
        }
        if !wrote_any && prev.is_none() && values.is_empty() {
            client
                .create_key(&target)
                .map_err(|e| format!("mirror WOW6432Node\\{subkey}: {e:?}"))?;
            report.normalized_wow64 += 1;
            report.entries += 1;
        }
    }
    drop(connection);
    Ok(report)
}

fn decode_text(bytes: &[u8], codepage: u32) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_be_bytes(pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let codec = Codec::new(codepage);
    let units = codec.to_utf16(codepage, bytes);
    String::from_utf16_lossy(&units)
}

fn encode_utf16_nul(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().chain([0]) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn parse_reg(bytes: &[u8], codepage: u32) -> Result<Vec<RegAction>, String> {
    let text = decode_text(bytes, codepage);
    let raw_lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut index = 0;
    while index < raw_lines.len() {
        let trimmed = raw_lines[index].trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            index += 1;
            continue;
        }
        break;
    }
    let Some(&header) = raw_lines.get(index) else {
        return Err("empty registry file".into());
    };
    let header_trimmed = header.trim();
    let is_v4 = if header_trimmed.eq_ignore_ascii_case("Windows Registry Editor Version 5.00") {
        false
    } else if header_trimmed.eq_ignore_ascii_case("REGEDIT4") {
        true
    } else {
        return Err("not a Windows registry (.reg) file".into());
    };
    index += 1;

    // Join backslash line continuations used by long hex values.
    let mut lines: Vec<String> = Vec::new();
    while index < raw_lines.len() {
        let mut line = raw_lines[index].to_owned();
        index += 1;
        while !line.trim_start().starts_with('[')
            && !line.trim_start().starts_with(';')
            && line.contains('=')
            && !line.trim_end().ends_with('"')
            && line.trim_end().ends_with('\\')
            && index < raw_lines.len()
        {
            let trimmed_len = line.trim_end().len();
            line.truncate(trimmed_len - 1);
            line.push_str(raw_lines[index].trim_start());
            index += 1;
        }
        lines.push(line);
    }

    let mut actions = Vec::new();
    let mut current_key: Option<(String, bool)> = None;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(removed) = inner.strip_prefix('-') {
                let (normalized, _) = normalize_key_path(removed.trim());
                actions.push(RegAction::DeleteKey { path: normalized });
                current_key = None;
            } else {
                let (normalized, wow64) = normalize_key_path(inner.trim());
                actions.push(RegAction::CreateKey {
                    path: normalized.clone(),
                    wow64,
                });
                current_key = Some((normalized, wow64));
            }
            continue;
        }
        let Some((ref key, wow64)) = current_key else {
            continue;
        };
        let Some((name, rhs)) = split_reg_assignment(trimmed) else {
            continue;
        };
        if rhs == "-" {
            actions.push(RegAction::DeleteValue {
                key: key.clone(),
                name,
            });
            continue;
        }
        let (kind, data) = parse_reg_value(rhs, is_v4, codepage)?;
        actions.push(RegAction::SetValue {
            key: key.clone(),
            name,
            kind,
            data,
            wow64,
        });
    }
    Ok(actions)
}

fn split_reg_assignment(line: &str) -> Option<(String, &str)> {
    if let Some(rest) = line.strip_prefix('@') {
        let rest = rest.trim_start();
        let rhs = rest.strip_prefix('=')?.trim();
        return Some((String::new(), rhs));
    }
    if !line.starts_with('"') {
        return None;
    }
    let (name, rest) = parse_quoted_string(line)?;
    let rhs = rest.trim_start().strip_prefix('=')?.trim();
    Some((name, rhs))
}

fn parse_quoted_string(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    if *bytes.first()? != b'"' {
        return None;
    }
    let mut out = String::new();
    let mut chars = input[1..].char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    out.push(escaped);
                } else {
                    out.push('\\');
                }
            }
            '"' => {
                return Some((out, &input[1 + idx + 1..]));
            }
            other => out.push(other),
        }
    }
    None
}

fn parse_reg_value(rhs: &str, is_v4: bool, codepage: u32) -> Result<(i32, Vec<u8>), String> {
    if rhs.starts_with('"') {
        let (text, _) = parse_quoted_string(rhs)
            .ok_or_else(|| format!("unterminated registry string: {rhs}"))?;
        return Ok((1, encode_utf16_nul(&text)));
    }
    let lower = rhs.to_ascii_lowercase();
    if let Some(hex_digits) = lower.strip_prefix("dword:") {
        let value = u32::from_str_radix(hex_digits.trim(), 16)
            .map_err(|_| format!("invalid dword value: {rhs}"))?;
        return Ok((4, value.to_le_bytes().to_vec()));
    }
    if let Some(hex_body) = lower.strip_prefix("hex:") {
        let bytes = parse_hex_bytes(hex_body)?;
        return Ok((3, bytes));
    }
    if let Some(rest) = lower.strip_prefix("hex(")
        && let Some((type_hex, hex_body)) = rest.split_once("):")
    {
        let kind = u32::from_str_radix(type_hex.trim(), 16)
            .map_err(|_| format!("invalid hex type: {rhs}"))? as i32;
        let bytes = parse_hex_bytes(hex_body)?;
        if is_v4 && matches!(kind, 2 | 7) {
            // REGEDIT4 encodes REG_EXPAND_SZ (2) and REG_MULTI_SZ (7) in ANSI.
            return Ok((kind, ansi_bytes_to_utf16le(&bytes, codepage)));
        }
        return Ok((kind, bytes));
    }
    Err(format!("unsupported registry value: {rhs}"))
}

fn parse_hex_bytes(body: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for part in body.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let byte = u8::from_str_radix(part, 16).map_err(|_| format!("invalid hex byte: {part}"))?;
        out.push(byte);
    }
    Ok(out)
}

fn ansi_bytes_to_utf16le(bytes: &[u8], codepage: u32) -> Vec<u8> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let codec = Codec::new(codepage);
    let mut out = Vec::with_capacity(bytes.len() * 2);
    let segments: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    for (index, segment) in segments.iter().enumerate() {
        if !segment.is_empty() {
            for unit in codec.to_utf16(codepage, segment) {
                out.extend_from_slice(&unit.to_le_bytes());
            }
        }
        if index + 1 < segments.len() {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    out
}

/// Normalizes `HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\<subpath>` onto 32-bit
/// `HKEY_LOCAL_MACHINE\SOFTWARE\<subpath>` for Hitode's `win32` Wine prefix.
fn normalize_key_path(path: &str) -> (String, bool) {
    let parts: Vec<&str> = path.split('\\').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 4
        && (parts[0].eq_ignore_ascii_case("HKEY_LOCAL_MACHINE")
            || parts[0].eq_ignore_ascii_case("HKLM"))
        && parts[1].eq_ignore_ascii_case("SOFTWARE")
        && parts[2].eq_ignore_ascii_case("WOW6432Node")
    {
        let mut normalized = Vec::with_capacity(parts.len() - 1);
        normalized.push("HKEY_LOCAL_MACHINE");
        normalized.push("SOFTWARE");
        normalized.extend_from_slice(&parts[3..]);
        return (normalized.join("\\"), true);
    }
    (parts.join("\\"), false)
}

fn resolve_nt_path(path: &str, user_key: &str) -> Option<String> {
    let (root, rest) = match path.split_once('\\') {
        Some((root, rest)) => (root, Some(rest)),
        None => (path, None),
    };
    let base = if root.eq_ignore_ascii_case("HKEY_LOCAL_MACHINE")
        || root.eq_ignore_ascii_case("HKLM")
    {
        r"\Registry\Machine".to_owned()
    } else if root.eq_ignore_ascii_case("HKEY_CURRENT_USER") || root.eq_ignore_ascii_case("HKCU") {
        user_key.to_owned()
    } else if root.eq_ignore_ascii_case("HKEY_CLASSES_ROOT") || root.eq_ignore_ascii_case("HKCR") {
        r"\Registry\Machine\Software\Classes".to_owned()
    } else if root.eq_ignore_ascii_case("HKEY_USERS") || root.eq_ignore_ascii_case("HKU") {
        r"\Registry\User".to_owned()
    } else if root.eq_ignore_ascii_case("HKEY_CURRENT_CONFIG") || root.eq_ignore_ascii_case("HKCC")
    {
        r"\Registry\Machine\System\CurrentControlSet\Hardware Profiles\Current".to_owned()
    } else {
        return None;
    };
    Some(match rest {
        Some(rest) if !rest.is_empty() => format!("{base}\\{rest}"),
        _ => base,
    })
}

fn is_protected_root(nt_path: &str, user_key: &str) -> bool {
    let lower = nt_path.to_ascii_lowercase();
    let user_lower = user_key.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        r"\registry\machine"
            | r"\registry\machine\software"
            | r"\registry\machine\software\classes"
            | r"\registry\machine\software\wow6432node"
            | r"\registry\machine\system"
            | r"\registry\user"
    ) || lower == user_lower
        || lower == format!(r"{user_lower}\software")
}

/// Neutralizes interactive waits and Windows UAC elevation boilerplate so a
/// game's registry restore batch script can run non-interactively under Wine.
fn sanitize_batch(raw: &[u8], codepage: u32, external_dir: Option<&str>) -> Vec<u8> {
    let text = decode_text(raw, codepage);
    let mut out = String::with_capacity(text.len() + 64);
    for raw_line in text.lines() {
        let mut line = raw_line.trim_end_matches('\r').to_owned();
        if let Some(dir) = external_dir {
            line = replace_batch_self_vars(&line, dir);
        }
        let cleaned = sanitize_batch_line(&line);
        out.push_str(&cleaned);
        out.push_str("\r\n");
    }
    let codec = Codec::new(codepage);
    let units: Vec<u16> = out.encode_utf16().collect();
    codec.from_utf16(codepage, &units).0
}

fn sanitize_batch_line(line: &str) -> String {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();

    // UAC self-elevation one-liners (very common in Chinese game rips).
    if lower.contains("mshta")
        && (lower.contains("vbscript") || lower.contains("shellexecute") || lower.contains("runas"))
    {
        return "rem [hitode] skipped elevation".into();
    }
    if lower.contains("powershell") && (lower.contains("runas") || lower.contains("-verb")) {
        return "rem [hitode] skipped elevation".into();
    }
    if lower.contains("getadmin.vbs") {
        return "rem [hitode] skipped elevation".into();
    }

    // Admin privilege checks whose failure jumps to a UACPrompt label.
    if (lower.contains("cacls.exe") || lower.contains("cacls ")) && lower.contains("config\\system")
        || lower.starts_with("fltmc")
        || lower.contains(" fltmc ")
        || lower.contains("net session")
    {
        return "ver >nul".into();
    }

    // Interactive pauses / waits.
    let without_at = lower.trim_start_matches('@').trim_start();
    if is_command(without_at, "pause") || is_command(without_at, "timeout") {
        return "rem [hitode] skipped pause".into();
    }

    // Strip trailing `& pause` or `&& pause` chained onto a real command.
    if let Some(pos) = find_trailing_pause(line) {
        return line[..pos].trim_end().to_owned();
    }

    line.to_owned()
}

fn is_command(lower_line: &str, name: &str) -> bool {
    match lower_line.strip_prefix(name) {
        None => false,
        Some(rest) => {
            rest.is_empty()
                || rest.starts_with(|c: char| {
                    c.is_ascii_whitespace() || matches!(c, '>' | '<' | '&' | '|')
                })
        }
    }
}

fn find_trailing_pause(line: &str) -> Option<usize> {
    let lower = line.to_ascii_lowercase();
    let idx = lower.rfind('&')?;
    let after = lower[idx + 1..].trim_start();
    if !is_command(after, "pause") {
        return None;
    }
    let start = if idx > 0 && lower.as_bytes()[idx - 1] == b'&' {
        idx - 1
    } else {
        idx
    };
    Some(start)
}

fn replace_batch_self_vars(line: &str, dir: &str) -> String {
    let drive = dir.get(..2).unwrap_or("D:");
    let path = dir.get(2..).unwrap_or("\\");
    let mut result = replace_ascii_case_insensitive(line, "%~dp0", dir);
    result = replace_ascii_case_insensitive(&result, "%~d0", drive);
    result = replace_ascii_case_insensitive(&result, "%~p0", path);
    result
}

fn replace_ascii_case_insensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0;
    while let Some(offset) = lower[cursor..].find(&needle_lower) {
        out.push_str(&haystack[cursor..cursor + offset]);
        out.push_str(replacement);
        cursor += offset + needle.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v5_utf16le_and_normalizes_wow6432node() {
        let text = concat!(
            "Windows Registry Editor Version 5.00\r\n",
            "\r\n",
            "[HKEY_LOCAL_MACHINE\\SOFTWARE\\WOW6432Node\\Vendor\\Game]\r\n",
            "@=\"Default\\\\Path\"\r\n",
            "\"InstallDir\"=\"D:\\\\Game\\\\\"\r\n", // privacy: allow authored guest drive path
            "\"Flag\"=dword:00000001\r\n",
            "\"Blob\"=hex:01,02,\\\r\n",
            "  03,ff\r\n",
            "\"RemoveMe\"=-\r\n",
            "[-HKEY_CURRENT_USER\\Software\\OldVendor]\r\n",
        );
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let actions = parse_reg(&bytes, 936).unwrap();
        assert_eq!(actions.len(), 7);
        assert_eq!(
            actions[0],
            RegAction::CreateKey {
                path: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                wow64: true,
            }
        );
        assert_eq!(
            actions[1],
            RegAction::SetValue {
                key: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                name: String::new(),
                kind: 1,
                data: encode_utf16_nul(r"Default\Path"),
                wow64: true,
            }
        );
        assert_eq!(
            actions[2],
            RegAction::SetValue {
                key: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                name: "InstallDir".into(),
                kind: 1,
                data: encode_utf16_nul(r"D:\Game\"), // privacy: allow authored guest drive path
                wow64: true,
            }
        );
        assert_eq!(
            actions[3],
            RegAction::SetValue {
                key: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                name: "Flag".into(),
                kind: 4,
                data: 1u32.to_le_bytes().to_vec(),
                wow64: true,
            }
        );
        assert_eq!(
            actions[4],
            RegAction::SetValue {
                key: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                name: "Blob".into(),
                kind: 3,
                data: vec![0x01, 0x02, 0x03, 0xff],
                wow64: true,
            }
        );
        assert_eq!(
            actions[5],
            RegAction::DeleteValue {
                key: r"HKEY_LOCAL_MACHINE\SOFTWARE\Vendor\Game".into(),
                name: "RemoveMe".into(),
            }
        );
        assert_eq!(
            actions[6],
            RegAction::DeleteKey {
                path: r"HKEY_CURRENT_USER\Software\OldVendor".into(),
            }
        );
    }

    #[test]
    fn parses_regedit4_gbk_and_converts_ansi_expand_sz() {
        let codec = Codec::new(936);
        let text = concat!(
            "REGEDIT4\r\n",
            "[HKEY_CURRENT_USER\\Software\\测试]\r\n",
            "\"路径\"=\"D:\\\\游戏\"\r\n", // privacy: allow authored guest drive path
            "\"Expand\"=hex(2):44,3a,5c,00\r\n",
        );
        let units: Vec<u16> = text.encode_utf16().collect();
        let (gbk, _) = codec.from_utf16(936, &units);
        let actions = parse_reg(&gbk, 936).unwrap();
        assert_eq!(
            actions[1],
            RegAction::SetValue {
                key: r"HKEY_CURRENT_USER\Software\测试".into(),
                name: "路径".into(),
                kind: 1,
                data: encode_utf16_nul(r"D:\游戏"), // privacy: allow authored guest drive path
                wow64: false,
            }
        );
        assert_eq!(
            actions[2],
            RegAction::SetValue {
                key: r"HKEY_CURRENT_USER\Software\测试".into(),
                name: "Expand".into(),
                kind: 2,
                data: encode_utf16_nul(r"D:\"), // privacy: allow authored guest drive path
                wow64: false,
            }
        );
    }

    #[test]
    fn sanitizes_uac_elevation_and_pause_lines() {
        let script = concat!(
            "@echo off\r\n",
            "%1 mshta vbscript:CreateObject(\"Shell.Application\").ShellExecute(\"cmd.exe\",\"/c %~s0 ::\",\"\",\"runas\",1)(window.close)&&exit\r\n",
            ">nul 2>&1 \"%SYSTEMROOT%\\system32\\cacls.exe\" \"%SYSTEMROOT%\\system32\\config\\system\"\r\n",
            "reg add \"HKLM\\SOFTWARE\\Test\" /v Dir /d \"%~dp0\" /f && pause\r\n",
            "pause >nul\r\n",
        );
        let cleaned = String::from_utf8(sanitize_batch(script.as_bytes(), 1252, None)).unwrap();
        let lines: Vec<&str> = cleaned.lines().collect();
        assert_eq!(lines[0], "@echo off");
        assert!(lines[1].starts_with("rem "));
        assert_eq!(lines[2], "ver >nul");
        assert_eq!(
            lines[3],
            "reg add \"HKLM\\SOFTWARE\\Test\" /v Dir /d \"%~dp0\" /f"
        );
        assert!(lines[4].starts_with("rem "));
    }
}
