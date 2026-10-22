//! `--survey`: the static gaps of every module in the game folder, written
//! before any guest code runs (PERS-EXPORT-9).
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use adv_runtime::survey::{Gaps, Module, analyze};

use crate::json;

/// Installers, uninstallers and their helper libraries ship beside games
/// but are not them.
const INSTALLERS: &[&str] = &["uninst", "install", "setup", "gameux", "_dl.exe"];
/// Patcher executables and the folders patches and backups sit in; a game's
/// own libraries may carry these words.
const PATCHERS: &[&str] = &["patch", "补丁", "backup"];
/// Executables, DLLs and the plugin and codec extensions engines load.
const EXTENSIONS: &[&str] = &[".exe", ".dll", ".tpm", ".spi", ".ax", ".ocx", ".acm"];

/// Every x86 module under the folder of `exe`, by path relative to it.
pub(crate) fn modules(exe: &Path) -> Vec<Module> {
    let root = exe.parent().unwrap_or(Path::new("."));
    let mut modules = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let installer = INSTALLERS.iter().any(|word| name.contains(word));
            let patcher = PATCHERS.iter().any(|word| name.contains(word));
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if depth < 4 && !installer && !patcher {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            if !EXTENSIONS.iter().any(|ext| name.ends_with(ext))
                || installer
                || (patcher && name.ends_with(".exe"))
                || entry.metadata().map_or(true, |m| m.len() > 64 << 20)
            {
                continue;
            }
            if let Some(manifest) = std::fs::read(&path).ok().and_then(|bytes| analyze(&bytes)) {
                let file = path.strip_prefix(root).unwrap_or(&path);
                modules.push(Module {
                    file: file.to_string_lossy().replace('\\', "/"),
                    manifest,
                });
            }
        }
    }
    modules.sort_by(|a, b| a.file.cmp(&b.file));
    modules
}

/// Writes the survey JSON and returns its one-line summary.
pub(crate) fn write(path: &Path, modules: &[Module], gaps: &Gaps) -> Result<String, String> {
    let list = |items: &BTreeSet<String>| {
        items
            .iter()
            .map(|item| json(item))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let map = |gaps: &BTreeMap<String, BTreeSet<String>>| {
        let entries = gaps
            .iter()
            .map(|(item, files)| format!("\n    {}: [{}]", json(item), list(files)))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{{entries}}}")
    };
    let files = modules.iter().map(|m| json(&m.file)).collect::<Vec<_>>();
    let text = format!(
        concat!(
            "{{\n  \"modules\": [{}],\n  \"lookup_sites\": {},\n  \"open_lookup_sites\": {},\n",
            "  \"missing_modules\": {},\n  \"missing_imports\": {},\n  \"absent_loads\": {},\n",
            "  \"unavailable_lookups\": {},\n  \"undeclared_lookups\": {},\n",
            "  \"missing_classes\": {}\n}}\n"
        ),
        files.join(", "),
        gaps.sites,
        gaps.open_sites,
        map(&gaps.missing_modules),
        map(&gaps.missing_imports),
        map(&gaps.absent_loads),
        map(&gaps.unavailable_lookups),
        map(&gaps.undeclared_lookups),
        map(&gaps.missing_classes),
    );
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text).map_err(|e| format!("survey {}: {e}", path.display()))?;
    Ok(format!(
        "survey: modules={} missing_modules={} missing_imports={} absent_loads={} unavailable_lookups={} undeclared_lookups={} missing_classes={} open_lookup_sites={}/{}",
        modules.len(),
        gaps.missing_modules.len(),
        gaps.missing_imports.len(),
        gaps.absent_loads.len(),
        gaps.unavailable_lookups.len(),
        gaps.undeclared_lookups.len(),
        gaps.missing_classes.len(),
        gaps.open_sites,
        gaps.sites,
    ))
}
