//! Installer state for copied game folders. A Silky's Plus installer leaves
//! `<exe>_inst.xml` beside the game as a template and, per user, fills a copy
//! in `%APPDATA%\SilkysPlus\<exe>` and copies the `save` folder next to it.
//! A folder copied from an installed machine carries only the template, so
//! the first run completes those two steps as the installer would have.

use super::{ci_find, ci_path};
use crate::OsConfig;
use std::io;

pub(super) fn complete(cfg: &OsConfig) -> io::Result<()> {
    let Some(stem) = cfg
        .exe_name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map(|(stem, _)| stem)
    else {
        return Ok(());
    };
    let name = format!("{stem}_inst.xml");
    let Some(template) = ci_find(&cfg.game_dir, std::slice::from_ref(&name)) else {
        return Ok(());
    };
    let template = std::fs::read(template)?;
    let Some(filled) = fill(cfg, &template) else {
        return Ok(());
    };
    let profile = ci_path(
        &cfg.system_dir,
        &[
            "Users".into(),
            cfg.user_name.clone(),
            "AppData".into(),
            "Roaming".into(),
            "SilkysPlus".into(),
            stem.into(),
        ],
    );
    let state = ci_path(&profile, &[name]);
    if state.exists() {
        return Ok(());
    }
    // Portable saves already map the profile's save folder onto the game's.
    if !cfg.write_game
        && let Some(save) = ci_find(&cfg.game_dir, &["save".into()])
        && save.is_dir()
    {
        super::portable_saves::copy_missing(&save, &ci_path(&profile, &["Save".into()]))?;
    }
    // Written last: its presence marks the installation complete.
    super::host::create_dir_all(&profile)?;
    super::host::write(&state, &filled)
}

/// The template with its empty install path and executable filled in.
fn fill(cfg: &OsConfig, template: &[u8]) -> Option<Vec<u8>> {
    let empty = |tag: &str| {
        let element = format!("<{tag}></{tag}>").into_bytes();
        template.windows(element.len()).position(|w| w == element)
    };
    let (path, exe) = (empty("InstallPath")?, empty("RunExe")?);
    let codec = crate::Codec::new(cfg.codepage);
    let root = cfg.guest_root.trim_end_matches('\\');
    let mut filled = Vec::with_capacity(template.len() + 64);
    let mut at = 0;
    for (start, tag, value) in [
        (path, "InstallPath", root.to_owned()),
        (exe, "RunExe", format!("{root}\\{}", cfg.exe_name)),
    ] {
        let open = tag.len() + 2;
        filled.extend_from_slice(&template[at..start + open]);
        filled.extend_from_slice(&codec.encode(&value));
        at = start + open;
    }
    filled.extend_from_slice(&template[at..]);
    (path < exe).then_some(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "<?xml version=\"1.0\" encoding=\"Shift_JIS\" ?>\r\n<InstallSetting>\r\n\t<InstallPath></InstallPath>\r\n\t<RunExe></RunExe>\r\n</InstallSetting>\r\n";

    fn config(root: &std::path::Path) -> OsConfig {
        let game = root.join("game");
        std::fs::create_dir_all(game.join("save")).unwrap();
        std::fs::write(game.join("apeiria_inst.xml"), TEMPLATE).unwrap();
        std::fs::write(game.join("save").join("onemes.dat"), b"template").unwrap();
        OsConfig::new(game, "apeiria.exe", root.join("data"))
    }

    fn profile(cfg: &OsConfig) -> std::path::PathBuf {
        cfg.data_dir
            .join("c/Users/Player/AppData/Roaming/SilkysPlus/apeiria")
    }

    // spec: HOST-SAVE-5
    #[test]
    fn a_copied_folder_gets_the_installers_state_once() {
        let root = std::env::temp_dir().join(format!("adv-installer-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cfg = config(&root);
        complete(&cfg).unwrap();
        let state = std::fs::read_to_string(profile(&cfg).join("apeiria_inst.xml")).unwrap();
        assert!(
            state.contains("<InstallPath>C:\\Game</InstallPath>"),
            "{state}"
        );
        assert!(
            state.contains("<RunExe>C:\\Game\\apeiria.exe</RunExe>"),
            "{state}"
        );
        let save = profile(&cfg).join("Save").join("onemes.dat");
        assert_eq!(std::fs::read(&save).unwrap(), b"template");
        // Later runs keep the player's progress.
        std::fs::write(&save, b"progress").unwrap();
        complete(&cfg).unwrap();
        assert_eq!(std::fs::read(&save).unwrap(), b"progress");
        std::fs::remove_dir_all(&root).unwrap();
    }

    // spec: HOST-SAVE-5
    #[test]
    fn other_layouts_are_left_alone() {
        let root = std::env::temp_dir().join(format!("adv-installer-other-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cfg = config(&root);
        std::fs::write(cfg.game_dir.join("apeiria_inst.xml"), "<?xml ?>").unwrap();
        complete(&cfg).unwrap();
        assert!(!profile(&cfg).exists());
        let mut portable = config(&root);
        std::fs::write(portable.game_dir.join("apeiria_inst.xml"), TEMPLATE).unwrap();
        portable.write_game = true;
        complete(&portable).unwrap();
        assert!(profile(&portable).join("apeiria_inst.xml").exists());
        assert!(
            !profile(&portable).join("Save").exists(),
            "the game's save folder serves"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
