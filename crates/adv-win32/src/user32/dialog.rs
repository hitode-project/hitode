//! Dialogs: DLGTEMPLATE / DLGTEMPLATEEX parsing, CreateDialog*, DialogBox* with a modal loop
//! run by the host (as continuations, so the guest context can block in it), and the
//! dialog-item helpers.

use std::collections::VecDeque;

use adv_abi::{GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::{Env, Outcome};
use adv_pe::ResId;

use super::message::{dispatch, next_any, send, wait_for_message};
use super::window::{
    self, ClassKey, CreateParams, DWL_DLGPROC, Rect, SW_HIDE, SW_SHOWNORMAL, User, WS_CHILD,
    WS_VISIBLE, create_window_encoded, destroy_window, nc_insets, show_window, wm,
};
use super::{USER32, user, user_mut};
use crate::stdcall;

mod presentation;
pub use presentation::{
    DialogControl, DialogEvent, DialogFont, DialogSet, DialogTarget, DialogWindow, post_host_event,
    presentation,
};

const RT_DIALOG: u16 = 5;
const DS_ABSALIGN: u32 = 0x1;
const DS_SETFONT: u32 = 0x40;
const DS_NOFAILCREATE: u32 = 0x10;
const DS_CENTER: u32 = 0x800;
const DS_SHELLFONT: u32 = 0x48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SzOrOrd {
    None,
    Ord(u16),
    Str(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub id: u32,
    pub class: SzOrOrd,
    pub title: SzOrOrd,
    /// Guest address of the creation data, including its size word, if any.
    pub data: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Template {
    pub style: u32,
    pub ex_style: u32,
    pub x: i16,
    pub y: i16,
    pub cx: i16,
    pub cy: i16,
    pub menu: SzOrOrd,
    pub class: SzOrOrd,
    pub title: String,
    /// Point size and face name (DS_SETFONT).
    pub font: Option<(u16, String)>,
    pub items: Vec<Item>,
}

/// Reads a template from guest memory.
struct Cursor<'a> {
    cpu: &'a dyn Guest,
    at: u32,
}

impl Cursor<'_> {
    fn u16(&mut self) -> Option<u16> {
        let v = self.cpu.read_u16(self.at).ok()?;
        self.at += 2;
        Some(v)
    }
    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = self.cpu.read_u32(self.at).ok()?;
        self.at += 4;
        Some(v)
    }
    fn u8(&mut self) -> Option<u8> {
        let mut b = [0];
        self.cpu.read(self.at, &mut b).ok()?;
        self.at += 1;
        Some(b[0])
    }
    fn wstr(&mut self) -> Option<String> {
        let mut units = Vec::new();
        loop {
            match self.u16()? {
                0 => break,
                u => units.push(u),
            }
            if units.len() > 4096 {
                return None;
            }
        }
        Some(String::from_utf16_lossy(&units))
    }
    fn sz_or_ord(&mut self) -> Option<SzOrOrd> {
        match self.cpu.read_u16(self.at).ok()? {
            0 => {
                self.at += 2;
                Some(SzOrOrd::None)
            }
            0xffff => {
                self.at += 2;
                Some(SzOrOrd::Ord(self.u16()?))
            }
            _ => Some(SzOrOrd::Str(self.wstr()?)),
        }
    }
    fn align4(&mut self) {
        self.at = self.at.next_multiple_of(4);
    }
}

/// Parse a DLGTEMPLATE or DLGTEMPLATEEX at `addr`.
pub fn parse_template(cpu: &dyn Guest, addr: u32) -> Option<Template> {
    let mut c = Cursor { cpu, at: addr };
    let ex = cpu.read_u16(addr).ok()? == 1 && cpu.read_u16(addr + 2).ok()? == 0xffff;
    let (style, ex_style, count);
    if ex {
        c.at += 8; // dlgVer, signature, helpID
        ex_style = c.u32()?;
        style = c.u32()?;
    } else {
        style = c.u32()?;
        ex_style = c.u32()?;
    }
    count = c.u16()?;
    let (x, y, cx, cy) = (c.i16()?, c.i16()?, c.i16()?, c.i16()?);
    let menu = c.sz_or_ord()?;
    let class = c.sz_or_ord()?;
    let title = c.wstr()?;
    let font = if style & DS_SETFONT != 0 || (ex && style & DS_SHELLFONT == DS_SHELLFONT) {
        let pt = c.u16()?;
        if ex {
            c.at += 4; // weight, italic, charset
        }
        Some((pt, c.wstr()?))
    } else {
        None
    };
    let mut items = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        c.align4();
        let (style, ex_style) = if ex {
            c.at += 4; // helpID
            let e = c.u32()?;
            (c.u32()?, e)
        } else {
            (c.u32()?, c.u32()?)
        };
        let (x, y, cx, cy) = (c.i16()?, c.i16()?, c.i16()?, c.i16()?);
        let id = if ex { c.u32()? } else { u32::from(c.u16()?) };
        let class = c.sz_or_ord()?;
        let title = c.sz_or_ord()?;
        let extra = c.u16()?;
        // Native USER32 passes the size word too, for both template formats.
        let data = (extra > 0).then_some(c.at - 2);
        c.at += u32::from(extra);
        items.push(Item {
            style,
            ex_style,
            x,
            y,
            cx,
            cy,
            id,
            class,
            title,
            data,
        });
    }
    let _ = c.u8();
    Some(Template {
        style,
        ex_style,
        x,
        y,
        cx,
        cy,
        menu,
        class,
        title,
        font,
        items,
    })
}

/// Dialog base units (average character width, height) for a template font.
pub fn base_units(font: Option<&(u16, String)>) -> (i32, i32) {
    match font {
        None => (8, 16), // system font
        Some((pt, _)) => {
            let h = (i32::from(*pt) * 4 + 1) / 3 + 2;
            (h / 2, h)
        }
    }
}

fn predefined_class(ord: u16) -> &'static str {
    match ord {
        0x80 => "Button",
        0x81 => "Edit",
        0x82 => "Static",
        0x83 => "ListBox",
        0x84 => "ScrollBar",
        0x85 => "ComboBox",
        _ => "Static",
    }
}

#[allow(clippy::too_many_arguments)]
/// CreateDialogIndirectParam: create the dialog and its controls, send WM_INITDIALOG,
/// show it if the template (or `force_visible`) says so. `then` gets the HWND or 0.
pub fn create_dialog(
    env: &mut Env<'_>,
    instance: u32,
    t: Template,
    owner: u32,
    dlgproc: u32,
    param: u32,
    force_visible: bool,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    create_dialog_encoded(
        env,
        instance,
        t,
        owner,
        dlgproc,
        param,
        force_visible,
        false,
        then,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_dialog_encoded(
    env: &mut Env<'_>,
    instance: u32,
    t: Template,
    owner: u32,
    dlgproc: u32,
    param: u32,
    force_visible: bool,
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    if owner != 0 && !user(env).is_window(&env.sys.objects, owner) {
        crate::util::set_last_error(env, 1400);
        return then(env, 0);
    }
    let (bx, by) = base_units(t.font.as_ref());
    let px = |v: i16| i32::from(v) * bx / 4;
    let py = |v: i16| i32::from(v) * by / 8;
    let (l, top, r, b) = nc_insets(t.style, t.ex_style);
    let (w, h) = (px(t.cx) + l + r, py(t.cy) + top + b);
    let u = user(env);
    let (mut x, mut y) = (px(t.x), py(t.y));
    let child = t.style & WS_CHILD != 0;
    if t.style & DS_CENTER != 0 {
        let area = if owner != 0 && !child {
            u.screen_rect(&env.sys.objects, owner)
                .unwrap_or(Rect::new(0, 0, u.screen.0, u.screen.1))
        } else {
            Rect::new(0, 0, u.screen.0, u.screen.1 - 40)
        };
        (x, y) = (
            area.left + (area.width() - w) / 2,
            area.top + (area.height() - h) / 2,
        );
    } else if !child && t.style & DS_ABSALIGN == 0 && owner != 0 {
        let (ox, oy) = u.client_origin(&env.sys.objects, owner);
        (x, y) = (x + ox, y + oy);
    }
    let class_name = match &t.class {
        SzOrOrd::Str(s) => s.clone(),
        SzOrOrd::Ord(o) => format!("#{o}"),
        SzOrOrd::None => "#32770".into(),
    };
    let visible = t.style & WS_VISIBLE != 0 || force_visible;
    let class_ptr = match t.class {
        SzOrOrd::Ord(ordinal) => u32::from(ordinal),
        _ => super::message::guest_text(env, &class_name, wide),
    };
    let title_ptr = super::message::guest_text(env, &t.title, wide);
    if class_ptr == 0 || title_ptr == 0 {
        if class_ptr >= 0x1_0000 {
            window::temp_free(env, class_ptr);
        }
        window::temp_free(env, title_ptr);
        crate::util::set_last_error(env, 8);
        return then(env, 0);
    }
    let p = CreateParams {
        ex_style: t.ex_style,
        class: match t.class {
            SzOrOrd::Ord(ordinal) => ClassKey::Atom(ordinal),
            _ => ClassKey::Name(class_name),
        },
        class_ptr,
        name_ptr: title_ptr,
        text: t.title.clone(),
        style: t.style & !WS_VISIBLE,
        x: x as u32,
        y: y as u32,
        w: w as u32,
        h: h as u32,
        parent: owner,
        menu: 0,
        instance,
        param,
    };
    create_window_encoded(env, p, wide, move |env, dlg| {
        if class_ptr >= 0x1_0000 {
            window::temp_free(env, class_ptr);
        }
        window::temp_free(env, title_ptr);
        if dlg == 0 {
            return then(env, 0);
        }
        if let Some(w) = env
            .ext
            .get::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &dlg)
            && w.extra.len() >= 8
        {
            w.extra[DWL_DLGPROC as usize..DWL_DLGPROC as usize + 4]
                .copy_from_slice(&dlgproc.to_le_bytes());
            w.control.base_units = (bx, by);
        }
        let font = t.font.as_ref().map_or(0, |(pt, face)| {
            crate::gdi32::create_font(env, -(i32::from(*pt) * 4 / 3), face)
        });
        let items: VecDeque<Item> = t.items.into_iter().collect();
        let allow_failed_controls = t.style & DS_NOFAILCREATE != 0;
        let after_font = move |env: &mut Env<'_>| {
            create_items(
                env,
                dlg,
                instance,
                items,
                (bx, by),
                font,
                wide,
                allow_failed_controls,
                move |env, complete| {
                    if !complete {
                        return destroy_window(env, dlg, move |env, _| then(env, 0));
                    }
                    let focus = first_tab_stop(user(env), &env.sys.objects, dlg);
                    send(env, dlg, wm::INITDIALOG, focus, param, move |env, r| {
                        // A modeless callback may destroy the dialog during initialization.
                        if !user(env).is_window(&env.sys.objects, dlg) {
                            return then(env, 0);
                        }
                        // EndDialog during INITDIALOG ends creation before showing or focusing.
                        if user(env)
                            .windows
                            .get(&env.sys.objects, &dlg)
                            .is_some_and(|w| w.control.dialog_end.is_some())
                        {
                            return then(env, dlg);
                        }
                        let show = move |env: &mut Env<'_>| {
                            if !visible {
                                return then(env, dlg);
                            }
                            show_window(env, dlg, SW_SHOWNORMAL, move |env, _| then(env, dlg))
                        };
                        if r != 0 && focus != 0 {
                            return window::set_focus(env, focus, move |env, _| show(env));
                        }
                        show(env)
                    })
                },
            )
        };
        if font != 0 {
            return send(env, dlg, wm::SETFONT, font, 0, move |env, _| {
                after_font(env)
            });
        }
        after_font(env)
    })
}

#[allow(clippy::too_many_arguments)]
fn create_items(
    env: &mut Env<'_>,
    dlg: u32,
    instance: u32,
    mut items: VecDeque<Item>,
    (bx, by): (i32, i32),
    font: u32,
    wide: bool,
    allow_failed_controls: bool,
    then: impl FnOnce(&mut Env<'_>, bool) -> Outcome + 'static,
) -> Outcome {
    let Some(it) = items.pop_front() else {
        return then(env, true);
    };
    let class_name = match &it.class {
        SzOrOrd::Ord(o) => predefined_class(*o).to_owned(),
        SzOrOrd::Str(s) => s.clone(),
        SzOrOrd::None => "Static".into(),
    };
    let title = match &it.title {
        SzOrOrd::Str(s) => s.clone(),
        _ => String::new(),
    };
    let class_ptr = super::message::guest_text(env, &class_name, wide);
    let title_ptr = match it.title {
        SzOrOrd::Ord(ordinal) => u32::from(ordinal),
        _ => super::message::guest_text(env, &title, wide),
    };
    if class_ptr == 0 || title_ptr == 0 {
        window::temp_free(env, class_ptr);
        if title_ptr >= 0x1_0000 {
            window::temp_free(env, title_ptr);
        }
        crate::util::set_last_error(env, 8);
        return then(env, false);
    }
    let p = CreateParams {
        ex_style: it.ex_style,
        class: ClassKey::Name(class_name),
        class_ptr,
        name_ptr: title_ptr,
        text: title,
        style: it.style | WS_CHILD,
        x: (i32::from(it.x) * bx / 4) as u32,
        y: (i32::from(it.y) * by / 8) as u32,
        w: (i32::from(it.cx) * bx / 4) as u32,
        h: (i32::from(it.cy) * by / 8) as u32,
        parent: dlg,
        menu: it.id,
        instance,
        param: it.data.unwrap_or(0),
    };
    create_window_encoded(env, p, wide, move |env, ctl| {
        window::temp_free(env, class_ptr);
        if title_ptr >= 0x1_0000 {
            window::temp_free(env, title_ptr);
        }
        if ctl == 0 {
            log::warn!("dialog control {} could not be created", it.id);
            adv_abi::warn_unavailable(
                env,
                "user32.dll!CreateDialog",
                &format!("dialog control {} could not be created", it.id),
            );
            if !allow_failed_controls {
                return then(env, false);
            }
        } else {
            let u = user(env);
            let w = u.windows.get(&env.sys.objects, &ctl).unwrap();
            let class = &u.classes[w.class].name;
            if !presentation::supported(class, w.style) {
                let reason = format!(
                    "dialog control class {class} with style {:#x} cannot be projected",
                    w.style
                );
                adv_abi::warn_unavailable(env, "user32.dll!CreateDialog", &reason);
            }
        }
        let next = move |env: &mut Env<'_>| {
            create_items(
                env,
                dlg,
                instance,
                items,
                (bx, by),
                font,
                wide,
                allow_failed_controls,
                then,
            )
        };
        if ctl != 0 && font != 0 {
            return send(env, ctl, wm::SETFONT, font, 0, move |env, _| next(env));
        }
        next(env)
    })
}

fn first_tab_stop(u: &User, objects: &adv_machine::Objects, dlg: u32) -> u32 {
    u.windows
        .get(objects, &dlg)
        .and_then(|d| {
            d.children.iter().copied().find(|c| {
                u.windows
                    .get(objects, c)
                    .is_some_and(|w| w.style & 0x0001_0000 != 0 && w.enabled() && w.visible())
            })
        })
        .unwrap_or(0)
}

/// GetDlgItem.
pub fn item(u: &User, objects: &adv_machine::Objects, dlg: u32, id: u32) -> u32 {
    u.windows
        .get(objects, &dlg)
        .and_then(|d| {
            d.children
                .iter()
                .copied()
                .find(|c| u.windows.get(objects, c).is_some_and(|w| w.id == id))
        })
        .unwrap_or(0)
}

fn template_resource(env: &mut Env<'_>, instance: u32, name: u32, wide: bool) -> Option<u32> {
    let id = crate::kernel32::res_id(crate::kernel32::int_or_str(env, name, wide));
    let found = crate::kernel32::find_resource_data(env, instance, &ResId::Id(RT_DIALOG), &id);
    if found.is_none() {
        log::warn!("dialog template {id:?} not found");
        let handle = if instance == 0 {
            env.cpu
                .read_u32(adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::IMAGE_BASE)
                .unwrap_or(0)
        } else {
            instance
        };
        let modules = adv_os::modules(env.ext);
        let error = modules.by_handle(handle).map_or(1812, |index| {
            let module = modules.get(index);
            if module.data_file.is_some() {
                return 1814;
            }
            match &module.resources {
                None => 1812, // ERROR_RESOURCE_DATA_NOT_FOUND
                Some(resources) if resources.get(&ResId::Id(RT_DIALOG)).is_none() => 1813,
                Some(_) => 1814, // ERROR_RESOURCE_NAME_NOT_FOUND
            }
        });
        crate::util::set_last_error(env, error);
    }
    found.map(|(addr, _)| addr)
}

fn create_dialog_param_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name, owner, proc, param) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let Some(t) = resource_template(env, inst, name, false) else {
        return Outcome::Return(0);
    };
    create_dialog(env, inst, t, owner, proc, param, false, |_, h| {
        Outcome::Return(h)
    })
}

fn create_dialog_indirect_param_a(env: &mut Env<'_>) -> Outcome {
    let (inst, tpl, owner, proc, param) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let Some(t) = parse_template(env.cpu, tpl) else {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    };
    create_dialog(env, inst, t, owner, proc, param, false, |_, h| {
        Outcome::Return(h)
    })
}

fn dialog_box_param_a(env: &mut Env<'_>) -> Outcome {
    let (inst, name, owner, proc, param) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let Some(t) = resource_template(env, inst, name, false) else {
        return Outcome::Return(u32::MAX);
    };
    dialog_box(env, inst, t, owner, proc, param, false)
}

fn dialog_box_indirect_param_a(env: &mut Env<'_>) -> Outcome {
    let (inst, tpl, owner, proc, param) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let Some(t) = parse_template(env.cpu, tpl) else {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(u32::MAX);
    };
    dialog_box(env, inst, t, owner, proc, param, false)
}

fn resource_template(env: &mut Env<'_>, instance: u32, name: u32, wide: bool) -> Option<Template> {
    let address = template_resource(env, instance, name, wide)?;
    parse_template(env.cpu, address).or_else(|| {
        crate::util::set_last_error(env, 87);
        None
    })
}

#[win32(user32, name="CreateDialogParamW", level=state)]
fn create_dialog_param_w(
    env: &mut Env<'_>,
    instance: Handle,
    name: WideStr,
    owner: Handle,
    proc: GuestPtr<()>,
    param: i32,
) -> Outcome {
    let Some(template) = resource_template(env, instance.0, name.0.addr(), true) else {
        return Outcome::Return(0);
    };
    create_dialog_encoded(
        env,
        instance.0,
        template,
        owner.0,
        proc.addr(),
        param as u32,
        false,
        true,
        |_, hwnd| Outcome::Return(hwnd),
    )
}

#[win32(user32, name="CreateDialogIndirectParamW", level=state)]
fn create_dialog_indirect_param_w(
    env: &mut Env<'_>,
    instance: Handle,
    template: GuestPtr<()>,
    owner: Handle,
    proc: GuestPtr<()>,
    param: i32,
) -> Outcome {
    let Some(template) = parse_template(env.cpu, template.addr()) else {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    };
    create_dialog_encoded(
        env,
        instance.0,
        template,
        owner.0,
        proc.addr(),
        param as u32,
        false,
        true,
        |_, hwnd| Outcome::Return(hwnd),
    )
}

#[win32(user32, name="DialogBoxParamW", level=state)]
fn dialog_box_param_w(
    env: &mut Env<'_>,
    instance: Handle,
    name: WideStr,
    owner: Handle,
    proc: GuestPtr<()>,
    param: i32,
) -> Outcome {
    let Some(template) = resource_template(env, instance.0, name.0.addr(), true) else {
        return Outcome::Return(u32::MAX);
    };
    dialog_box(
        env,
        instance.0,
        template,
        owner.0,
        proc.addr(),
        param as u32,
        true,
    )
}

#[win32(user32, name="DialogBoxIndirectParamW", level=state)]
fn dialog_box_indirect_param_w(
    env: &mut Env<'_>,
    instance: Handle,
    template: GuestPtr<()>,
    owner: Handle,
    proc: GuestPtr<()>,
    param: i32,
) -> Outcome {
    let Some(template) = parse_template(env.cpu, template.addr()) else {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(u32::MAX);
    };
    dialog_box(
        env,
        instance.0,
        template,
        owner.0,
        proc.addr(),
        param as u32,
        true,
    )
}

/// DialogBoxIndirectParam: disable the owner, create the dialog, run a modal loop until
/// EndDialog, destroy it, re-enable the owner.
#[allow(clippy::too_many_arguments)]
fn dialog_box(
    env: &mut Env<'_>,
    inst: u32,
    t: Template,
    owner: u32,
    proc: u32,
    param: u32,
    wide: bool,
) -> Outcome {
    if owner != 0 && !user(env).is_window(&env.sys.objects, owner) {
        crate::util::set_last_error(env, 1400);
        return Outcome::Return(0);
    }
    let owner = user(env).root(&env.sys.objects, owner);
    let owner_was_enabled = user(env)
        .windows
        .get(&env.sys.objects, &owner)
        .is_some_and(window::Window::enabled);
    if owner_was_enabled
        && let Some(w) = env
            .ext
            .get::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &owner)
    {
        w.style |= window::WS_DISABLED;
    }
    create_dialog_encoded(
        env,
        inst,
        t,
        owner,
        proc,
        param,
        true,
        wide,
        move |env, dlg| {
            if dlg == 0 {
                restore_owner(env, owner, owner_was_enabled);
                return Outcome::Return(u32::MAX);
            }
            if let Some(w) = env
                .ext
                .get::<crate::Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &dlg)
            {
                w.control.modal = true;
            }
            modal_loop(env, dlg, owner, owner_was_enabled)
        },
    )
}

fn restore_owner(env: &mut Env<'_>, owner: u32, was_enabled: bool) {
    if was_enabled
        && let Some(w) = env
            .ext
            .get::<crate::Win32>()
            .user
            .windows
            .get_mut(&mut env.sys.objects, &owner)
    {
        w.style &= !window::WS_DISABLED;
    }
}

fn modal_loop(env: &mut Env<'_>, dlg: u32, owner: u32, owner_enabled: bool) -> Outcome {
    super::message::sent::drain(env, move |env| {
        modal_loop_inner(env, dlg, owner, owner_enabled)
    })
}

fn modal_loop_inner(env: &mut Env<'_>, dlg: u32, owner: u32, owner_enabled: bool) -> Outcome {
    let state = user(env)
        .windows
        .get(&env.sys.objects, &dlg)
        .map(|w| (w.control.dialog_end, w.destroying));
    let finish = move |env: &mut Env<'_>, result: u32| {
        destroy_window(env, dlg, move |env, _| {
            restore_owner(env, owner, owner_enabled);
            if owner != 0 {
                return window::activate_window(env, owner, move |_| Outcome::Return(result));
            }
            Outcome::Return(result)
        })
    };
    match state {
        None | Some((_, true)) => return finish(env, 0),
        Some((Some(r), _)) => return finish(env, r),
        _ => {}
    }
    match next_any(env) {
        Some(m) if m.msg == wm::QUIT => {
            // Leave WM_QUIT for the application's own loop.
            let ctx = env.context;
            user_mut(env).queue(ctx).quit = Some(m.wparam);
            finish(env, 0)
        }
        Some(m) => dispatch(env, m, move |env, _| {
            modal_loop(env, dlg, owner, owner_enabled)
        }),
        None => wait_for_message(env, move |env| modal_loop(env, dlg, owner, owner_enabled)),
    }
}

fn end_dialog(env: &mut Env<'_>) -> Outcome {
    let (dlg, result) = (env.arg(0), env.arg(1));
    let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &dlg)
    else {
        return Outcome::Return(0);
    };
    w.control.dialog_end = Some(result);
    show_window(env, dlg, SW_HIDE, |_, _| Outcome::Return(1))
}

fn get_dlg_item(env: &mut Env<'_>) -> Outcome {
    let (dlg, id) = (env.arg(0), env.arg(1));
    Outcome::Return(item(user(env), &env.sys.objects, dlg, id))
}

fn get_dlg_ctrl_id(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    Outcome::Return(
        user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .map_or(0, |w| w.id),
    )
}

fn send_dlg_item_message_a(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let h = item(user(env), &env.sys.objects, dlg, id);
    super::message::send_encoded(env, h, msg, wp, lp, false, |_, r| Outcome::Return(r))
}

fn set_dlg_item_text_a(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, text) = (env.arg(0), env.arg(1), env.arg(2));
    let h = item(user(env), &env.sys.objects, dlg, id);
    super::message::send_encoded(env, h, wm::SETTEXT, 0, text, false, |_, r| {
        Outcome::Return(r)
    })
}

fn get_dlg_item_text_a(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, buf, cap) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let h = item(user(env), &env.sys.objects, dlg, id);
    if h == 0 {
        if cap > 0 {
            let _ = env.cpu.write(buf, &[0]);
        }
        return Outcome::Return(0);
    }
    super::message::send_encoded(env, h, wm::GETTEXT, cap, buf, false, |_, r| {
        Outcome::Return(r)
    })
}

#[win32(user32, name="SendDlgItemMessageW", level=state)]
fn send_dlg_item_message_w(
    env: &mut Env<'_>,
    dlg: Handle,
    id: i32,
    msg: u32,
    wp: u32,
    lp: i32,
) -> Outcome {
    let hwnd = item(user(env), &env.sys.objects, dlg.0, id as u32);
    super::message::send_encoded(env, hwnd, msg, wp, lp as u32, true, |_, r| {
        Outcome::Return(r)
    })
}

#[win32(user32, name="SetDlgItemTextW", level=state)]
fn set_dlg_item_text_w(env: &mut Env<'_>, dlg: Handle, id: i32, text: WideStr) -> Outcome {
    let hwnd = item(user(env), &env.sys.objects, dlg.0, id as u32);
    super::message::send_encoded(env, hwnd, wm::SETTEXT, 0, text.0.addr(), true, |_, r| {
        Outcome::Return(r)
    })
}

#[win32(user32, name="GetDlgItemTextW", level=state)]
fn get_dlg_item_text_w(
    env: &mut Env<'_>,
    dlg: Handle,
    id: i32,
    text: WideStr,
    capacity: i32,
) -> Outcome {
    if capacity <= 0 {
        return Outcome::Return(0);
    }
    let hwnd = item(user(env), &env.sys.objects, dlg.0, id as u32);
    if hwnd == 0 {
        let _ = super::message::write_text(env, text.0.addr(), capacity as u32, "", true);
        return Outcome::Return(0);
    }
    super::message::send_encoded(
        env,
        hwnd,
        wm::GETTEXT,
        capacity as u32,
        text.0.addr(),
        true,
        |_, r| Outcome::Return(r),
    )
}

fn set_dlg_item_int(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, v, signed) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3) != 0);
    let s = if signed {
        (v as i32).to_string()
    } else {
        v.to_string()
    };
    let h = item(user(env), &env.sys.objects, dlg, id);
    let wide = user(env)
        .windows
        .get(&env.sys.objects, &h)
        .is_some_and(|w| w.unicode);
    let p = super::message::guest_text(env, &s, wide);
    if p == 0 {
        crate::util::set_last_error(env, 8);
        return Outcome::Return(0);
    }
    send(env, h, wm::SETTEXT, 0, p, move |env, r| {
        window::temp_free(env, p);
        Outcome::Return(r)
    })
}

fn get_dlg_item_int(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, ok_out, signed) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3) != 0);
    let u = user(env);
    let text = u
        .windows
        .get(&env.sys.objects, &item(u, &env.sys.objects, dlg, id))
        .map(|w| w.text.trim().to_owned())
        .unwrap_or_default();
    let v = if signed {
        text.parse::<i32>().ok().map(|v| v as u32)
    } else {
        text.parse::<u32>().ok()
    };
    if ok_out != 0 {
        let _ = env.cpu.write_u32(ok_out, u32::from(v.is_some()));
    }
    Outcome::Return(v.unwrap_or(0))
}

fn check_dlg_button(env: &mut Env<'_>) -> Outcome {
    let (dlg, id, check) = (env.arg(0), env.arg(1), env.arg(2));
    let h = item(user(env), &env.sys.objects, dlg, id);
    send(env, h, 0xf1, check, 0, |_, _| Outcome::Return(1))
}

fn is_dlg_button_checked(env: &mut Env<'_>) -> Outcome {
    let (dlg, id) = (env.arg(0), env.arg(1));
    let h = item(user(env), &env.sys.objects, dlg, id);
    send(env, h, 0xf0, 0, 0, |_, r| Outcome::Return(r))
}

fn check_radio_button(env: &mut Env<'_>) -> Outcome {
    let (dlg, first, last, check) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let msgs: VecDeque<[u32; 4]> = (first..=last)
        .map(|id| {
            [
                item(user(env), &env.sys.objects, dlg, id),
                0xf1,
                u32::from(id == check),
                0,
            ]
        })
        .filter(|m| m[0] != 0)
        .collect();
    super::message::send_all(env, msgs, |_| Outcome::Return(1))
}

fn map_dialog_rect(env: &mut Env<'_>) -> Outcome {
    let (dlg, p) = (env.arg(0), env.arg(1));
    let Some((bx, by)) = user(env)
        .windows
        .get(&env.sys.objects, &dlg)
        .map(|w| w.control.base_units)
    else {
        return Outcome::Return(0);
    };
    let Some(r) = Rect::read(env.cpu, p) else {
        return Outcome::Return(0);
    };
    let r = Rect {
        left: r.left * bx / 4,
        top: r.top * by / 8,
        right: r.right * bx / 4,
        bottom: r.bottom * by / 8,
    };
    Outcome::Return(u32::from(r.write(env.cpu, p)))
}

fn get_dialog_base_units(_env: &mut Env<'_>) -> Outcome {
    let (x, y) = base_units(None);
    Outcome::Return(window::make_long(x, y))
}

fn is_dialog_message_a(env: &mut Env<'_>) -> Outcome {
    // Keyboard navigation (Tab, Enter, Esc) is not emulated: the caller dispatches the
    // message itself. Enter/Esc still reach the dialog as WM_COMMAND IDOK/IDCANCEL.
    let (dlg, msg) = (env.arg(0), env.arg(1));
    is_dialog_message(env, dlg, msg)
}

#[win32(user32, name="IsDialogMessageW", level=state)]
fn is_dialog_message_w(env: &mut Env<'_>, dlg: Handle, msg: GuestPtr<()>) -> Outcome {
    is_dialog_message(env, dlg.0, msg.addr())
}

fn is_dialog_message(env: &mut Env<'_>, dlg: u32, msg: u32) -> Outcome {
    let m = super::message::Msg {
        hwnd: env.cpu.read_u32(msg).unwrap_or(0),
        msg: env.cpu.read_u32(msg + 4).unwrap_or(0),
        wparam: env.cpu.read_u32(msg + 8).unwrap_or(0),
        ..Default::default()
    };
    let u = user(env);
    let for_dialog = m.hwnd == dlg
        || u.windows
            .get(&env.sys.objects, &m.hwnd)
            .is_some_and(|w| u.root(&env.sys.objects, w.hwnd) == dlg);
    if for_dialog && m.msg == wm::KEYDOWN && (m.wparam == 0x0d || m.wparam == 0x1b) {
        let id = if m.wparam == 0x0d { 1 } else { 2 };
        let ctl = item(u, &env.sys.objects, dlg, id);
        return send(env, dlg, wm::COMMAND, id, ctl, |_, _| Outcome::Return(1));
    }
    Outcome::Return(0)
}

pub fn register(reg: &mut Registry) {
    for api in [
        create_dialog_param_w_ABI,
        create_dialog_indirect_param_w_ABI,
        dialog_box_param_w_ABI,
        dialog_box_indirect_param_w_ABI,
        send_dlg_item_message_w_ABI,
        set_dlg_item_text_w_ABI,
        get_dlg_item_text_w_ABI,
        is_dialog_message_w_ABI,
    ] {
        reg.register(api);
    }
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    s(reg, "CreateDialogParamA", 5, create_dialog_param_a);
    s(
        reg,
        "CreateDialogIndirectParamA",
        5,
        create_dialog_indirect_param_a,
    );
    s(reg, "DialogBoxParamA", 5, dialog_box_param_a);
    s(
        reg,
        "DialogBoxIndirectParamA",
        5,
        dialog_box_indirect_param_a,
    );
    s(reg, "EndDialog", 2, end_dialog);
    s(reg, "GetDlgItem", 2, get_dlg_item);
    s(reg, "GetDlgCtrlID", 1, get_dlg_ctrl_id);
    s(reg, "SendDlgItemMessageA", 5, send_dlg_item_message_a);
    s(reg, "SetDlgItemTextA", 3, set_dlg_item_text_a);
    s(reg, "GetDlgItemTextA", 4, get_dlg_item_text_a);
    s(reg, "SetDlgItemInt", 4, set_dlg_item_int);
    s(reg, "GetDlgItemInt", 4, get_dlg_item_int);
    s(reg, "CheckDlgButton", 3, check_dlg_button);
    s(reg, "IsDlgButtonChecked", 2, is_dlg_button_checked);
    s(reg, "CheckRadioButton", 4, check_radio_button);
    s(reg, "MapDialogRect", 2, map_dialog_rect);
    s(reg, "GetDialogBaseUnits", 0, get_dialog_base_units);
    s(reg, "IsDialogMessageA", 2, is_dialog_message_a);
}
