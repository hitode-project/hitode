//! Resource file names are guest VFS paths; the shared text owner registers a
//! private copy with visibility separate from memory-font registrations.
use super::*;

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
        add_memory_ABI,
        remove_memory_ABI,
    ] {
        registry.register(api);
    }
}
pub(super) fn ansi(env: &Env<'_>, path: AnsiStr) -> Option<String> {
    let bytes = path.read(env.cpu, 32768).ok()?;
    Some(env.ext.get::<Fonts>().codec.decode(&bytes))
}
pub(super) fn wide(env: &Env<'_>, path: WideStr) -> Option<String> {
    String::from_utf16(&path.read(env.cpu, 32768).ok()?).ok()
}
#[win32(gdi32, name = "AddFontResourceA", level = implement)]
pub(super) fn add_a(env: &mut Env<'_>, path: AnsiStr) -> i32 {
    ansi(env, path).map_or(0, |path| add(env, &path, 0))
}
#[win32(gdi32, name = "AddFontResourceW", level = implement)]
pub(super) fn add_w(env: &mut Env<'_>, path: WideStr) -> i32 {
    wide(env, path).map_or(0, |path| add(env, &path, 0))
}
#[win32(gdi32, name = "RemoveFontResourceA", level = implement)]
pub(super) fn remove_a(env: &mut Env<'_>, path: AnsiStr) -> i32 {
    ansi(env, path).map_or(0, |path| remove(env, &path, 0))
}
#[win32(gdi32, name = "RemoveFontResourceW", level = implement)]
pub(super) fn remove_w(env: &mut Env<'_>, path: WideStr) -> i32 {
    wide(env, path).map_or(0, |path| remove(env, &path, 0))
}
#[win32(gdi32, name = "AddFontResourceExA", level = implement)]
pub(super) fn add_ex_a(
    env: &mut Env<'_>,
    path: AnsiStr,
    flags: u32,
    _reserved: GuestPtr<()>,
) -> i32 {
    ansi(env, path).map_or(0, |path| add(env, &path, flags))
}
#[win32(gdi32, name = "AddFontResourceExW", level = implement)]
pub(super) fn add_ex_w(
    env: &mut Env<'_>,
    path: WideStr,
    flags: u32,
    _reserved: GuestPtr<()>,
) -> i32 {
    wide(env, path).map_or(0, |path| add(env, &path, flags))
}
#[win32(gdi32, name = "RemoveFontResourceExA", level = implement)]
pub(super) fn remove_ex_a(
    env: &mut Env<'_>,
    path: AnsiStr,
    flags: u32,
    _reserved: GuestPtr<()>,
) -> i32 {
    ansi(env, path).map_or(0, |path| remove(env, &path, flags))
}
#[win32(gdi32, name = "RemoveFontResourceExW", level = implement)]
pub(super) fn remove_ex_w(
    env: &mut Env<'_>,
    path: WideStr,
    flags: u32,
    _reserved: GuestPtr<()>,
) -> i32 {
    wide(env, path).map_or(0, |path| remove(env, &path, flags))
}
/// Memory fonts are private to the process and never enumerated
/// (WIN32-GDI-15). Each call is one registration that its handle names.
#[win32(gdi32, name = "AddFontMemResourceEx", level = implement)]
pub(super) fn add_memory(
    env: &mut Env<'_>,
    font: GuestPtr<()>,
    size: u32,
    _reserved: GuestPtr<()>,
    count: GuestPtr<u32>,
) -> Handle {
    let length = size as usize;
    if font.is_null() || count.is_null() || length == 0 || length > text::MAX_FONT_RESOURCE_BYTES {
        set_last_error(env, 87);
        return Handle(0);
    }
    let mut bytes = vec![0; length];
    if env.cpu.read(font.addr(), &mut bytes).is_err()
        || adv_abi::guest::check_range(env.cpu, count.addr(), 4, adv_cpu::backend::Prot::WRITE)
            .is_err()
    {
        set_last_error(env, 87);
        return Handle(0);
    }
    let fonts = env.ext.get_mut::<Fonts>();
    let Some(handle) = fonts.next_memory.checked_add(1) else {
        return Handle(0);
    };
    let Ok((id, faces)) = fonts.text.add_resource(&bytes) else {
        return Handle(0);
    };
    fonts.next_memory = handle;
    fonts.memory.insert(handle, id);
    let _ = env.cpu.write(count.addr(), &faces.to_le_bytes());
    Handle(handle)
}
#[win32(gdi32, name = "RemoveFontMemResourceEx", level = implement)]
pub(super) fn remove_memory(env: &mut Env<'_>, handle: Handle) -> i32 {
    let fonts = env.ext.get_mut::<Fonts>();
    let Some(&id) = fonts.memory.get(&handle.0) else {
        set_last_error(env, 6);
        return 0;
    };
    if fonts.text.remove_resource(id).ok() != Some(true) {
        return 0;
    }
    fonts.memory.remove(&handle.0);
    1
}
const FR_PRIVATE: u32 = 0x10;
const FR_NOT_ENUM: u32 = 0x20;
pub(super) fn valid_flags(env: &mut Env<'_>, flags: u32) -> bool {
    if flags & !(FR_PRIVATE | FR_NOT_ENUM) != 0 {
        set_last_error(env, 87);
        return false;
    }
    true
}
fn nt_path(env: &Env<'_>, path: &str) -> String {
    if path.starts_with(r"\??\") {
        return path.to_owned();
    }
    if path.as_bytes().get(1) == Some(&b':') {
        return format!(r"\??\{path}");
    }
    let peb = env.cpu.read_u32(env.ctx().teb + 0x30).unwrap();
    let params = env
        .cpu
        .read_u32(peb + adv_machine::layout::peb::PROCESS_PARAMETERS)
        .unwrap();
    let directory = env.cpu.read_u32(params + 0x28).unwrap();
    let len = env.cpu.read_u16(params + 0x24).unwrap();
    let bytes = placement::read(env, directory, u32::from(len), false).unwrap();
    let directory = String::from_utf16_lossy(&placement::units(bytes));
    format!(
        r"\??\{}{path}",
        directory.trim_end_matches('\\').to_owned() + r"\"
    )
}
pub(super) fn key(env: &Env<'_>, path: &str) -> String {
    nt_path(env, path).to_lowercase()
}
pub(super) fn read(env: &mut Env<'_>, path: &str) -> Option<Vec<u8>> {
    let path = nt_path(env, path);
    let handle =
        adv_nt::file::open_path(env, path.encode_utf16().collect(), 0x80000000, 7, 0).ok()?;
    let result = (|| {
        let fd = adv_nt::server::handle_fd(env, handle).ok()?;
        let mounts = env.ext.get_or_default::<adv_nt::file::volume::Mounts>();
        adv_nt::file::volume::read_all(mounts, fd.fd).ok()
    })();
    let _ = adv_nt::server::close(env, handle);
    result
}
pub(super) fn add(env: &mut Env<'_>, path: &str, flags: u32) -> i32 {
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
pub(super) fn remove(env: &mut Env<'_>, path: &str, flags: u32) -> i32 {
    if !valid_flags(env, flags) {
        return 0;
    }
    let key = (key(env, path), flags);
    let fonts = env.ext.get_mut::<Fonts>();
    let Some(reference) = fonts.resources.get_mut(&key) else {
        return i32::from(
            flags & FR_NOT_ENUM != 0
                && fonts
                    .resources
                    .contains_key(&(key.0.clone(), flags & !FR_NOT_ENUM)),
        );
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

pub(super) fn make_dir(env: &mut Env<'_>, a: &[u32]) -> Outcome {
    if a[2] < 251 || a[4] <= 2 {
        return Outcome::Return(0);
    }
    let Some(path) = wide(env, WideStr(GuestPtr::new(a[3]))) else {
        return Outcome::Return(0);
    };
    let Some(bytes) = read(env, &path) else {
        return Outcome::Return(0);
    };
    let fonts = env.ext.get_mut::<Fonts>();
    let Ok((id, _)) = fonts.text.add_resource(&bytes) else {
        return Outcome::Return(0);
    };
    let result = (|| {
        let request = fonts.text.resource_font(id, 0, 100, 0).ok()?;
        fonts.text.outline_metrics(&request).ok()
    })();
    let _ = fonts.text.remove_resource(id);
    let Some(outline) = result else {
        return Outcome::Return(0);
    };
    let m = &outline.text;
    let mut bytes = [0; 251];
    let word = |bytes: &mut [u8], at: usize, v: i32| {
        bytes[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes())
    };
    word(&mut bytes, 0, 1);
    word(&mut bytes, 4, 0x200);
    bytes[6..10].copy_from_slice(&149u32.to_le_bytes());
    bytes[10..22].copy_from_slice(b"Wine fontdir");
    word(&mut bytes, 70, 0x4003 | if a[0] != 0 { 0x80 } else { 0 });
    for (at, v) in [
        (72, outline.em_units as i32),
        (74, 72),
        (76, 72),
        (78, m.ascent),
        (80, m.internal_leading),
        (82, m.external_leading),
        (87, m.weight),
        (92, m.height),
        (95, m.average_width),
        (97, m.maximum_width),
    ] {
        word(&mut bytes, at, v);
    }
    bytes[84] = u8::from(m.italic);
    bytes[85] = u8::from(m.underline);
    bytes[86] = u8::from(m.strikeout);
    bytes[89] = encode_encoding(m.encoding);
    bytes[94] = pack_metrics(m)[51];
    bytes[99..103].copy_from_slice(&[
        m.first_character,
        m.last_character,
        m.default_character,
        m.break_character,
    ]);
    bytes[109..113].copy_from_slice(&118u32.to_le_bytes());
    let codec = env.ext.get::<Fonts>().codec;
    let mut at = 118;
    for name in [&outline.names[0], &outline.names[1], &outline.names[2]] {
        let name = codec.encode(name);
        let n = name.len().min(250 - at);
        bytes[at..at + n].copy_from_slice(&name[..n]);
        at += n + 1;
    }
    if env.cpu.write(a[1], &bytes).is_err() {
        return Outcome::Return(0);
    }
    Outcome::Return(at as u32)
}

#[cfg(test)]
mod tests;
