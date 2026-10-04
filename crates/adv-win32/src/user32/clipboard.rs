//! A guest-local clipboard: common Global objects and ordinary guest callbacks.
//! No native clipboard API is called. Delayed/GDI/private-owner rendering is
//! explicitly unsupported rather than publishing nonexistent data.
use super::{message, user, user_mut};
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, Env, GuestPtr, Handle, Outcome, Registry, WideStr, win32};
use adv_cpu::backend::Prot;
use adv_machine::Identity;
use adv_os::{
    Heaps,
    clipboard::{Data, Open, Viewer},
    global::{Block, Globals},
};
use std::collections::VecDeque;
#[cfg(test)]
mod tests;

const TEXT: u32 = 1;
const OEM: u32 = 7;
const UNICODE: u32 = 13;
const LOCALE: u32 = 16;
const MAX_BYTES: u32 = 16 * 1024 * 1024;
fn fail(env: &mut Env<'_>, code: u32) -> u32 {
    set_last_error(env, code);
    0
}
fn opened(env: &mut Env<'_>) -> bool {
    if user(env)
        .clipboard
        .open
        .is_some_and(|o| o.thread == env.context)
    {
        true
    } else {
        fail(env, 1418);
        false
    }
}
fn live(env: &Env<'_>, hwnd: u32) -> bool {
    user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| !w.destroying)
}
#[win32(user32,name="OpenClipboard",level=state)]
fn open_clipboard(env: &mut Env<'_>, hwnd: Handle) -> u32 {
    if hwnd.0 != 0 && !live(env, hwnd.0) {
        return fail(env, 1400);
    }
    let current = env.context;
    let cb = &mut user_mut(env).clipboard;
    if let Some(open) = cb.open {
        // Reopening the same task/window is idempotent, not a nested lock.
        if open.thread == current && open.window == hwnd.0 {
            return 1;
        }
        return fail(env, 5);
    }
    cb.open = Some(Open {
        thread: current,
        window: hwnd.0,
    });
    1
}
fn retire(env: &mut Env<'_>, id: Identity<Block>) -> bool {
    if env.sys.objects.release(id).is_none() {
        return false;
    }
    if env.sys.objects.strong_count(id) != Some(1) {
        return true;
    }
    let Some(handle) = env.sys.objects.owned(id).map(|b| b.handle) else {
        return false;
    };
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    globals
        .free(
            &mut env.sys.objects,
            heaps,
            env.cpu,
            &mut env.sys.vm,
            handle,
            true,
        )
        .is_ok()
}
#[win32(user32,name="EmptyClipboard",level=implement)]
fn empty_clipboard(env: &mut Env<'_>) -> Outcome {
    if !opened(env) {
        return Outcome::Return(0);
    }
    let (previous, data) = {
        let cb = &mut user_mut(env).clipboard;
        let previous = cb.owner;
        cb.owner = cb.open.unwrap().window;
        cb.touch();
        (previous, std::mem::take(&mut cb.data))
    };
    for entry in data {
        if !retire(env, entry.identity) {
            return Outcome::Fail("clipboard Global reference/heap ownership invariant".into());
        }
    }
    if previous != 0 {
        message::send(env, previous, 0x307, 0, 0, |_, _| Outcome::Return(1))
    } else {
        Outcome::Return(1)
    }
}
/// Publish only after retaining the common object; replacement is transactional.
fn publish(env: &mut Env<'_>, format: u32, id: Identity<Block>, changed: bool) -> bool {
    if user(env)
        .clipboard
        .data
        .iter()
        .any(|d| d.format == format && d.identity == id && (!changed || !d.generated))
    {
        return true;
    }
    if !env.sys.objects.retain(id) {
        fail(env, 8);
        return false;
    }
    env.sys.objects.owned_mut(id).unwrap().clipboard = true;
    let previous = {
        let cb = &mut user_mut(env).clipboard;
        let mut previous = Vec::new();
        if changed && matches!(format, TEXT | OEM | UNICODE | LOCALE) {
            cb.data.retain(|entry| {
                if entry.generated {
                    previous.push(entry.identity);
                    false
                } else {
                    true
                }
            });
        }
        if let Some(old) = cb.data.iter_mut().find(|d| d.format == format) {
            previous.push(
                std::mem::replace(
                    old,
                    Data {
                        format,
                        identity: id,
                        generated: !changed,
                    },
                )
                .identity,
            );
        } else {
            cb.data.push(Data {
                format,
                identity: id,
                generated: !changed,
            });
        }
        if changed {
            cb.touch();
        }
        previous
    };
    previous.into_iter().all(|previous| retire(env, previous))
}
fn supports(env: &Env<'_>, format: u32) -> bool {
    matches!(format, TEXT | OEM | UNICODE | LOCALE) || user(env).clipboard.name(format).is_some()
}
#[win32(user32,name="SetClipboardData",level=implement)]
fn set_clipboard_data(env: &mut Env<'_>, format: u32, handle: Handle) -> Handle {
    if !opened(env) {
        return Handle(0);
    }
    if user(env).clipboard.owner == 0
        || user(env).clipboard.open.unwrap().window != user(env).clipboard.owner
    {
        return Handle(fail(env, 5));
    }
    if !supports(env, format) || handle.0 == 0 {
        return Handle(fail(env, 120));
    }
    let Some(globals) = env.ext.try_get::<Globals>() else {
        return Handle(fail(env, 6));
    };
    let Some(id) = globals.identity(&env.sys.objects, handle.0) else {
        return Handle(fail(env, 6));
    };
    let block = env.sys.objects.owned(id).unwrap();
    if !block.movable || block.pointer == 0 || block.size == 0 {
        return Handle(fail(env, 87));
    }
    if block.size > MAX_BYTES {
        return Handle(fail(env, 8));
    }
    if format == LOCALE && block.size < 4 {
        return Handle(fail(env, 87));
    }
    if adv_abi::guest::check_range(env.cpu, block.pointer, block.size, Prot::READ).is_err() {
        return Handle(fail(env, 87));
    }
    if publish(env, format, id, true) {
        handle
    } else {
        Handle(0)
    }
}
fn formats(env: &Env<'_>) -> Vec<u32> {
    let mut list: Vec<u32> = user(env).clipboard.data.iter().map(|d| d.format).collect();
    if let Some(source) = list
        .iter()
        .copied()
        .find(|f| matches!(*f, TEXT | OEM | UNICODE))
    {
        let targets = match source {
            TEXT => [OEM, UNICODE],
            OEM => [TEXT, UNICODE],
            _ => [OEM, TEXT],
        };
        for target in targets {
            let cp = known_codepage(env);
            if cp.is_none() || (cp == Some(1252) && (source == OEM || target == OEM)) {
                continue;
            }
            if !list.contains(&target) {
                list.push(target);
            }
        }
    }
    list
}
fn allocate_bytes(env: &mut Env<'_>, bytes: &[u8]) -> Option<Identity<Block>> {
    let size = u32::try_from(bytes.len()).ok()?.max(1).checked_add(7)? & !7;
    if size > MAX_BYTES || !env.sys.objects.can_insert() {
        fail(env, 8);
        return None;
    }
    let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
    let Some(pointer) = heaps.alloc(env.cpu, &mut env.sys.vm, 0, size, true) else {
        fail(env, 8);
        return None;
    };
    if env.cpu.write(pointer, bytes).is_err() {
        heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        return None;
    }
    let block = Block {
        handle: 0,
        pointer,
        size,
        movable: true,
        locks: 0,
        attributes: 0,
        clipboard: false,
    };
    let Some(handle) = globals.insert(&mut env.sys.objects, block) else {
        heaps.free(env.cpu, &mut env.sys.vm, 0, pointer);
        fail(env, 8);
        return None;
    };
    globals.identity(&env.sys.objects, handle)
}
fn synthesized(env: &mut Env<'_>, format: u32, bytes: &[u8]) -> Option<u32> {
    let id = allocate_bytes(env, bytes)?;
    if !publish(env, format, id, false) {
        let handle = env.sys.objects.owned(id)?.handle;
        let (heaps, globals) = env.ext.get2_mut::<Heaps, Globals>();
        let _ = globals.free(
            &mut env.sys.objects,
            heaps,
            env.cpu,
            &mut env.sys.vm,
            handle,
            false,
        );
        return None;
    }
    env.sys.objects.owned(id).map(|b| b.handle)
}
fn known_codepage(env: &Env<'_>) -> Option<u32> {
    let locale = user(env)
        .clipboard
        .data
        .iter()
        .find(|d| d.format == LOCALE)
        .copied();
    let lang = if let Some(data) = locale {
        let block = env.sys.objects.owned(data.identity)?;
        GuestPtr::<u32>::new(block.pointer).read(env.cpu).ok()? & 0xffff
    } else {
        user(env).keyboard_layout & 0xffff
    };
    match lang {
        0x411 => Some(932),
        0x804 => Some(936),
        0x409 => Some(1252),
        _ => None,
    }
}
fn converted(env: &mut Env<'_>, target: u32) -> Option<u32> {
    let source = user(env)
        .clipboard
        .data
        .iter()
        .find(|d| matches!(d.format, TEXT | OEM | UNICODE))
        .copied()?;
    let block = env.sys.objects.owned(source.identity)?.clone();
    if block.size > MAX_BYTES
        || adv_abi::guest::check_range(env.cpu, block.pointer, block.size, Prot::READ).is_err()
    {
        fail(env, 87);
        return None;
    }
    let mut bytes = vec![0; block.size as usize];
    env.cpu.read(block.pointer, &mut bytes).ok()?;
    let codec = env.ext.get::<Win32>().codec;
    let Some(cp) = known_codepage(env) else {
        fail(env, 120);
        return None;
    };
    // Japanese and Chinese profiles use the same ANSI/OEM multibyte tables.
    // US OEM437 is not in the current codec stack: refuse that conversion.
    if cp == 1252 && (source.format == OEM || target == OEM) {
        fail(env, 120);
        return None;
    }
    let units = if source.format == UNICODE {
        let mut units = Vec::new();
        let mut terminated = false;
        for pair in bytes.as_chunks::<2>().0 {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit == 0 {
                terminated = true;
                break;
            }
            units.push(unit);
        }
        if !terminated {
            fail(env, 87);
            return None;
        }
        units
    } else {
        let Some(end) = bytes.iter().position(|&b| b == 0) else {
            fail(env, 87);
            return None;
        };
        codec.to_utf16(cp, &bytes[..end])
    };
    let output: Vec<u8> = if target == UNICODE {
        units
            .into_iter()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut bytes = codec.from_utf16(cp, &units).0;
        bytes.push(0);
        bytes
    };
    synthesized(env, target, &output)
}
#[win32(user32,name="GetClipboardData",level=implement)]
fn get_clipboard_data(env: &mut Env<'_>, format: u32) -> Handle {
    if !opened(env) {
        return Handle(0);
    }
    if let Some(data) = user(env).clipboard.data.iter().find(|d| d.format == format) {
        return Handle(env.sys.objects.owned(data.identity).map_or(0, |b| b.handle));
    }
    if matches!(format, TEXT | OEM | UNICODE)
        && user(env)
            .clipboard
            .data
            .iter()
            .any(|d| matches!(d.format, TEXT | OEM | UNICODE))
    {
        return Handle(converted(env, format).unwrap_or(0));
    }
    Handle(fail(env, 0))
}
#[win32(user32,name="CloseClipboard",level=state)]
fn close_clipboard(env: &mut Env<'_>) -> Outcome {
    if !opened(env) {
        return Outcome::Return(0);
    }
    if user(env).clipboard.data.iter().any(|d| d.format == TEXT)
        && !user(env).clipboard.data.iter().any(|d| d.format == LOCALE)
    {
        let locale = user(env).keyboard_layout & 0xffff;
        if synthesized(env, LOCALE, &locale.to_le_bytes()).is_none() {
            fail(env, 8);
            return Outcome::Return(0);
        }
    }
    let head = {
        let cb = &mut user_mut(env).clipboard;
        cb.open = None;
        if std::mem::take(&mut cb.changed) {
            cb.head()
        } else {
            0
        }
    };
    if head != 0 {
        message::send(env, head, 0x308, 0, 0, |_, _| Outcome::Return(1))
    } else {
        Outcome::Return(1)
    }
}
#[win32(user32,name="EnumClipboardFormats",level=state)]
fn enum_clipboard_formats(env: &mut Env<'_>, previous: u32) -> u32 {
    if !opened(env) {
        return 0;
    }
    let list = formats(env);
    let index = if previous == 0 {
        0
    } else {
        let Some(i) = list.iter().position(|&f| f == previous) else {
            return fail(env, 87);
        };
        i + 1
    };
    list.get(index).copied().unwrap_or_else(|| fail(env, 0))
}
#[win32(user32,name="CountClipboardFormats",level=state)]
fn count_clipboard_formats(env: &mut Env<'_>) -> i32 {
    formats(env).len() as i32
}
#[win32(user32,name="IsClipboardFormatAvailable",level=state)]
fn is_clipboard_format_available(env: &mut Env<'_>, format: u32) -> u32 {
    u32::from(formats(env).contains(&format))
}
#[win32(user32,name="GetClipboardOwner",level=state)]
fn get_clipboard_owner(env: &mut Env<'_>) -> Handle {
    Handle(user(env).clipboard.owner)
}
#[win32(user32,name="GetOpenClipboardWindow",level=state)]
fn get_open_clipboard_window(env: &mut Env<'_>) -> Handle {
    Handle(user(env).clipboard.open.map_or(0, |o| o.window))
}
#[win32(user32,name="GetClipboardSequenceNumber",level=state)]
fn get_clipboard_sequence_number(env: &mut Env<'_>) -> u32 {
    user(env).clipboard.sequence
}
fn register_name(env: &mut Env<'_>, name: Vec<u16>) -> u32 {
    user_mut(env)
        .clipboard
        .register(name)
        .unwrap_or_else(|| fail(env, 87))
}
#[win32(user32,name="RegisterClipboardFormatA",level=state)]
fn register_clipboard_format_a(env: &mut Env<'_>, name: AnsiStr) -> u32 {
    let Ok(bytes) = name.read(env.cpu, 1024) else {
        return fail(env, 87);
    };
    register_name(env, env.ext.get::<Win32>().codec.to_utf16(0, &bytes))
}
#[win32(user32,name="RegisterClipboardFormatW",level=state)]
fn register_clipboard_format_w(env: &mut Env<'_>, name: WideStr) -> u32 {
    let Ok(units) = name.read(env.cpu, 256) else {
        return fail(env, 87);
    };
    register_name(env, units)
}
fn format_name(env: &mut Env<'_>, format: u32, output: u32, count: i32, wide: bool) -> i32 {
    if count <= 0 {
        fail(env, 87);
        return 0;
    }
    let Some(name) = user(env).clipboard.name(format) else {
        fail(env, 87);
        return 0;
    };
    let (bytes, n) = if wide {
        let n = name.len().min(count as usize - 1);
        (
            name[..n]
                .iter()
                .copied()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
            n,
        )
    } else {
        let encoded = env.ext.get::<Win32>().codec.from_utf16(0, name).0;
        let n = encoded.len().min(count as usize - 1);
        (
            encoded[..n].iter().copied().chain([0]).collect::<Vec<_>>(),
            n,
        )
    };
    if adv_abi::guest::check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err()
        || env.cpu.write(output, &bytes).is_err()
    {
        fail(env, 87);
        return 0;
    }
    n as i32
}
#[win32(user32,name="GetClipboardFormatNameA",level=state)]
fn get_clipboard_format_name_a(env: &mut Env<'_>, format: u32, output: AnsiStr, count: i32) -> i32 {
    format_name(env, format, output.0.addr(), count, false)
}
#[win32(user32,name="GetClipboardFormatNameW",level=state)]
fn get_clipboard_format_name_w(env: &mut Env<'_>, format: u32, output: WideStr, count: i32) -> i32 {
    format_name(env, format, output.0.addr(), count, true)
}
#[win32(user32,name="SetClipboardViewer",level=state)]
fn set_clipboard_viewer(env: &mut Env<'_>, hwnd: Handle) -> Outcome {
    if !live(env, hwnd.0) {
        return Outcome::Return(fail(env, 1400));
    }
    let cb = &mut user_mut(env).clipboard;
    if cb.viewers.iter().any(|v| v.window == hwnd.0) {
        return Outcome::Return(fail(env, 87));
    }
    let old = cb.head();
    cb.viewers.insert(0, Viewer { window: hwnd.0 });
    message::send(env, hwnd.0, 0x308, 0, 0, move |_, _| Outcome::Return(old))
}
#[win32(user32,name="GetClipboardViewer",level=state)]
fn get_clipboard_viewer(env: &mut Env<'_>) -> Handle {
    Handle(user(env).clipboard.head())
}
#[win32(user32,name="ChangeClipboardChain",level=state)]
fn change_clipboard_chain(env: &mut Env<'_>, remove: Handle, next: Handle) -> Outcome {
    let cb = &mut user_mut(env).clipboard;
    let Some(index) = cb.viewers.iter().position(|v| v.window == remove.0) else {
        return Outcome::Return(fail(env, 87));
    };
    let actual = cb.viewers.get(index + 1).map_or(0, |v| v.window);
    if actual != next.0 {
        return Outcome::Return(fail(env, 87));
    }
    cb.viewers.remove(index);
    let head = cb.head();
    if head == 0 {
        Outcome::Return(1)
    } else {
        message::send(env, head, 0x30d, remove.0, next.0, |_, _| {
            Outcome::Return(1)
        })
    }
}
/// Window removal cannot retain a clipboard lock or stale viewer link. Rendered
/// data remains system-owned even after its owner window disappears.
pub(super) fn remove_windows(
    env: &mut Env<'_>,
    windows: &[u32],
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let mut notifications = VecDeque::new();
    {
        let cb = &mut user_mut(env).clipboard;
        let released = cb.open.is_some_and(|o| windows.contains(&o.window));
        for &hwnd in windows {
            if let Some((head, next)) = cb.remove_window(hwnd)
                && head != 0
                && !windows.contains(&head)
            {
                notifications.push_back([head, 0x30d, hwnd, next]);
            }
        }
        if released && std::mem::take(&mut cb.changed) && cb.head() != 0 {
            notifications.push_back([cb.head(), 0x308, 0, 0]);
        }
    }
    message::send_all(env, notifications, then)
}
/// ExitThread also releases a NULL-window open lock and removes its viewers.
pub(crate) fn exit_thread(
    env: &mut Env<'_>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let current = env.context;
    let windows: Vec<u32> = user(env)
        .windows
        .values(&env.sys.objects)
        .filter(|w| w.thread == current)
        .map(|w| w.hwnd)
        .collect();
    let released = {
        let cb = &mut user_mut(env).clipboard;
        if cb.open.is_some_and(|o| o.thread == current) {
            cb.open = None;
            true
        } else {
            false
        }
    };
    remove_windows(env, &windows, move |env| {
        let head = {
            let cb = &mut user_mut(env).clipboard;
            if released && std::mem::take(&mut cb.changed) {
                cb.head()
            } else {
                0
            }
        };
        if head != 0 {
            message::send(env, head, 0x308, 0, 0, move |env, _| then(env))
        } else {
            then(env)
        }
    })
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        open_clipboard_ABI,
        close_clipboard_ABI,
        empty_clipboard_ABI,
        set_clipboard_data_ABI,
        get_clipboard_data_ABI,
        enum_clipboard_formats_ABI,
        count_clipboard_formats_ABI,
        is_clipboard_format_available_ABI,
        get_clipboard_owner_ABI,
        get_open_clipboard_window_ABI,
        get_clipboard_sequence_number_ABI,
        register_clipboard_format_a_ABI,
        register_clipboard_format_w_ABI,
        get_clipboard_format_name_a_ABI,
        get_clipboard_format_name_w_ABI,
        set_clipboard_viewer_ABI,
        get_clipboard_viewer_ABI,
        change_clipboard_chain_ABI,
    ] {
        registry.register(api);
    }
}
