//! Callable picker failures. No fabricated cancellation, PIDL or file metadata.
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, win32};
use adv_machine::Env;
use std::collections::BTreeMap;

#[derive(Default)]
struct Errors(BTreeMap<u32, (u32, u32)>);

fn unavailable(env: &mut Env<'_>, api: &str, file_dialog: bool) -> u32 {
    if file_dialog {
        if env.ext.try_get::<Errors>().is_none() {
            env.ext.insert(Errors::default());
        }
        // Context slots are reused; a retained diagnostic belongs to its TID,
        // never to a subsequent thread occupying the same bounded slot.
        let identity = (env.context, env.ctx().tid);
        env.ext
            .get_mut::<Errors>()
            .0
            .insert(identity.0, (identity.1, 2)); // CDERR_INITIALIZATION
        crate::util::set_last_error(env, 120);
    } else {
        crate::util::set_last_error(env, 50);
    }
    adv_abi::warn_unavailable(
        env,
        api,
        "guest file/folder picker and shell item metadata backend is unavailable",
    );
    0
}

#[win32(comdlg32, name = "GetOpenFileNameA", level = safe_fail)]
fn open_a(env: &mut Env<'_>, _data: GuestPtr<()>) -> i32 {
    unavailable(env, "comdlg32.dll!GetOpenFileNameA", true) as i32
}
#[win32(comdlg32, name = "GetOpenFileNameW", level = safe_fail)]
fn open_w(env: &mut Env<'_>, _data: GuestPtr<()>) -> i32 {
    unavailable(env, "comdlg32.dll!GetOpenFileNameW", true) as i32
}
#[win32(comdlg32, name = "GetSaveFileNameA", level = safe_fail)]
fn save_a(env: &mut Env<'_>, _data: GuestPtr<()>) -> i32 {
    unavailable(env, "comdlg32.dll!GetSaveFileNameA", true) as i32
}
#[win32(comdlg32, name = "GetSaveFileNameW", level = safe_fail)]
fn save_w(env: &mut Env<'_>, _data: GuestPtr<()>) -> i32 {
    unavailable(env, "comdlg32.dll!GetSaveFileNameW", true) as i32
}
#[win32(comdlg32, name = "CommDlgExtendedError", level = state)]
fn error(env: &mut Env<'_>) -> u32 {
    env.ext
        .try_get::<Errors>()
        .and_then(|errors| errors.0.get(&env.context))
        .filter(|&&(tid, _)| tid == env.ctx().tid)
        .map_or(0, |&(_, error)| error)
}

#[win32(shell32, name = "SHBrowseForFolderA", level = safe_fail)]
fn browse_a(env: &mut Env<'_>, _info: GuestPtr<()>) -> GuestPtr<()> {
    GuestPtr::new(unavailable(env, "shell32.dll!SHBrowseForFolderA", false))
}
#[win32(shell32, name = "SHBrowseForFolderW", level = safe_fail)]
fn browse_w(env: &mut Env<'_>, _info: GuestPtr<()>) -> GuestPtr<()> {
    GuestPtr::new(unavailable(env, "shell32.dll!SHBrowseForFolderW", false))
}
#[win32(shell32, name = "SHGetPathFromIDListA", level = safe_fail)]
fn path_a(env: &mut Env<'_>, _item: GuestPtr<()>, _output: AnsiStr) -> i32 {
    unavailable(env, "shell32.dll!SHGetPathFromIDListA", false) as i32
}
#[win32(shell32, name = "SHGetPathFromIDListW", level = safe_fail)]
fn path_w(env: &mut Env<'_>, _item: GuestPtr<()>, _output: WideStr) -> i32 {
    unavailable(env, "shell32.dll!SHGetPathFromIDListW", false) as i32
}
#[win32(shell32, name = "SHGetFileInfoA", level = safe_fail)]
fn info_a(
    env: &mut Env<'_>,
    _path: AnsiStr,
    _attributes: u32,
    _info: GuestPtr<()>,
    _size: u32,
    _flags: u32,
) -> u32 {
    unavailable(env, "shell32.dll!SHGetFileInfoA", false)
}
#[win32(shell32, name = "SHGetFileInfoW", level = safe_fail)]
fn info_w(
    env: &mut Env<'_>,
    _path: WideStr,
    _attributes: u32,
    _info: GuestPtr<()>,
    _size: u32,
    _flags: u32,
) -> u32 {
    unavailable(env, "shell32.dll!SHGetFileInfoW", false)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        open_a_ABI,
        open_w_ABI,
        save_a_ABI,
        save_w_ABI,
        error_ABI,
        browse_a_ABI,
        browse_w_ABI,
        path_a_ABI,
        path_w_ABI,
        info_a_ABI,
        info_w_ABI,
    ] {
        registry.register(api);
    }
}
