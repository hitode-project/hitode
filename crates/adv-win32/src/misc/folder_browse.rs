//! Filesystem-only Shell folder selection over the guest VFS and USER dialogs.
//! PIDLs are opaque guest task allocations; no host shell pointers cross the ABI.
use crate::{
    Win32, stdcall,
    user32::{
        dialog::{self, Item, SzOrOrd, Template},
        user,
        window::{WS_DISABLED, WS_VISIBLE, wm},
    },
};
use adv_abi::{AnsiStr, GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_os::{
    Heaps,
    com::{task_alloc, task_free},
};
use std::collections::BTreeMap;

const PROC: &str = "<FolderBrowseDlgProc>";
const MAGIC: u32 = 0x50445641;
#[derive(Clone)]
struct Browse {
    ticket: u32,
    info: u32,
    display: u32,
    wide: bool,
    current: String,
    root: Option<String>,
    callback: u32,
    data: u32,
    page: usize,
    children: Vec<String>,
    enabled: bool,
    ok_text: String,
}
#[derive(Default)]
struct State {
    next: u32,
    pending: BTreeMap<u32, Browse>,
    dialogs: BTreeMap<u32, Browse>,
}
fn read_text(env: &Env<'_>, address: u32, wide: bool) -> Option<String> {
    if address == 0 {
        return Some(String::new());
    }
    if wide {
        String::from_utf16(&WideStr(GuestPtr::new(address)).read(env.cpu, 260).ok()?).ok()
    } else {
        Some(
            env.ext
                .get::<Win32>()
                .codec
                .decode(&AnsiStr(GuestPtr::new(address)).read(env.cpu, 260).ok()?),
        )
    }
}
fn write_text(env: &mut Env<'_>, address: u32, text: &str, wide: bool) -> bool {
    if address == 0 {
        return false;
    }
    let mut bytes = if wide {
        text.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    } else {
        env.ext.get::<Win32>().codec.encode(text)
    };
    if bytes.len() / if wide { 2 } else { 1 } >= 260 {
        return false;
    }
    bytes.extend_from_slice(if wide { &[0, 0] } else { &[0] });
    env.cpu.write(address, &bytes).is_ok()
}
fn pidl(env: &mut Env<'_>, path: &str) -> u32 {
    let text: Vec<u8> = path
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let cb = 6 + text.len();
    if cb > 0xfffe {
        return 0;
    }
    let pointer = task_alloc(env, (cb + 2) as u32);
    if pointer == 0 {
        return 0;
    }
    let bytes: Vec<u8> = (cb as u16)
        .to_le_bytes()
        .into_iter()
        .chain(MAGIC.to_le_bytes())
        .chain(text)
        .chain([0, 0])
        .collect();
    if env.cpu.write(pointer, &bytes).is_err() {
        task_free(env, pointer);
        return 0;
    }
    pointer
}
fn pidl_path(env: &mut Env<'_>, pointer: u32) -> Option<String> {
    let size = env.ext.get_mut::<Heaps>().size(0, pointer)?;
    if !(10..=530).contains(&size) {
        return None;
    }
    let cb = u32::from(env.cpu.read_u16(pointer).ok()?);
    if cb + 2 != size
        || cb % 2 != 0
        || env.cpu.read_u32(pointer + 2).ok()? != MAGIC
        || env.cpu.read_u16(pointer + cb).ok()? != 0
    {
        return None;
    }
    let text = read_text(env, pointer + 6, true)?;
    (!text.is_empty()).then_some(text)
}
fn allowed(env: &Env<'_>, browse: &Browse, path: &str) -> bool {
    let vfs = &env.ext.get::<Win32>().vfs;
    if !vfs.is_dir(path) {
        return false;
    }
    browse.root.as_ref().is_none_or(|root| {
        let p = path.to_lowercase();
        let r = root.to_lowercase();
        p == r || p.starts_with(&format!("{}\\", r.trim_end_matches('\\')))
    })
}
fn caption(env: &mut Env<'_>, hwnd: u32, id: u32, text: String, enabled: bool, visible: bool) {
    let control = dialog::item(user(env), &env.sys.objects, hwnd, id);
    if let Some(w) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &control)
    {
        w.text = text;
        w.style = (w.style & !(WS_DISABLED | WS_VISIBLE))
            | if enabled { 0 } else { WS_DISABLED }
            | if visible { WS_VISIBLE } else { 0 };
    }
}
fn refresh(env: &mut Env<'_>, hwnd: u32) {
    let Some(mut b) = env.ext.get::<State>().dialogs.get(&hwnd).cloned() else {
        return;
    };
    b.children = env
        .ext
        .get::<Win32>()
        .vfs
        .list(&b.current)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.is_dir && e.name != "." && e.name != "..")
        .map(|e| e.name)
        .collect();
    b.page = b.page.min(b.children.len().saturating_sub(1) / 8);
    caption(env, hwnd, 10, b.current.clone(), true, true);
    for index in 0..8 {
        let text = b.children.get(b.page * 8 + index).cloned();
        caption(
            env,
            hwnd,
            100 + index as u32,
            text.clone().unwrap_or_default(),
            text.is_some(),
            text.is_some(),
        );
    }
    caption(env, hwnd, 1, b.ok_text.clone(), b.enabled, true);
    caption(env, hwnd, 4, "Previous".into(), b.page > 0, true);
    caption(
        env,
        hwnd,
        5,
        "Next".into(),
        (b.page + 1) * 8 < b.children.len(),
        true,
    );
    env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
}
fn selected(env: &mut Env<'_>, hwnd: u32) -> Outcome {
    refresh(env, hwnd);
    let b = env.ext.get::<State>().dialogs[&hwnd].clone();
    if b.callback == 0 {
        return Outcome::Return(1);
    }
    let pointer = pidl(env, &b.current);
    if pointer == 0 {
        return Outcome::Return(0);
    }
    Outcome::call(b.callback, vec![hwnd, 2, pointer, b.data], move |env, _| {
        task_free(env, pointer);
        Outcome::Return(1)
    })
}
fn procedure(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if msg == wm::INITDIALOG {
        let Some(b) = env.ext.get_mut::<State>().pending.remove(&lp) else {
            return Outcome::Return(0);
        };
        let (callback, data) = (b.callback, b.data);
        env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
        if callback != 0 {
            return Outcome::call(callback, vec![hwnd, 1, 0, data], move |env, _| {
                selected(env, hwnd)
            });
        }
        return selected(env, hwnd);
    }
    let Some(mut b) = env.ext.get::<State>().dialogs.get(&hwnd).cloned() else {
        return Outcome::Return(0);
    };
    match msg {
        0x466 | 0x467 => {
            // BFFM_SETSELECTIONA/W
            let path = if wp != 0 {
                read_text(env, lp, msg == 0x467)
            } else {
                pidl_path(env, lp)
            };
            if let Some(path) = path {
                let path = env.ext.get::<Win32>().vfs.normalize(&path);
                if allowed(env, &b, &path) {
                    b.current = path;
                    b.page = 0;
                    env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
                    return selected(env, hwnd);
                }
            }
            Outcome::Return(0)
        }
        0x465 => {
            // BFFM_ENABLEOK
            b.enabled = lp != 0;
            env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
            refresh(env, hwnd);
            Outcome::Return(0)
        }
        0x464 | 0x468 => {
            // BFFM_SETSTATUSTEXTA/W
            if let Some(text) = read_text(env, lp, msg == 0x468) {
                caption(env, hwnd, 11, text, true, true);
            }
            Outcome::Return(0)
        }
        0x469 => {
            // BFFM_SETOKTEXT (Unicode)
            if let Some(text) = read_text(env, lp, true) {
                b.ok_text = text;
                env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
                refresh(env, hwnd);
            }
            Outcome::Return(0)
        }
        wm::COMMAND if wp >> 16 == 0 => {
            match wp & 0xffff {
                1 if b.enabled && allowed(env, &b, &b.current) => {
                    let pointer = pidl(env, &b.current);
                    if pointer == 0 {
                        return dialog::end(env, hwnd, 0);
                    }
                    if b.display != 0 {
                        let name = b
                            .current
                            .trim_end_matches('\\')
                            .rsplit('\\')
                            .next()
                            .unwrap_or(&b.current);
                        if !write_text(env, b.display, name, b.wide) {
                            task_free(env, pointer);
                            return dialog::end(env, hwnd, 0);
                        }
                    }
                    let _ = env.cpu.write_u32(b.info + 28, 0); // No icon image list is exposed.
                    dialog::end(env, hwnd, pointer)
                }
                2 => dialog::end(env, hwnd, 0),
                id @ (3..=5) => {
                    if id == 3 {
                        if let Some((parent, _)) =
                            b.current.trim_end_matches('\\').rsplit_once('\\')
                        {
                            let path = if parent.ends_with(':') {
                                format!("{parent}\\")
                            } else {
                                parent.into()
                            };
                            if allowed(env, &b, &path) {
                                b.current = path;
                                b.page = 0;
                            }
                        }
                    } else if id == 4 {
                        b.page = b.page.saturating_sub(1);
                    } else {
                        b.page += 1;
                    }
                    env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
                    selected(env, hwnd)
                }
                id @ (100..=107) => {
                    if let Some(name) = b.children.get(b.page * 8 + (id - 100) as usize) {
                        let path = format!("{}\\{name}", b.current.trim_end_matches('\\'));
                        if allowed(env, &b, &path) {
                            b.current = path;
                            b.page = 0;
                            env.ext.get_mut::<State>().dialogs.insert(hwnd, b);
                            return selected(env, hwnd);
                        }
                    }
                    Outcome::Return(0)
                }
                _ => Outcome::Return(0),
            }
        }
        _ => Outcome::Return(0),
    }
}
fn template(title: String) -> Template {
    let mut items = Vec::new();
    let mut add = |id, class, text: &str, x, y, cx, cy| {
        items.push(Item {
            style: 0x50010000 | u32::from(id == 1),
            ex_style: 0,
            x,
            y,
            cx,
            cy,
            id,
            class: SzOrOrd::Ord(class),
            title: SzOrOrd::Str(text.into()),
            data: None,
        })
    };
    add(10, 0x82, "", 10, 10, 280, 20);
    for index in 0..8 {
        add(100 + index, 0x80, "", 10, 32 + index as i16 * 16, 280, 14);
    }
    add(11, 0x82, "", 10, 163, 280, 20);
    for (id, text, x) in [
        (3, "Up", 10),
        (4, "Previous", 66),
        (5, "Next", 122),
        (1, "OK", 190),
        (2, "Cancel", 244),
    ] {
        add(id, 0x80, text, x, 190, 50, 14);
    }
    Template {
        style: 0x80c808c0,
        ex_style: 0,
        x: 0,
        y: 0,
        cx: 304,
        cy: 216,
        menu: SzOrOrd::None,
        class: SzOrOrd::None,
        title,
        font: Some((9, "MS Shell Dlg".into())),
        items,
    }
}
fn browse(env: &mut Env<'_>, info: u32, wide: bool) -> Outcome {
    let mut data = [0; 32];
    if env.cpu.read(info, &mut data).is_err() {
        return Outcome::Return(0);
    }
    let word = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
    let flags = word(4);
    // Virtual shell nodes, edit validation and COM filtering are not implemented.
    if flags & !5 != 0 {
        adv_abi::warn_unavailable(
            env,
            "shell32.dll!SHBrowseForFolder",
            "folder picker flags outside the filesystem subset",
        );
        crate::util::set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let root = if word(1) == 0 {
        None
    } else {
        match pidl_path(env, word(1)) {
            Some(p) => Some(p),
            None => {
                crate::util::set_last_error(env, 50);
                return Outcome::Return(0);
            }
        }
    };
    let current = root
        .clone()
        .unwrap_or_else(|| super::special_folder(env.ext.get::<Win32>(), 0).unwrap());
    if root.is_none() {
        if let Some(host) = env.ext.get::<Win32>().vfs.write_path(&current) {
            let _ = std::fs::create_dir_all(host);
        }
    }
    let mut b = Browse {
        ticket: 0,
        info,
        display: word(2),
        wide,
        current,
        root,
        callback: word(5),
        data: word(6),
        page: 0,
        children: Vec::new(),
        enabled: true,
        ok_text: "OK".into(),
    };
    if !allowed(env, &b, &b.current)
        || b.callback != 0 && !env.cpu.accessible(b.callback, 1, Prot::EXEC)
    {
        return Outcome::Return(0);
    }
    let title = read_text(env, word(3), wide).unwrap_or_default();
    let state = env.ext.get_or_default::<State>();
    if state.pending.len() + state.dialogs.len() >= 64 {
        crate::util::set_last_error(env, 8);
        return Outcome::Return(0);
    }
    let Some(ticket) = state.next.checked_add(1) else {
        return Outcome::Return(0);
    };
    state.next = ticket;
    b.ticket = ticket;
    state.pending.insert(ticket, b);
    let proc = match env.sys.thunks.publish(env.cpu, "shell32.dll", PROC) {
        Ok(p) => p,
        Err(e) => {
            env.ext.get_mut::<State>().pending.remove(&ticket);
            return Outcome::Fail(e.to_string());
        }
    };
    dialog::dialog_box_then(
        env,
        0,
        template(title),
        word(0),
        proc,
        ticket,
        true,
        Box::new(move |env, result| {
            env.ext.get_mut::<State>().pending.remove(&ticket);
            // The modal owner has been restored and the dialog destroyed.
            env.ext
                .get_mut::<State>()
                .dialogs
                .retain(|_, b| b.ticket != ticket);
            Outcome::Return(if result == u32::MAX { 0 } else { result })
        }),
    )
}

#[win32(shell32,name="SHBrowseForFolderA",level=state)]
fn browse_a(env: &mut Env<'_>, info: GuestPtr<()>) -> Outcome {
    browse(env, info.addr(), false)
}
#[win32(shell32,name="SHBrowseForFolderW",level=state)]
fn browse_w(env: &mut Env<'_>, info: GuestPtr<()>) -> Outcome {
    browse(env, info.addr(), true)
}
fn path(env: &mut Env<'_>, item: u32, output: u32, wide: bool) -> i32 {
    let Some(text) = pidl_path(env, item) else {
        let _ = write_text(env, output, "", wide);
        return 0;
    };
    i32::from(write_text(env, output, &text, wide))
}
#[win32(shell32,name="SHGetPathFromIDListA",level=state)]
fn path_a(env: &mut Env<'_>, item: GuestPtr<()>, output: AnsiStr) -> i32 {
    path(env, item.addr(), output.0.addr(), false)
}
#[win32(shell32,name="SHGetPathFromIDListW",level=state)]
fn path_w(env: &mut Env<'_>, item: GuestPtr<()>, output: WideStr) -> i32 {
    path(env, item.addr(), output.0.addr(), true)
}
fn folder_location(env: &mut Env<'_>, csidl: u32, token: u32, output: u32) -> i32 {
    if output == 0 || env.cpu.write_u32(output, 0).is_err() {
        return 0x8000_4003u32 as i32; // E_POINTER
    }
    if token != 0 {
        adv_abi::warn_unavailable(
            env,
            "SHGetFolderLocation",
            "folder lookup for another user token is unsupported",
        );
        return 0x8000_4001u32 as i32;
    }
    let Some(text) = super::special_folder(env.ext.get::<Win32>(), csidl) else {
        if csidl & 0xff <= 0x3d {
            adv_abi::warn_unavailable(
                env,
                "SHGetFolderLocation",
                "this Shell folder has no guest filesystem mapping",
            );
            return 0x8000_4001u32 as i32;
        }
        return 0x8007_0057u32 as i32; // E_INVALIDARG
    };
    // Provision the same virtual profile folders as the existing path APIs.
    // These are filesystem PIDLs owned by the shared COM task allocator.
    let Some(host) = env.ext.get::<Win32>().vfs.write_path(&text) else {
        return 0x8007_0003u32 as i32;
    };
    if let Err(error) = std::fs::create_dir_all(host) {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => 3,
            std::io::ErrorKind::PermissionDenied => 5,
            std::io::ErrorKind::AlreadyExists => 183,
            _ => 31,
        };
        return (0x8007_0000u32 | code) as i32;
    }
    let pointer = pidl(env, &text);
    if pointer == 0 {
        return 0x8007_000eu32 as i32;
    }
    if env.cpu.write_u32(output, pointer).is_err() {
        task_free(env, pointer);
        return 0x8000_4003u32 as i32;
    }
    0
}
#[win32(shell32,name="SHGetSpecialFolderLocation",level=state)]
fn special_location(env: &mut Env<'_>, _owner: Handle, csidl: i32, output: GuestPtr<u32>) -> i32 {
    folder_location(env, csidl as u32, 0, output.addr())
}
#[win32(shell32,name="SHGetFolderLocation",level=state)]
fn location(
    env: &mut Env<'_>,
    _owner: Handle,
    csidl: i32,
    token: Handle,
    _flags: u32,
    output: GuestPtr<u32>,
) -> i32 {
    // Native Shell accepts reserved dwFlags values without changing the result.
    folder_location(env, csidl as u32, token.0, output.addr())
}
#[win32(shell32,name="ILFree",level=state)]
fn free_pidl(env: &mut Env<'_>, item: GuestPtr<()>) {
    task_free(env, item.addr());
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        browse_a_ABI,
        browse_w_ABI,
        path_a_ABI,
        path_w_ABI,
        special_location_ABI,
        location_ABI,
        free_pidl_ABI,
    ] {
        reg.register(api);
    }
    stdcall(reg, "shell32.dll", PROC, 4, procedure);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture, env};
    #[test]
    // spec: WIN32-API-1
    fn special_folder_locations_share_paths_task_allocations_and_failure_outputs() {
        let mut f = Fixture::new("shell-folder-location");
        for csidl in [
            0x00, 0x05, 0x1a, 0x1c, 0x23, 0x24, 0x25, 0x26, 0x28, 0x2e, 0x401c, 0x801c, 0x105,
        ] {
            for extended in [false, true] {
                let status = if extended {
                    f.call(
                        "shell32.dll",
                        "SHGetFolderLocation",
                        &[0, csidl, 0, 2, DATA],
                    )
                } else {
                    f.call(
                        "shell32.dll",
                        "SHGetSpecialFolderLocation",
                        &[0, csidl, DATA],
                    )
                };
                assert_eq!(status, 0);
                let pointer = f.m.cpu.read_u32(DATA).unwrap();
                assert_ne!(pointer, 0);
                let expected = super::super::special_folder(f.m.ext.get::<Win32>(), csidl).unwrap();
                for (api, wide) in [
                    ("SHGetPathFromIDListA", false),
                    ("SHGetPathFromIDListW", true),
                ] {
                    assert_eq!(f.call("shell32.dll", api, &[pointer, DATA + 16]), 1);
                    assert_eq!(
                        read_text(&env(&mut f.m), DATA + 16, wide).as_deref(),
                        Some(expected.as_str())
                    );
                }
                if extended {
                    f.call("shell32.dll", "ILFree", &[pointer]);
                } else {
                    f.call("ole32.dll", "CoTaskMemFree", &[pointer]);
                }
                assert!(f.m.ext.get_mut::<Heaps>().size(0, pointer).is_none());
            }
        }
        f.m.cpu.write_u32(DATA, 1).unwrap();
        assert_eq!(
            f.call(
                "shell32.dll",
                "SHGetSpecialFolderLocation",
                &[0, 0x7f, DATA]
            ),
            0x8007_0057
        );
        assert_eq!(f.m.cpu.read_u32(DATA).unwrap(), 0);
        assert_eq!(
            f.call("shell32.dll", "SHGetSpecialFolderLocation", &[0, 0x1c, 0]),
            0x8000_4003
        );
        assert_eq!(
            f.call(
                "shell32.dll",
                "SHGetFolderLocation",
                &[0, 0x1c, 123, 0, DATA]
            ),
            0x8000_4001
        );
        assert_eq!(f.m.cpu.read_u32(DATA).unwrap(), 0);
        f.call("shell32.dll", "ILFree", &[0]);
        f.cleanup();
    }
    #[test]
    // spec: WIN32-OBJECT-6
    fn folder_pidls_share_task_ownership_and_convert_ansi_and_unicode_paths() {
        let mut f = Fixture::new("shell-pidl");
        let text = "C:\\Game\\百合";
        let pointer = pidl(&mut env(&mut f.m), text);
        assert_ne!(pointer, 0);
        assert_eq!(
            f.call("shell32.dll", "SHGetPathFromIDListW", &[pointer, DATA]),
            1
        );
        assert_eq!(read_text(&env(&mut f.m), DATA, true).as_deref(), Some(text));
        assert_eq!(
            f.call("shell32.dll", "SHGetPathFromIDListA", &[pointer, DATA]),
            1
        );
        assert_eq!(
            read_text(&env(&mut f.m), DATA, false).as_deref(),
            Some(text)
        );
        f.call("ole32.dll", "CoTaskMemFree", &[pointer]);
        assert_eq!(
            f.call("shell32.dll", "SHGetPathFromIDListW", &[pointer, DATA]),
            0
        );
        assert_eq!(f.m.cpu.read_u16(DATA).unwrap(), 0);
        assert_eq!(
            f.call("shell32.dll", "SHGetPathFromIDListA", &[0xdeadbeef, DATA]),
            0
        );
        f.cleanup();
    }
}
