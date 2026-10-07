//! Resource file names are guest VFS paths; the shared text owner registers a
//! private copy with visibility separate from memory-font registrations.
use super::*;
use std::io::Read;

pub(super) struct Reference {
    pub id: text::FontResourceId,
    count: u32,
    faces: u32,
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        add_a_ABI,
        add_w_ABI,
        remove_a_ABI,
        remove_w_ABI,
        add_ex_a_ABI,
        add_ex_w_ABI,
        remove_ex_a_ABI,
        remove_ex_w_ABI,
    ] {
        registry.register(api);
    }
}
fn ansi(env: &Env<'_>, path: AnsiStr) -> Option<String> {
    let bytes = path.read(env.cpu, 32768).ok()?;
    Some(env.ext.get::<Win32>().codec.decode(&bytes))
}
fn wide(env: &Env<'_>, path: WideStr) -> Option<String> {
    String::from_utf16(&path.read(env.cpu, 32768).ok()?).ok()
}
#[win32(gdi32, name = "AddFontResourceA", level = implement)]
fn add_a(env: &mut Env<'_>, path: AnsiStr) -> i32 {
    ansi(env, path).map_or(0, |path| add(env, &path, 0))
}
#[win32(gdi32, name = "AddFontResourceW", level = implement)]
fn add_w(env: &mut Env<'_>, path: WideStr) -> i32 {
    wide(env, path).map_or(0, |path| add(env, &path, 0))
}
#[win32(gdi32, name = "RemoveFontResourceA", level = implement)]
fn remove_a(env: &mut Env<'_>, path: AnsiStr) -> i32 {
    ansi(env, path).map_or(0, |path| remove(env, &path, 0))
}
#[win32(gdi32, name = "RemoveFontResourceW", level = implement)]
fn remove_w(env: &mut Env<'_>, path: WideStr) -> i32 {
    wide(env, path).map_or(0, |path| remove(env, &path, 0))
}
#[win32(gdi32, name = "AddFontResourceExA", level = implement)]
fn add_ex_a(env: &mut Env<'_>, path: AnsiStr, flags: u32, _reserved: GuestPtr<()>) -> i32 {
    ansi(env, path).map_or(0, |path| add(env, &path, flags))
}
#[win32(gdi32, name = "AddFontResourceExW", level = implement)]
fn add_ex_w(env: &mut Env<'_>, path: WideStr, flags: u32, _reserved: GuestPtr<()>) -> i32 {
    wide(env, path).map_or(0, |path| add(env, &path, flags))
}
#[win32(gdi32, name = "RemoveFontResourceExA", level = implement)]
fn remove_ex_a(env: &mut Env<'_>, path: AnsiStr, flags: u32, _reserved: GuestPtr<()>) -> i32 {
    ansi(env, path).map_or(0, |path| remove(env, &path, flags))
}
#[win32(gdi32, name = "RemoveFontResourceExW", level = implement)]
fn remove_ex_w(env: &mut Env<'_>, path: WideStr, flags: u32, _reserved: GuestPtr<()>) -> i32 {
    wide(env, path).map_or(0, |path| remove(env, &path, flags))
}
const FR_PRIVATE: u32 = 0x10;
const FR_NOT_ENUM: u32 = 0x20;
fn valid_flags(env: &mut Env<'_>, flags: u32) -> bool {
    if flags & !(FR_PRIVATE | FR_NOT_ENUM) != 0 {
        set_last_error(env, 87);
        return false;
    }
    true
}
fn key(env: &Env<'_>, path: &str) -> String {
    env.ext.get::<Win32>().vfs.normalize(path).to_lowercase()
}
fn read(env: &Env<'_>, path: &str) -> Option<Vec<u8>> {
    let vfs = &env.ext.get::<Win32>().vfs;
    let host = vfs.lookup(path)?;
    let mut file = std::fs::File::open(host).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 || length > text::MAX_FONT_RESOURCE_BYTES {
        return None;
    }
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).ok()?;
    if vfs.sections.has_entries() {
        let identity = adv_os::vfs::file_identity(&file).ok()?;
        if let Some(section) = vfs.sections.lookup(identity) {
            section.merge_read(0, &mut bytes).ok()?;
        }
    }
    Some(bytes)
}
fn add(env: &mut Env<'_>, path: &str, flags: u32) -> i32 {
    if !valid_flags(env, flags) || path.is_empty() {
        return 0;
    }
    let key = (key(env, path), flags);
    let fonts = env.ext.get_mut::<Fonts>();
    if let Some(reference) = fonts.resources.get_mut(&key) {
        let Some(count) = reference.count.checked_add(1) else {
            return 0;
        };
        reference.count = count;
        let faces = reference.faces;
        fonts.text.invalidate_fonts();
        return faces as i32;
    }
    let Some(bytes) = read(env, path) else {
        return 0;
    };
    let fonts = env.ext.get_mut::<Fonts>();
    let Ok((id, faces)) = fonts
        .text
        .add_file_resource(&bytes, flags & FR_NOT_ENUM == 0)
    else {
        return 0;
    };
    fonts.resources.insert(
        key,
        Reference {
            id,
            count: 1,
            faces,
        },
    );
    faces as i32
}
fn remove(env: &mut Env<'_>, path: &str, flags: u32) -> i32 {
    if !valid_flags(env, flags) {
        return 0;
    }
    let key = (key(env, path), flags);
    let fonts = env.ext.get_mut::<Fonts>();
    let Some(reference) = fonts.resources.get_mut(&key) else {
        if flags & FR_NOT_ENUM != 0
            && fonts
                .resources
                .contains_key(&(key.0.clone(), flags & !FR_NOT_ENUM))
        {
            // Windows can report success without consuming a visible reference
            // for this mismatched-flags corner. Preserve the documented owner
            // contract and expose the uncovered behavior instead of mutating it.
            let thunk = env.sys.thunks.get(env.thunk);
            let api = format!("{}!{}", thunk.module, thunk.name);
            adv_abi::warn_unavailable(
                env,
                &api,
                "removing a visible font resource with FR_NOT_ENUM is unavailable; use the registration flags",
            );
        }
        return 0;
    };
    if reference.count > 1 {
        reference.count -= 1;
        fonts.text.invalidate_fonts();
        return 1;
    }
    let id = reference.id;
    if fonts.text.remove_resource(id).ok() != Some(true) {
        return 0;
    }
    fonts.resources.remove(&key);
    1
}

#[cfg(test)]
mod tests;
