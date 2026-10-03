//! Window-owned system menu state, queried and edited by ordinary Win32 APIs.
//! Built-in labels use a fixed virtual en-US locale, independent of the host UI.
use super::{user, user_mut};
use crate::{stdcall, util::set_last_error};
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Default)]
pub struct Menus {
    owners: BTreeMap<u32, u32>,
    menus: BTreeMap<u32, Vec<Item>>,
    restored: BTreeSet<u32>,
    attached: BTreeMap<u32, u32>,
    class_resources: BTreeMap<usize, adv_pe::ResId>,
    help_ids: BTreeMap<u32, u32>,
}
#[derive(Clone, Default)]
struct Item {
    id: u32,
    flags: u32,
    text: Vec<u16>,
    data: u32,
    bitmap: u32,
    checked: u32,
    unchecked: u32,
    submenu: u32,
}

impl Menus {
    pub(super) fn remove_window(&mut self, hwnd: u32, objects: &mut HashMap<u32, String>) {
        self.remove_system(hwnd, objects);
        if let Some(menu) = self.attached.remove(&hwnd) {
            self.destroy(menu, objects);
        }
    }
    fn remove_system(&mut self, hwnd: u32, objects: &mut HashMap<u32, String>) {
        self.restored.remove(&hwnd);
        if let Some(menu) = self.owners.remove(&hwnd) {
            self.destroy(menu, objects);
        }
    }
    fn destroy(&mut self, menu: u32, objects: &mut HashMap<u32, String>) -> bool {
        if !self.menus.contains_key(&menu) {
            return false;
        }
        let mut pending = vec![menu];
        while let Some(menu) = pending.pop() {
            if let Some(items) = self.menus.remove(&menu) {
                objects.remove(&menu);
                self.help_ids.remove(&menu);
                pending.extend(
                    items
                        .iter()
                        .filter_map(|item| (item.submenu != 0).then_some(item.submenu)),
                );
            }
        }
        true
    }
    fn locate(&self, menu: u32, value: u32, by_position: bool) -> Option<(u32, usize)> {
        if by_position {
            return (value < self.menus.get(&menu)?.len() as u32).then_some((menu, value as usize));
        }
        let mut pending = vec![(menu, 0)];
        let mut visited = BTreeSet::new();
        while let Some((menu, begin)) = pending.pop() {
            if begin == 0 && !visited.insert(menu) {
                continue;
            }
            let Some(items) = self.menus.get(&menu) else {
                continue;
            };
            for (at, item) in items.iter().enumerate().skip(begin) {
                if item.id == value {
                    return Some((menu, at));
                }
                if item.submenu != 0 {
                    pending.push((menu, at + 1));
                    pending.push((item.submenu, 0));
                    break;
                }
            }
        }
        None
    }
    fn reaches(&self, start: u32, target: u32) -> bool {
        let mut pending = vec![start];
        let mut seen = BTreeSet::new();
        while let Some(menu) = pending.pop() {
            if menu == target {
                return true;
            }
            if !seen.insert(menu) {
                continue;
            }
            if let Some(items) = self.menus.get(&menu) {
                pending.extend(
                    items
                        .iter()
                        .filter_map(|item| (item.submenu != 0).then_some(item.submenu)),
                );
            }
        }
        false
    }
}

pub(super) fn class_menu(env: &mut Env<'_>, class: usize, pointer: u32, wide: bool) {
    if pointer == 0 {
        user_mut(env).menus.class_resources.remove(&class);
        return;
    }
    let resource = crate::kernel32::res_id(crate::kernel32::int_or_str(env, pointer, wide));
    user_mut(env).menus.class_resources.insert(class, resource);
}

pub(super) fn attach_creation(
    env: &mut Env<'_>,
    hwnd: u32,
    menu: u32,
    child: bool,
) -> Result<u32, u32> {
    if child {
        return Ok(0);
    }
    let menu = if menu != 0 {
        if !user(env).menus.menus.contains_key(&menu) {
            return Err(1401);
        }
        menu
    } else {
        let win = &user(env).windows[&hwnd];
        let resource = user(env).menus.class_resources.get(&win.class).cloned();
        if let Some(resource) = resource {
            load_resource(env, win.instance, &resource)?
        } else {
            0
        }
    };
    if menu != 0 {
        user_mut(env).menus.attached.insert(hwnd, menu);
    }
    Ok(menu)
}

fn get_system_menu(env: &mut Env<'_>) -> Outcome {
    let (hwnd, revert) = (env.arg(0), env.arg(1) != 0);
    let Some(window) = user(env).windows.get(&hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let style = window.style;
    // CS_NOCLOSE removes Close from the initial menu. A native TRUE reset
    // restores the unmodified default menu, including Close, for this window.
    let no_close = user(env).classes[window.class].style & 0x200 != 0
        && !user(env).menus.restored.contains(&hwnd);
    let u = user_mut(env);
    if revert {
        u.menus.remove_system(hwnd, &mut u.objects);
        u.menus.restored.insert(hwnd);
        return Outcome::Return(0);
    }
    if style & 0x80000 == 0 {
        return Outcome::Return(0);
    } // WS_SYSMENU
    if let Some(&menu) = u.menus.owners.get(&hwnd) {
        return Outcome::Return(menu);
    }
    let menu = u.add_object("system menu".into());
    let enabled = |yes| if yes { 0 } else { 3 }; // MF_DISABLED | MF_GRAYED
    let label = |id, flags, text: &str| Item {
        id,
        flags,
        text: text.encode_utf16().collect(),
        ..Item::default()
    };
    let mut items = vec![
        label(
            0xf120,
            enabled(style & (0x01000000 | 0x20000000) != 0),
            "&Restore",
        ),
        label(0xf010, enabled(style & 0x01000000 == 0), "&Move"),
        label(
            0xf000,
            enabled(style & 0x40000 != 0 && style & 0x01000000 == 0),
            "&Size",
        ),
        label(
            0xf020,
            enabled(style & 0x20000 != 0 && style & 0x20000000 == 0),
            "Mi&nimize",
        ),
        label(
            0xf030,
            enabled(style & 0x10000 != 0 && style & 0x01000000 == 0),
            "Ma&ximize",
        ),
    ];
    if !no_close {
        items.push(Item {
            flags: 0x800,
            ..Item::default()
        }); // separator
        items.push(label(0xf060, 0x1000, "&Close\tAlt+F4"));
    }
    u.menus.owners.insert(hwnd, menu);
    u.menus.menus.insert(menu, items);
    Outcome::Return(menu)
}

fn edit(env: &mut Env<'_>, mode: u8) -> Outcome {
    let (menu, id, flags) = (env.arg(0), env.arg(1), env.arg(2));
    let failure = if mode == 0 { 0 } else { u32::MAX };
    if !user(env).menus.menus.contains_key(&menu) {
        set_last_error(env, 1401);
        return Outcome::Return(failure);
    }
    let Some((menu, index)) = user(env).menus.locate(menu, id, flags & 0x400 != 0) else {
        set_last_error(env, 1456);
        return Outcome::Return(failure);
    };
    if mode == 0 || mode == 3 {
        let u = user_mut(env);
        let item = u.menus.menus.get_mut(&menu).unwrap().remove(index);
        if mode == 0 && item.submenu != 0 {
            u.menus.destroy(item.submenu, &mut u.objects);
        }
        return Outcome::Return(1);
    }
    let items = user_mut(env).menus.menus.get_mut(&menu).unwrap();
    let mask = if mode == 1 { 3 } else { 8 };
    let old = items[index].flags & mask;
    items[index].flags = (items[index].flags & !mask) | (flags & mask);
    Outcome::Return(old)
}

fn item_info(env: &mut Env<'_>, wide: bool, replace: bool) -> Outcome {
    let (menu, index, by_position, pointer) = (env.arg(0), env.arg(1), env.arg(2) != 0, env.arg(3));
    if !user(env).menus.menus.contains_key(&menu) {
        set_last_error(env, 1401);
        return Outcome::Return(0);
    }
    let location = user(env).menus.locate(menu, index, by_position);
    if replace && location.is_none() {
        set_last_error(env, 1456);
        return Outcome::Return(0);
    }
    let (menu, at) = location.unwrap_or((menu, user(env).menus.menus[&menu].len()));
    let size = env.cpu.read_u32(pointer).unwrap_or(0);
    if !matches!(size, 44 | 48) || !env.cpu.accessible(pointer, size, Prot::READ) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let read = |offset| env.cpu.read_u32(pointer + offset).unwrap_or(0);
    let mask = read(4);
    if mask & 0x10 != 0 && mask & (0x40 | 0x80 | 0x100) != 0 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let mut item = if replace {
        user(env).menus.menus[&menu][at].clone()
    } else {
        Item::default()
    };
    if mask & 1 != 0 {
        item.flags = (item.flags & !0x108b) | (read(12) & 0x108b);
    }
    if mask & (0x10 | 0x100) != 0 {
        item.flags = (item.flags & !0x6f64) | (read(8) & 0x6f64);
    }
    if mask & 2 != 0 {
        item.id = read(16);
    }
    if mask & 4 != 0 {
        let submenu = read(20);
        if submenu != 0
            && (!user(env).menus.menus.contains_key(&submenu)
                || user(env).menus.reaches(submenu, menu))
        {
            set_last_error(
                env,
                if user(env).menus.menus.contains_key(&submenu) {
                    87
                } else {
                    1401
                },
            );
            return Outcome::Return(0);
        }
        item.submenu = submenu;
    }
    if mask & 8 != 0 {
        item.checked = read(24);
        item.unchecked = read(28);
    }
    if mask & 0x20 != 0 {
        item.data = read(32);
    }
    if mask & 0x80 != 0 && size == 48 {
        item.bitmap = read(44);
    }
    if mask & 0x10 != 0 && item.flags & 0x4 != 0 {
        // Legacy MFT_BITMAP carries an opaque HBITMAP in dwTypeData, not a
        // string address. Never dereference it or truncate the handle.
        item.bitmap = read(36);
    }
    if mask & (0x40 | 0x10) != 0 && item.flags & (0x4 | 0x100 | 0x800) == 0 {
        item.text = match text(env, read(36), wide) {
            Ok(text) => text,
            Err(error) => return menu_error(env, error),
        };
    }
    let items = user_mut(env).menus.menus.get_mut(&menu).unwrap();
    if !replace && items.len() >= 16384 {
        set_last_error(env, 8);
        return Outcome::Return(0);
    }
    if replace {
        items[at] = item;
    } else {
        items.insert(at, item);
    }
    Outcome::Return(1)
}

fn get_item(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (menu, index, by_position, pointer) = (env.arg(0), env.arg(1), env.arg(2) != 0, env.arg(3));
    let size = env.cpu.read_u32(pointer).unwrap_or(0);
    if !matches!(size, 44 | 48) || !env.cpu.accessible(pointer, size, Prot::RW) {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let mask = env.cpu.read_u32(pointer + 4).unwrap_or(0);
    if mask & 0x10 != 0 && mask & (0x40 | 0x80 | 0x100) != 0 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    if !user(env).menus.menus.contains_key(&menu) {
        set_last_error(env, 1401);
        return Outcome::Return(0);
    }
    let Some((menu, at)) = user(env).menus.locate(menu, index, by_position) else {
        set_last_error(env, 1456);
        return Outcome::Return(0);
    };
    let item = &user(env).menus.menus[&menu][at];
    let mut fields = Vec::new();
    if mask & 1 != 0 {
        fields.push((12, item.flags & 0x108b));
    }
    if mask & (0x10 | 0x100) != 0 {
        fields.push((8, item.flags & 0x6f64));
    }
    if mask & 2 != 0 {
        fields.push((16, item.id));
    }
    if mask & 4 != 0 {
        fields.push((20, item.submenu));
    }
    if mask & 8 != 0 {
        fields.extend([(24, item.checked), (28, item.unchecked)]);
    }
    if mask & 0x20 != 0 {
        fields.push((32, item.data));
    }
    if mask & 0x80 != 0 && size == 48 {
        fields.push((44, item.bitmap));
    }
    let text = if wide {
        item.text
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<_>>()
    } else {
        env.ext
            .get::<crate::Win32>()
            .codec
            .from_utf16(0, &item.text)
            .0
    };
    if mask & 0x10 != 0 && item.flags & (0x4 | 0x100 | 0x800) != 0 {
        // Native legacy queries return a scalar for bitmap items and zero for
        // separators/owner-draw items. The caller's out/cch are not buffers.
        fields.extend([
            (
                36,
                if item.flags & 0x4 != 0 {
                    item.bitmap
                } else {
                    0
                },
            ),
            (40, 0),
        ]);
    } else if mask & (0x40 | 0x10) != 0 {
        let out = env.cpu.read_u32(pointer + 36).unwrap_or(0);
        let capacity = env.cpu.read_u32(pointer + 40).unwrap_or(0) as usize;
        let width = if wide { 2 } else { 1 };
        let n = if out == 0 {
            text.len() / width
        } else {
            (text.len() / width).min(capacity.saturating_sub(1))
        };
        fields.push((40, n as u32));
        if out != 0 && capacity != 0 {
            let mut output = text[..n * width].to_vec();
            output.resize(output.len() + width, 0);
            if !env.cpu.accessible(out, output.len() as u32, Prot::WRITE) {
                set_last_error(env, 998);
                return Outcome::Return(0);
            }
            if let Err(error) = env.cpu.write(out, &output) {
                return Outcome::Fail(error.to_string());
            }
        }
    }
    for (offset, value) in fields {
        if let Err(error) = env.cpu.write_u32(pointer + offset, value) {
            return Outcome::Fail(error.to_string());
        }
    }
    Outcome::Return(1)
}

fn menu_error(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn new_handle(env: &mut Env<'_>) -> Result<u32, u32> {
    let u = user_mut(env);
    if u.menus.menus.len() >= 16384 {
        return Err(8);
    }
    let menu = u.add_object("menu".into());
    u.menus.menus.insert(menu, Vec::new());
    Ok(menu)
}
fn create_menu(env: &mut Env<'_>) -> Outcome {
    match new_handle(env) {
        Ok(menu) => Outcome::Return(menu),
        Err(error) => menu_error(env, error),
    }
}
fn destroy_menu(env: &mut Env<'_>) -> Outcome {
    let menu = env.arg(0);
    let u = user_mut(env);
    if u.menus.destroy(menu, &mut u.objects) {
        Outcome::Return(1)
    } else {
        menu_error(env, 1401)
    }
}
fn get_menu(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let Some(win) = user(env).windows.get(&hwnd) else {
        return menu_error(env, 1400);
    };
    // USER32's shared slot is a control ID for a child, never an attachment.
    Outcome::Return(if win.parent != 0 {
        win.id
    } else {
        user(env).menus.attached.get(&hwnd).copied().unwrap_or(0)
    })
}
fn set_menu(env: &mut Env<'_>) -> Outcome {
    let (hwnd, menu) = (env.arg(0), env.arg(1));
    let Some(win) = user(env).windows.get(&hwnd) else {
        return menu_error(env, 1400);
    };
    if win.parent != 0 {
        return menu_error(env, 1436);
    }
    if menu != 0 && !user(env).menus.menus.contains_key(&menu) {
        return menu_error(env, 1401);
    }
    let u = user_mut(env);
    if menu == 0 {
        u.menus.attached.remove(&hwnd);
    } else {
        u.menus.attached.insert(hwnd, menu);
    }
    Outcome::Return(1)
}
fn get_submenu(env: &mut Env<'_>) -> Outcome {
    let (menu, at) = (env.arg(0), env.arg(1));
    if !user(env).menus.menus.contains_key(&menu) {
        return menu_error(env, 1401);
    }
    Outcome::Return(
        user(env)
            .menus
            .menus
            .get(&menu)
            .and_then(|items| items.get(at as usize))
            .map_or(0, |item| item.submenu),
    )
}
fn text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, u32> {
    if pointer == 0 {
        return Ok(Vec::new());
    }
    if wide {
        let mut units = Vec::new();
        for at in 0..crate::util::MAX_STR {
            let address = pointer.checked_add((at * 2) as u32).ok_or(87u32)?;
            let unit = env.cpu.read_u16(address).map_err(|_| 87u32)?;
            if unit == 0 {
                return Ok(units);
            }
            units.push(unit);
        }
        Err(206)
    } else {
        let bytes = env
            .cpu
            .read_cstr(pointer, crate::util::MAX_STR)
            .map_err(|_| 87u32)?;
        if bytes.len() == crate::util::MAX_STR || pointer.checked_add(bytes.len() as u32).is_none()
        {
            return Err(206);
        }
        Ok(env.ext.get::<crate::Win32>().codec.to_utf16(0, &bytes))
    }
}
fn legacy_insert(env: &mut Env<'_>, wide: bool, append: bool) -> Outcome {
    let menu = env.arg(0);
    if !user(env).menus.menus.contains_key(&menu) {
        return menu_error(env, 1401);
    }
    let shift = u32::from(!append);
    let (flags, id, source) = (env.arg(1 + shift), env.arg(2 + shift), env.arg(3 + shift));
    let target = if append {
        None
    } else {
        user(env).menus.locate(menu, env.arg(1), flags & 0x400 != 0)
    };
    let (menu, at) = target.unwrap_or((menu, user(env).menus.menus[&menu].len()));
    let mut item = Item {
        id,
        flags: flags & !0x410,
        ..Item::default()
    };
    if flags & 0x10 != 0 {
        if !user(env).menus.menus.contains_key(&id) {
            return menu_error(env, 1401);
        }
        if user(env).menus.reaches(id, menu) {
            return menu_error(env, 87);
        }
        item.submenu = id;
    }
    if flags & 0x800 != 0 {
        item.id = 0;
        item.flags |= 3;
    } else if flags & 4 != 0 {
        item.bitmap = source;
    } else if flags & 0x100 != 0 {
        item.data = source;
    } else {
        item.text = match text(env, source, wide) {
            Ok(text) => text,
            Err(error) => return menu_error(env, error),
        };
    }
    let items = user_mut(env).menus.menus.get_mut(&menu).unwrap();
    if items.len() >= 16384 {
        return menu_error(env, 8);
    }
    items.insert(at, item);
    Outcome::Return(1)
}

// Parse before allocating any HMENU: malformed or oversized templates cannot
// leave partial handle trees. Indirect templates have no supplied byte length;
// enforce the same 1-MiB, 64-level and 16384-item limits using checked reads.
struct Template<'a> {
    cpu: &'a dyn adv_cpu::backend::Guest,
    base: u32,
    len: u32,
    at: u32,
    extended: bool,
    items: usize,
    menus: usize,
}
struct ParsedMenu {
    items: Vec<(Item, Option<ParsedMenu>)>,
    help: u32,
}
impl Template<'_> {
    fn word(&mut self) -> Result<u16, u32> {
        if self.at.checked_add(2).is_none_or(|end| end > self.len) {
            return Err(13);
        }
        let address = self.base.checked_add(self.at).ok_or(13u32)?;
        let value = self.cpu.read_u16(address).map_err(|_| 13u32)?;
        self.at += 2;
        Ok(value)
    }
    fn dword(&mut self) -> Result<u32, u32> {
        let low = self.word()?;
        Ok(u32::from(low) | u32::from(self.word()?) << 16)
    }
    fn align(&mut self) -> Result<(), u32> {
        self.at = self.at.checked_add(3).ok_or(13u32)? & !3;
        if self.at > self.len { Err(13) } else { Ok(()) }
    }
    fn menu(&mut self, depth: usize, help: u32) -> Result<ParsedMenu, u32> {
        if depth >= 64 {
            return Err(13);
        }
        self.menus += 1;
        let mut result = ParsedMenu {
            items: Vec::new(),
            help,
        };
        loop {
            self.items += 1;
            if self.items > 16384 {
                return Err(8);
            }
            let (flags, id, information) = if self.extended {
                self.align()?;
                let kind = self.dword()?;
                let state = self.dword()?;
                let id = self.dword()?;
                (kind | state, id, self.word()?)
            } else {
                let information = self.word()?;
                (
                    u32::from(information & !0x90),
                    if information & 0x10 == 0 {
                        u32::from(self.word()?)
                    } else {
                        0
                    },
                    information,
                )
            };
            let mut label = Vec::new();
            loop {
                let unit = self.word()?;
                if unit == 0 {
                    break;
                }
                label.push(unit);
            }
            let popup = information & if self.extended { 1 } else { 0x10 } != 0;
            let mut item = Item {
                id,
                flags,
                text: label,
                ..Item::default()
            };
            if flags & 0x800 != 0
                || (!self.extended && !popup && item.id == 0 && item.text.is_empty())
            {
                item.flags |= 0x800;
                item.flags |= 3;
            }
            let child = if popup {
                let help = if self.extended {
                    self.align()?;
                    self.dword()?
                } else {
                    0
                };
                Some(self.menu(depth + 1, help)?)
            } else {
                None
            };
            result.items.push((item, child));
            if information & 0x80 != 0 {
                return Ok(result);
            }
        }
    }
}
fn build_menu(env: &mut Env<'_>, parsed: ParsedMenu, extended: bool) -> u32 {
    // The parsed total and existing count were checked together by load_template.
    let menu = new_handle(env).unwrap();
    let mut items = Vec::with_capacity(parsed.items.len());
    for (mut item, child) in parsed.items {
        if let Some(child) = child {
            item.submenu = build_menu(env, child, extended);
            if !extended {
                item.id = item.submenu;
            }
        }
        items.push(item);
    }
    user_mut(env).menus.menus.insert(menu, items);
    user_mut(env).menus.help_ids.insert(menu, parsed.help);
    menu
}
fn parse_template(
    cpu: &dyn adv_cpu::backend::Guest,
    base: u32,
    len: u32,
) -> Result<(ParsedMenu, bool, usize), u32> {
    if base == 0 || len > 1024 * 1024 {
        return Err(13);
    }
    let mut input = Template {
        cpu,
        base,
        len,
        at: 0,
        extended: false,
        items: 0,
        menus: 0,
    };
    let version = input.word()?;
    let offset = input.word()?;
    if version > 1 {
        return Err(13);
    }
    input.extended = version == 1;
    let help = if input.extended { input.dword()? } else { 0 };
    let start = 4 + u32::from(offset);
    if start < input.at || start > len {
        return Err(13);
    }
    input.at = start;
    let parsed = input.menu(0, help)?;
    let extended = input.extended;
    Ok((parsed, extended, input.menus))
}
fn load_template(env: &mut Env<'_>, base: u32, len: u32) -> Result<u32, u32> {
    let (parsed, extended, menus) = parse_template(env.cpu, base, len)?;
    if menus + user(env).menus.menus.len() > 16384 {
        return Err(8);
    }
    Ok(build_menu(env, parsed, extended))
}
fn load_resource(env: &mut Env<'_>, instance: u32, name: &adv_pe::ResId) -> Result<u32, u32> {
    let (base, len) =
        crate::kernel32::find_resource_data(env, instance, &adv_pe::ResId::Id(4), name)
            .ok_or(1814u32)?;
    load_template(env, base, len)
}
fn load_menu(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (instance, pointer) = (env.arg(0), env.arg(1));
    let name = crate::kernel32::res_id(crate::kernel32::int_or_str(env, pointer, wide));
    match load_resource(env, instance, &name) {
        Ok(menu) => Outcome::Return(menu),
        Err(error) => menu_error(env, error),
    }
}
fn load_indirect(env: &mut Env<'_>) -> Outcome {
    match load_template(env, env.arg(0), 1024 * 1024) {
        Ok(menu) => Outcome::Return(menu),
        Err(error) => menu_error(env, error),
    }
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, super::USER32, "GetSystemMenu", 2, get_system_menu);
    stdcall(reg, super::USER32, "InsertMenuItemA", 4, |e| {
        item_info(e, false, false)
    });
    stdcall(reg, super::USER32, "InsertMenuItemW", 4, |e| {
        item_info(e, true, false)
    });
    stdcall(reg, super::USER32, "SetMenuItemInfoA", 4, |e| {
        item_info(e, false, true)
    });
    stdcall(reg, super::USER32, "SetMenuItemInfoW", 4, |e| {
        item_info(e, true, true)
    });
    stdcall(reg, super::USER32, "GetMenuItemInfoA", 4, |e| {
        get_item(e, false)
    });
    stdcall(reg, super::USER32, "GetMenuItemInfoW", 4, |e| {
        get_item(e, true)
    });
    stdcall(reg, super::USER32, "DeleteMenu", 3, |e| edit(e, 0));
    stdcall(reg, super::USER32, "RemoveMenu", 3, |e| edit(e, 3));
    stdcall(reg, super::USER32, "EnableMenuItem", 3, |e| edit(e, 1));
    stdcall(reg, super::USER32, "CheckMenuItem", 3, |e| edit(e, 2));
    stdcall(reg, super::USER32, "IsMenu", 1, |e| {
        Outcome::Return(u32::from(user(e).menus.menus.contains_key(&e.arg(0))))
    });
    stdcall(reg, super::USER32, "GetMenuItemCount", 1, |e| {
        let Some(items) = user(e).menus.menus.get(&e.arg(0)) else {
            set_last_error(e, 1401);
            return Outcome::Return(u32::MAX);
        };
        Outcome::Return(items.len() as u32)
    });
    stdcall(reg, super::USER32, "GetMenuItemID", 2, |e| {
        Outcome::Return(
            user(e)
                .menus
                .menus
                .get(&e.arg(0))
                .and_then(|items| items.get(e.arg(1) as usize))
                .map_or(
                    u32::MAX,
                    |item| if item.submenu == 0 { item.id } else { u32::MAX },
                ),
        )
    });
    stdcall(reg, super::USER32, "GetMenuState", 3, |e| {
        let Some((menu, at)) = user(e)
            .menus
            .locate(e.arg(0), e.arg(1), e.arg(2) & 0x400 != 0)
        else {
            return Outcome::Return(u32::MAX);
        };
        let item = &user(e).menus.menus[&menu][at];
        let flags = if item.submenu == 0 {
            item.flags
        } else {
            (item.flags & 0xff)
                | 0x10
                | ((user(e).menus.menus.get(&item.submenu).map_or(0, Vec::len) as u32) << 8)
        };
        Outcome::Return(flags)
    });
    stdcall(reg, super::USER32, "CreateMenu", 0, create_menu);
    stdcall(reg, super::USER32, "CreatePopupMenu", 0, create_menu);
    stdcall(reg, super::USER32, "DestroyMenu", 1, destroy_menu);
    stdcall(reg, super::USER32, "GetMenu", 1, get_menu);
    stdcall(reg, super::USER32, "SetMenu", 2, set_menu);
    stdcall(reg, super::USER32, "GetSubMenu", 2, get_submenu);
    stdcall(reg, super::USER32, "LoadMenuA", 2, |e| load_menu(e, false));
    stdcall(reg, super::USER32, "LoadMenuW", 2, |e| load_menu(e, true));
    stdcall(reg, super::USER32, "LoadMenuIndirectA", 1, load_indirect);
    stdcall(reg, super::USER32, "LoadMenuIndirectW", 1, load_indirect);
    stdcall(reg, super::USER32, "AppendMenuA", 4, |e| {
        legacy_insert(e, false, true)
    });
    stdcall(reg, super::USER32, "AppendMenuW", 4, |e| {
        legacy_insert(e, true, true)
    });
    stdcall(reg, super::USER32, "InsertMenuA", 5, |e| {
        legacy_insert(e, false, false)
    });
    stdcall(reg, super::USER32, "InsertMenuW", 5, |e| {
        legacy_insert(e, true, false)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::{Guest, MemFault, Reg};
    const BASE: u32 = 0x10000;
    struct TemplateGuest(Vec<u8>);
    impl Guest for TemplateGuest {
        fn reg(&self, _: Reg) -> u32 {
            0
        }
        fn set_reg(&mut self, _: Reg, _: u32) {}
        fn read(&self, addr: u32, out: &mut [u8]) -> Result<(), MemFault> {
            let fault = MemFault {
                addr,
                len: out.len() as u32,
            };
            let at = addr.checked_sub(BASE).ok_or(fault)? as usize;
            out.copy_from_slice(self.0.get(at..at + out.len()).ok_or(fault)?);
            Ok(())
        }
        fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault> {
            Err(MemFault {
                addr,
                len: data.len() as u32,
            })
        }
        fn map(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
        fn protect(&mut self, addr: u32, len: u32, _: Prot) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
        fn unmap(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
            Err(MemFault { addr, len })
        }
    }
    fn parse(words: &[u16]) -> Result<(ParsedMenu, bool, usize), u32> {
        let guest = TemplateGuest(words.iter().flat_map(|word| word.to_le_bytes()).collect());
        parse_template(&guest, BASE, guest.0.len() as u32)
    }
    #[test]
    fn malformed_and_short_templates_fail_before_building_menu_handles() {
        for words in [
            &[0u16, 0][..],
            &[2, 0],
            &[1, 0, 0, 0],
            &[0, 0, 0x80, 1, b'x' as u16],
        ] {
            assert!(matches!(parse(words), Err(13)));
        }
        let guest = TemplateGuest(vec![0; 4]);
        assert!(matches!(
            parse_template(&guest, BASE, 1024 * 1024 + 1),
            Err(13)
        ));
        assert!(matches!(parse_template(&guest, u32::MAX, 16), Err(13)));
    }
    #[test]
    fn template_depth_and_item_budgets_have_exact_boundaries() {
        let mut words = vec![0, 0];
        for _ in 0..63 {
            words.extend([0x90, 0]);
        }
        words.extend([0x80, 1, 0]);
        assert_eq!(parse(&words).map(|result| result.2), Ok(64));
        words.splice(2..2, [0x90, 0]);
        assert!(matches!(parse(&words), Err(13)));
        let mut words = vec![0, 0];
        for _ in 0..16383 {
            words.extend([0, 1, 0]);
        }
        words.extend([0x80, 2, 0]);
        assert_eq!(parse(&words).map(|result| result.0.items.len()), Ok(16384));
        words.splice(2..2, [0, 1, 0]);
        assert!(matches!(parse(&words), Err(8)));
    }
    #[test]
    fn cyclic_links_are_detected_and_destroy_visits_shared_submenus_once() {
        let mut menus = Menus::default();
        let mut objects = HashMap::new();
        menus.menus.insert(
            1,
            vec![
                Item {
                    submenu: 2,
                    ..Item::default()
                },
                Item {
                    submenu: 2,
                    ..Item::default()
                },
            ],
        );
        menus.menus.insert(
            2,
            vec![Item {
                submenu: 3,
                ..Item::default()
            }],
        );
        menus.menus.insert(3, Vec::new());
        for handle in 1..=3 {
            objects.insert(handle, "menu".into());
        }
        // A proposed 3 -> 1 link would reach 3 and form a cycle.
        assert!(menus.reaches(1, 3));
        assert!(!menus.reaches(3, 1));
        assert!(menus.destroy(1, &mut objects));
        assert!(menus.menus.is_empty() && objects.is_empty());
    }
}
