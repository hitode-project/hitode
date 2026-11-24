//! On-demand installation of Windows redistributable DLLs that Wine does not
//! ship (such as Microsoft's MFC runtime).
//!
//! Installers are fetched directly from Microsoft over HTTPS (or chosen from a
//! local file) and only the missing non-Wine DLLs are extracted into the shared
//! prefix's `system32` on the host. Leaving Wine's own CRT symlinks untouched
//! keeps native-body identity bindings intact and avoids running heavy Windows
//! bootstrapper installers across macOS, iOS and Android.
use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{
    DecompressorOxide, decompress, inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_DLL_BYTES: usize = 32 * 1024 * 1024;
const MAX_CAB_DEPTH: usize = 3;
const CAB_SIGNATURE: &[u8; 8] = b"MSCF\0\0\0\0";

const ALLOWED_HOSTS: &[&str] = &[
    "aka.ms",
    "download.visualstudio.microsoft.com",
    "download.microsoft.com",
    "visualstudio.microsoft.com",
];

/// A redistributable runtime component that supplies DLLs missing from Wine.
#[derive(Clone, Copy, Debug)]
pub struct Component {
    pub id: &'static str,
    pub url: &'static str,
    /// Lowercase DLL that must be present in an extracted bundle.
    pub required: &'static str,
    /// Lowercase filename prefixes extracted from installer cabinets.
    /// Trailing `_x86` suffixes inside Microsoft's VC++ cabinets are stripped.
    pub extract_prefixes: &'static [&'static str],
}

const COMPONENTS: &[Component] = &[Component {
    id: "vc140-mfc",
    url: "https://aka.ms/vs/17/release/vc_redist.x86.exe",
    required: "mfc140u.dll",
    extract_prefixes: &["mfc140", "mfcm140"],
}];

pub fn component(id: &str) -> Option<&'static Component> {
    COMPONENTS.iter().find(|entry| entry.id == id)
}

fn allowed_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| ALLOWED_HOSTS.contains(&host))
}

fn download_client() -> Result<reqwest::blocking::Client, String> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        if !allowed_url(attempt.url()) {
            return attempt.error("redirected outside Microsoft download hosts");
        }
        attempt.follow()
    });
    let builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("Hitode/", env!("CARGO_PKG_VERSION")))
        .redirect(policy);
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(crate::vndb::system_certificates()?);
    builder.build().map_err(|e| e.to_string())
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    if !allowed_url(&parsed) {
        return Err("unsupported download host".into());
    }
    let response = download_client()?
        .get(parsed)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if !allowed_url(response.url()) {
        return Err("download ended outside Microsoft hosts".into());
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_BUNDLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err("downloaded installer is unexpectedly large".into());
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeArch {
    X86,
    X64,
    Other,
    NotPe,
}

fn pe_arch(bytes: &[u8]) -> PeArch {
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return PeArch::NotPe;
    }
    let pe_offset = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    if pe_offset
        .checked_add(24)
        .is_none_or(|end| end > bytes.len())
    {
        return PeArch::NotPe;
    }
    if &bytes[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return PeArch::NotPe;
    }
    match u16::from_le_bytes(bytes[pe_offset + 4..pe_offset + 6].try_into().unwrap()) {
        0x014c => PeArch::X86,
        0x8664 => PeArch::X64,
        _ => PeArch::Other,
    }
}

/// Normalizes an archive or file name into a safe lowercase `system32` DLL
/// name when it belongs to `component`. Rejects x64/arm64 variants and paths.
fn matching_dll_name(raw: &str, component: &Component) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next()?;
    let lower = base.to_ascii_lowercase();
    if lower.ends_with("_x64") || lower.ends_with("_arm64") {
        return None;
    }
    let normalized = lower.strip_suffix("_x86").unwrap_or(&lower);
    if !normalized.ends_with(".dll") {
        return None;
    }
    if normalized.contains("..")
        || !normalized
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return None;
    }
    if !component
        .extract_prefixes
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return None;
    }
    Some(normalized.to_owned())
}

// --- Minimal Microsoft Cabinet (NONE + MSZIP) reader ---

struct CabFolder {
    data_offset: usize,
    data_count: usize,
    compression: u16,
}

struct CabFile {
    name: String,
    folder: usize,
    offset: usize,
    size: usize,
}

struct Cabinet<'a> {
    data: &'a [u8],
    data_reserve: usize,
    folders: Vec<CabFolder>,
    files: Vec<CabFile>,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn skip_cstr(bytes: &[u8], offset: usize) -> Option<usize> {
    let rel = bytes.get(offset..)?.iter().position(|&b| b == 0)?;
    offset.checked_add(rel + 1)
}

impl<'a> Cabinet<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 36 || &data[..8] != CAB_SIGNATURE {
            return None;
        }
        let cab_size = read_u32(data, 8)? as usize;
        if cab_size < 36 || cab_size > data.len() {
            return None;
        }
        let data = &data[..cab_size];
        let files_offset = read_u32(data, 16)? as usize;
        let folder_count = read_u16(data, 26)? as usize;
        let file_count = read_u16(data, 28)? as usize;
        let flags = read_u16(data, 30)?;

        let mut cursor = 36usize;
        let mut folder_reserve = 0usize;
        let mut data_reserve = 0usize;
        if flags & 0x0004 != 0 {
            let header_reserve = read_u16(data, cursor)? as usize;
            folder_reserve = usize::from(*data.get(cursor + 2)?);
            data_reserve = usize::from(*data.get(cursor + 3)?);
            cursor = cursor.checked_add(4)?.checked_add(header_reserve)?;
        }
        if flags & 0x0001 != 0 {
            cursor = skip_cstr(data, cursor)?;
            cursor = skip_cstr(data, cursor)?;
        }
        if flags & 0x0002 != 0 {
            cursor = skip_cstr(data, cursor)?;
            cursor = skip_cstr(data, cursor)?;
        }

        let mut folders = Vec::with_capacity(folder_count);
        for _ in 0..folder_count {
            let data_offset = read_u32(data, cursor)? as usize;
            let data_count = read_u16(data, cursor + 4)? as usize;
            let compression = read_u16(data, cursor + 6)? & 0x000f;
            cursor = cursor.checked_add(8)?.checked_add(folder_reserve)?;
            folders.push(CabFolder {
                data_offset,
                data_count,
                compression,
            });
        }

        let mut files = Vec::with_capacity(file_count);
        let mut fcursor = files_offset;
        for _ in 0..file_count {
            let size = read_u32(data, fcursor)? as usize;
            let offset = read_u32(data, fcursor + 4)? as usize;
            let folder = read_u16(data, fcursor + 8)? as usize;
            let name_start = fcursor.checked_add(16)?;
            let name_end_incl = skip_cstr(data, name_start)?;
            let name = String::from_utf8_lossy(&data[name_start..name_end_incl - 1]).into_owned();
            fcursor = name_end_incl;
            if folder < folders.len() {
                files.push(CabFile {
                    name,
                    folder,
                    offset,
                    size,
                });
            }
        }

        Some(Self {
            data,
            data_reserve,
            folders,
            files,
        })
    }

    fn decompress_folder(&self, folder_index: usize, limit: usize) -> Result<Vec<u8>, String> {
        let folder = self
            .folders
            .get(folder_index)
            .ok_or("cabinet folder out of range")?;
        let mut cursor = folder.data_offset;
        let mut out = Vec::new();
        let mut window = vec![0u8; 65_536];
        let mut hist_len = 0usize;

        for _ in 0..folder.data_count {
            let cb_data = read_u16(self.data, cursor + 4).ok_or("truncated CFDATA")? as usize;
            let cb_uncomp = read_u16(self.data, cursor + 6).ok_or("truncated CFDATA")? as usize;
            let payload_start = cursor
                .checked_add(8)
                .and_then(|v| v.checked_add(self.data_reserve))
                .ok_or("CFDATA offset overflow")?;
            let payload_end = payload_start
                .checked_add(cb_data)
                .ok_or("CFDATA length overflow")?;
            let payload = self
                .data
                .get(payload_start..payload_end)
                .ok_or("truncated CFDATA payload")?;
            cursor = payload_end;

            if cb_uncomp > 32_768 || out.len().saturating_add(cb_uncomp) > limit {
                return Err("cabinet folder exceeds size limit".into());
            }

            match folder.compression {
                0 => {
                    if payload.len() < cb_uncomp {
                        return Err("truncated uncompressed CAB block".into());
                    }
                    out.extend_from_slice(&payload[..cb_uncomp]);
                }
                1 => {
                    if payload.len() < 2 || &payload[..2] != b"CK" {
                        return Err("invalid MSZIP block header".into());
                    }
                    let mut decompressor = DecompressorOxide::new();
                    let (status, _, written) = decompress(
                        &mut decompressor,
                        &payload[2..],
                        &mut window,
                        hist_len,
                        TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
                    );
                    if status != TINFLStatus::Done || written != cb_uncomp {
                        return Err(format!("MSZIP decompression failed ({status:?})"));
                    }
                    out.extend_from_slice(&window[hist_len..hist_len + written]);
                    let end = hist_len + written;
                    let keep = end.min(32_768);
                    window.copy_within(end - keep..end, 0);
                    hist_len = keep;
                }
                kind => return Err(format!("unsupported CAB compression type {kind}")),
            }
        }
        Ok(out)
    }
}

fn extract_from_bundle(
    data: &[u8],
    component: &Component,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut extracted = BTreeMap::new();
    let mut offset = 0usize;
    while offset + 36 <= data.len() {
        let Some(rel) = data[offset..]
            .windows(CAB_SIGNATURE.len())
            .position(|window| window == CAB_SIGNATURE)
        else {
            break;
        };
        let pos = offset + rel;
        let Some(size) = read_u32(data, pos + 8).map(|n| n as usize) else {
            break;
        };
        if size >= 36 && pos.checked_add(size).is_some_and(|end| end <= data.len()) {
            let slice = &data[pos..pos + size];
            if extract_from_cabinet(slice, component, 0, &mut extracted)? {
                offset = pos + size;
                if extracted.contains_key(component.required) {
                    break;
                }
                continue;
            }
        }
        offset = pos + 4;
    }
    Ok(extracted)
}

fn extract_from_cabinet(
    slice: &[u8],
    component: &Component,
    depth: usize,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> Result<bool, String> {
    let Some(cabinet) = Cabinet::parse(slice) else {
        return Ok(false);
    };

    // Pass 1: decompress only folders that directly contain matching DLLs.
    let mut matching_folders: Vec<usize> = cabinet
        .files
        .iter()
        .filter(|file| matching_dll_name(&file.name, component).is_some())
        .map(|file| file.folder)
        .collect();
    matching_folders.sort_unstable();
    matching_folders.dedup();

    let mut matched_here = false;
    for folder_idx in matching_folders {
        let stream = cabinet.decompress_folder(folder_idx, MAX_BUNDLE_BYTES)?;
        for file in cabinet.files.iter().filter(|f| f.folder == folder_idx) {
            let Some(dll_name) = matching_dll_name(&file.name, component) else {
                continue;
            };
            if file.size > MAX_DLL_BYTES {
                continue;
            }
            let Some(end) = file.offset.checked_add(file.size) else {
                continue;
            };
            let Some(bytes) = stream.get(file.offset..end) else {
                continue;
            };
            if pe_arch(bytes) == PeArch::X86 {
                out.insert(dll_name, bytes.to_vec());
                matched_here = true;
            }
        }
    }

    if matched_here || depth >= MAX_CAB_DEPTH || out.contains_key(component.required) {
        return Ok(true);
    }

    // Pass 2: inspect nested cabinets inside WiX Burn payloads (e.g. a0..a11).
    let mut nested = Vec::new();
    for folder_idx in 0..cabinet.folders.len() {
        let Ok(stream) = cabinet.decompress_folder(folder_idx, MAX_BUNDLE_BYTES) else {
            continue;
        };
        for file in cabinet.files.iter().filter(|f| f.folder == folder_idx) {
            if file.size < 36 || file.size > MAX_BUNDLE_BYTES {
                continue;
            }
            let Some(end) = file.offset.checked_add(file.size) else {
                continue;
            };
            let Some(bytes) = stream.get(file.offset..end) else {
                continue;
            };
            if bytes.starts_with(CAB_SIGNATURE) {
                nested.push(bytes.to_vec());
            }
        }
    }

    // Check inner cabinet headers first so matching payloads extract without
    // decompressing unrelated update cabinets.
    nested.sort_by_key(|buf| {
        let has_match = Cabinet::parse(buf).is_some_and(|inner| {
            inner
                .files
                .iter()
                .any(|entry| matching_dll_name(&entry.name, component).is_some())
        });
        !has_match
    });

    for buf in nested {
        extract_from_cabinet(&buf, component, depth + 1, out)?;
        if out.contains_key(component.required) {
            break;
        }
    }

    Ok(true)
}

fn install_files(
    system32: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(system32).map_err(|e| e.to_string())?;
    let pid = std::process::id();
    let mut installed = Vec::with_capacity(files.len());
    for (name, bytes) in files {
        let target = system32.join(name);
        let temp = system32.join(format!(".{name}.{pid}.tmp"));
        if let Err(error) = std::fs::write(&temp, bytes) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("failed to write {name}: {error}"));
        }
        if let Err(error) = std::fs::rename(&temp, &target) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("failed to install {name}: {error}"));
        }
        installed.push(name.clone());
    }
    Ok(installed)
}

pub fn install_from_bundle(
    component: &Component,
    bundle: &[u8],
    system32: &Path,
) -> Result<Vec<String>, String> {
    let files = extract_from_bundle(bundle, component)?;
    if !files.contains_key(component.required) {
        return Err(format!(
            "installer does not contain 32-bit {}",
            component.required
        ));
    }
    install_files(system32, &files)
}

pub fn import_from_path(
    component: &Component,
    source: &Path,
    system32: &Path,
) -> Result<Vec<String>, String> {
    let meta = std::fs::metadata(source).map_err(|e| e.to_string())?;
    if meta.len() as usize > MAX_BUNDLE_BYTES {
        return Err("selected file is too large".into());
    }
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let is_dll = source
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"));
    if is_dll {
        let raw_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("selected file has no name")?;
        let name = matching_dll_name(raw_name, component).ok_or_else(|| {
            format!(
                "expected 32-bit {} or vc_redist.x86.exe",
                component.required
            )
        })?;
        match pe_arch(&bytes) {
            PeArch::X86 => {}
            PeArch::X64 => {
                return Err(format!(
                    "{raw_name} is 64-bit; choose the 32-bit (x86) DLL or vc_redist.x86.exe"
                ));
            }
            PeArch::Other | PeArch::NotPe => {
                return Err(format!("{raw_name} is not a valid 32-bit Windows DLL"));
            }
        }
        let mut files = BTreeMap::new();
        files.insert(name, bytes);
        return install_files(system32, &files);
    }
    install_from_bundle(component, &bytes, system32)
}

#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
fn system32_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::drive::drive(app)
        .map_err(|e| e.to_string())?
        .join("windows/system32"))
}

/// Downloads Microsoft's official redistributable for `id` and installs only
/// its missing non-Wine DLLs into the shared prefix's `system32`.
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
#[tauri::command]
pub async fn install_runtime_component(
    app: tauri::AppHandle,
    id: String,
) -> Result<Vec<String>, String> {
    use tauri::Manager;
    let component = component(&id).ok_or_else(|| format!("unknown runtime component: {id}"))?;
    let _activity = app.state::<crate::game_storage::Activity>().acquire()?;
    let system32 = system32_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let bundle = download(component.url)?;
        install_from_bundle(component, &bundle, &system32)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Installs a missing runtime component from a local `vc_redist.x86.exe`
/// installer or 32-bit `.dll` file into the shared prefix's `system32`.
#[cfg(any(target_os = "macos", target_os = "android", target_os = "ios"))]
#[tauri::command]
pub async fn import_runtime_component(
    app: tauri::AppHandle,
    id: String,
    path: String,
) -> Result<Vec<String>, String> {
    use tauri::Manager;
    let component = component(&id).ok_or_else(|| format!("unknown runtime component: {id}"))?;
    let _activity = app.state::<crate::game_storage::Activity>().acquire()?;
    let system32 = system32_dir(&app)?;
    let source = std::path::PathBuf::from(path);
    #[cfg(mobile)]
    let files_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("files");
    tauri::async_runtime::spawn_blocking(move || {
        let imported = import_from_path(component, &source, &system32);
        #[cfg(mobile)]
        crate::settings::cleanup_rejected_pick(&files_dir, &source, cfg!(target_os = "ios"));
        imported
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::compress_to_vec;

    fn fake_pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x80];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
        bytes[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    /// Builds a valid MSZIP CAB (with header reserve bytes, as Microsoft's
    /// VC++ cabinets carry) for self-contained tests.
    fn build_mszip_cab(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut folder_stream = Vec::new();
        let mut file_offsets = Vec::with_capacity(entries.len());
        for (_, data) in entries {
            file_offsets.push(folder_stream.len() as u32);
            folder_stream.extend_from_slice(data);
        }

        let mut blocks: Vec<(Vec<u8>, u16)> = Vec::new();
        for chunk in folder_stream.chunks(32_768) {
            let mut payload = b"CK".to_vec();
            payload.extend_from_slice(&compress_to_vec(chunk, 6));
            blocks.push((payload, chunk.len() as u16));
        }
        if blocks.is_empty() {
            let mut payload = b"CK".to_vec();
            payload.extend_from_slice(&compress_to_vec(&[], 6));
            blocks.push((payload, 0));
        }

        let header_reserve = 20u16;
        let coff_files = 36 + 4 + header_reserve as u32 + 8;
        let mut file_table = Vec::new();
        for ((name, data), offset) in entries.iter().zip(file_offsets) {
            file_table.extend_from_slice(&(data.len() as u32).to_le_bytes());
            file_table.extend_from_slice(&offset.to_le_bytes());
            file_table.extend_from_slice(&0u16.to_le_bytes());
            file_table.extend_from_slice(&0u16.to_le_bytes());
            file_table.extend_from_slice(&0u16.to_le_bytes());
            file_table.extend_from_slice(&0u16.to_le_bytes());
            file_table.extend_from_slice(name.as_bytes());
            file_table.push(0);
        }
        let coff_data = coff_files + file_table.len() as u32;
        let mut data_section = Vec::new();
        for (payload, uncomp) in &blocks {
            data_section.extend_from_slice(&0u32.to_le_bytes());
            data_section.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            data_section.extend_from_slice(&uncomp.to_le_bytes());
            data_section.extend_from_slice(payload);
        }
        let total_size = coff_data + data_section.len() as u32;

        let mut cab = Vec::with_capacity(total_size as usize);
        cab.extend_from_slice(CAB_SIGNATURE);
        cab.extend_from_slice(&total_size.to_le_bytes());
        cab.extend_from_slice(&0u32.to_le_bytes());
        cab.extend_from_slice(&coff_files.to_le_bytes());
        cab.extend_from_slice(&0u32.to_le_bytes());
        cab.push(3);
        cab.push(1);
        cab.extend_from_slice(&1u16.to_le_bytes()); // cFolders
        cab.extend_from_slice(&(entries.len() as u16).to_le_bytes()); // cFiles
        cab.extend_from_slice(&0x0004u16.to_le_bytes()); // RESERVE_PRESENT
        cab.extend_from_slice(&0u16.to_le_bytes());
        cab.extend_from_slice(&0u16.to_le_bytes());
        cab.extend_from_slice(&header_reserve.to_le_bytes());
        cab.push(0);
        cab.push(0);
        cab.resize(cab.len() + header_reserve as usize, 0);
        // CFFOLDER
        cab.extend_from_slice(&coff_data.to_le_bytes());
        cab.extend_from_slice(&(blocks.len() as u16).to_le_bytes());
        cab.extend_from_slice(&1u16.to_le_bytes()); // MSZIP
        cab.extend_from_slice(&file_table);
        cab.extend_from_slice(&data_section);
        cab
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hitode-redist-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // spec: HOST-LIB-9
    #[test]
    fn extracts_only_x86_mfc_dlls_from_nested_bundle_without_touching_crt() {
        let vc140 = component("vc140-mfc").unwrap();
        let mfc_u = fake_pe(0x014c);
        let mfc_chs = fake_pe(0x014c);
        let mfc_x64 = fake_pe(0x8664);
        let crt = fake_pe(0x014c);

        let inner_crt =
            build_mszip_cab(&[("vcruntime140.dll_x86", &crt), ("msvcp140.dll_x86", &crt)]);
        let inner_mfc = build_mszip_cab(&[
            ("mfc140u.dll_x86", &mfc_u),
            ("mfc140chs.dll_x86", &mfc_chs),
            ("mfc140u.dll_x64", &mfc_x64),
            ("../escape.dll_x86", &mfc_u),
        ]);
        let outer = build_mszip_cab(&[("a10", &inner_crt), ("a11", &inner_mfc)]);

        let mut bundle = fake_pe(0x014c);
        bundle.extend_from_slice(&outer);

        let root = temp_dir("nested");
        let system32 = root.join("system32");
        let installed = install_from_bundle(vc140, &bundle, &system32).unwrap();

        assert_eq!(installed, vec!["mfc140chs.dll", "mfc140u.dll"]);
        assert_eq!(std::fs::read(system32.join("mfc140u.dll")).unwrap(), mfc_u);
        assert_eq!(
            std::fs::read(system32.join("mfc140chs.dll")).unwrap(),
            mfc_chs
        );
        assert!(!system32.join("vcruntime140.dll").exists());
        assert!(!system32.join("msvcp140.dll").exists());
        assert!(!root.join("escape.dll").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    // spec: HOST-LIB-9
    #[test]
    fn local_dll_import_validates_name_and_architecture() {
        let vc140 = component("vc140-mfc").unwrap();
        let root = temp_dir("local-dll");
        let system32 = root.join("system32");

        let x64_path = root.join("mfc140u.dll");
        std::fs::write(&x64_path, fake_pe(0x8664)).unwrap();
        let err = import_from_path(vc140, &x64_path, &system32).unwrap_err();
        assert!(err.contains("64-bit"), "{err}");

        let wrong_name = root.join("vcruntime140.dll");
        std::fs::write(&wrong_name, fake_pe(0x014c)).unwrap();
        assert!(import_from_path(vc140, &wrong_name, &system32).is_err());

        let x86_path = root.join("MFC140U.DLL");
        let x86_bytes = fake_pe(0x014c);
        std::fs::write(&x86_path, &x86_bytes).unwrap();
        let installed = import_from_path(vc140, &x86_path, &system32).unwrap();
        assert_eq!(installed, vec!["mfc140u.dll"]);
        assert_eq!(
            std::fs::read(system32.join("mfc140u.dll")).unwrap(),
            x86_bytes
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn download_host_allowlist_requires_https_microsoft_hosts() {
        assert!(allowed_url(
            &reqwest::Url::parse("https://aka.ms/vs/17/release/vc_redist.x86.exe").unwrap()
        ));
        assert!(allowed_url(
            &reqwest::Url::parse(
                "https://download.visualstudio.microsoft.com/download/pr/test/VC_redist.x86.exe"
            )
            .unwrap()
        ));
        assert!(!allowed_url(
            &reqwest::Url::parse("http://aka.ms/vs/17/release/vc_redist.x86.exe").unwrap()
        ));
        assert!(!allowed_url(
            &reqwest::Url::parse("https://evil.example/vc_redist.x86.exe").unwrap()
        ));
    }

    #[test]
    fn decompresses_multi_block_mszip_folder_over_32kib() {
        let vc140 = component("vc140-mfc").unwrap();
        let mut big_pe = fake_pe(0x014c);
        // Span two 32 KiB MSZIP blocks to exercise sliding-window carry-over.
        for i in 0..40_000u32 {
            big_pe.push((i.wrapping_mul(31) & 0xff) as u8);
        }
        let cab = build_mszip_cab(&[("mfc140u.dll_x86", &big_pe)]);
        let root = temp_dir("multiblock");
        let system32 = root.join("system32");
        let installed = install_from_bundle(vc140, &cab, &system32).unwrap();
        assert_eq!(installed, vec!["mfc140u.dll"]);
        assert_eq!(std::fs::read(system32.join("mfc140u.dll")).unwrap(), big_pe);
        let _ = std::fs::remove_dir_all(&root);
    }
}
