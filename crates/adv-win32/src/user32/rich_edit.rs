//! RichEdit's module-owned classes use the existing USER window and text owner.
//! Styled documents, streams and OLE objects remain explicitly unavailable.
use super::{
    USER32, message, user, user_mut,
    window::{Class, wm},
};
use crate::{Win32, stdcall, util::set_last_error};
use adv_abi::Registry;
use adv_cpu::backend::MemFault;
use adv_machine::{Env, Outcome};
use adv_os::{ModuleId, ModuleKind};

pub(super) const PROC: &str = "<RichEditWndProc>";
const CLASSES: [(&str, bool); 3] = [
    ("RICHEDIT", false),
    ("RichEdit20A", false),
    ("RichEdit20W", true),
];

pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, USER32, PROC, 4, procedure);
}

fn owns(env: &Env<'_>, id: ModuleId) -> bool {
    let module = adv_os::modules(env.ext).get(id);
    module.kind == ModuleKind::Host && module.name == "riched32.dll"
}

pub(crate) fn loaded(env: &mut Env<'_>, id: ModuleId) -> Result<bool, MemFault> {
    if !owns(env, id) {
        return Ok(true);
    }
    let procedure = env.sys.thunks.resolve(env.cpu, USER32, PROC)?;
    let user = user_mut(env);
    if user.classes.iter().any(|class| {
        class.instance == 0
            && CLASSES
                .iter()
                .any(|&(name, _)| name.eq_ignore_ascii_case(&class.name))
            && class.proc != procedure
    }) {
        return Ok(false);
    }
    for (name, unicode) in CLASSES {
        if user
            .classes
            .iter()
            .any(|class| class.name == name && class.instance == 0)
        {
            continue;
        }
        if let Some(class) = user.classes.iter_mut().find(|class| {
            class.name.is_empty() && class.proc == procedure && class.unicode == Some(unicode)
        }) {
            // Reuse the module's retired slots and atoms across repeated loads.
            *class = Class {
                name: name.into(),
                atom: class.atom,
                style: 0x4088,
                proc: procedure,
                cls_extra: 0,
                wnd_extra: 4,
                instance: 0,
                icon: 0,
                small_icon: 0,
                cursor: 0,
                background: 0,
                menu_name: 0,
                menu_text: None,
                menu_query_strings: [0; 2],
                unicode: Some(unicode),
            };
        } else {
            let atom = user.register_class(name, 0x4088, procedure, 4, 0, 0, 0, 0);
            user.classes
                .iter_mut()
                .find(|class| class.atom == atom)
                .unwrap()
                .unicode = Some(unicode);
        }
    }
    Ok(true)
}

pub(crate) fn released(env: &mut Env<'_>, id: ModuleId) {
    if !owns(env, id) || adv_os::modules(env.ext).get(id).load_count != 0 {
        return;
    }
    let Ok(procedure) = env.sys.thunks.resolve(env.cpu, USER32, PROC) else {
        return;
    };
    let mut strings = Vec::new();
    for class in &mut user_mut(env).classes {
        if class.instance == 0
            && class.proc == procedure
            && CLASSES
                .iter()
                .any(|&(name, _)| name.eq_ignore_ascii_case(&class.name))
        {
            // Keep stable class slots for windows through their destroy callbacks.
            class.name.clear();
            class.menu_text = None;
            strings.extend(std::mem::take(&mut class.menu_query_strings));
        }
    }
    for pointer in strings.into_iter().filter(|&pointer| pointer != 0) {
        super::window::temp_free(env, pointer);
    }
}

fn normalize(text: &str, multiline: bool, legacy: bool) -> String {
    if !multiline {
        return text.split(['\r', '\n']).next().unwrap_or_default().into();
    }
    if legacy {
        text.into()
    } else {
        let lines = text
            .replace("\r\r\n", " ")
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        lines.replace('\n', "\r\n")
    }
}

fn procedure(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        return Outcome::Return(0);
    };
    let (wide, multiline, legacy) = (
        window.unicode,
        window.style & 4 != 0,
        user(env).classes[window.class].name == "RICHEDIT",
    );
    match msg {
        wm::NCCREATE => {
            if window.text.starts_with("{\\rtf") {
                return unavailable(env);
            }
            let text = normalize(&window.text, multiline, legacy);
            env.ext
                .get_mut::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
                .unwrap()
                .text = text;
            Outcome::Return(1)
        }
        wm::SETTEXT => {
            let Some(text) = message::read_text(env, lp, wide) else {
                set_last_error(env, 87);
                return Outcome::Return(0);
            };
            if text.starts_with("{\\rtf") {
                return unavailable(env);
            }
            let text = normalize(&text, multiline, legacy);
            let window = env
                .ext
                .get_mut::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
                .unwrap();
            window.text = text;
            window.needs_paint = true;
            Outcome::Return(1)
        }
        wm::GETDLGCODE => Outcome::Return(if multiline { 0x8f } else { 0x8b }),
        wm::PAINT => {
            message::validate(env, hwnd);
            Outcome::Return(0)
        }
        0xb0..=0xdf | 0x400..=0x500 => {
            adv_abi::warn_unavailable(
                env,
                "RichEdit window message",
                "selection, formatting, streams and OLE messages are not implemented",
            );
            set_last_error(env, 50);
            Outcome::Return(0)
        }
        _ => message::def_window_proc(env, hwnd, msg, wp, lp, |_, result| Outcome::Return(result)),
    }
}

fn unavailable(env: &mut Env<'_>) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        "RichEdit rich text",
        "RTF document parsing is not implemented",
    );
    set_last_error(env, 50);
    Outcome::Return(0)
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn native_single_and_multiline_text() {
        assert_eq!(normalize("ab\ncd", false, true), "ab");
        assert_eq!(normalize("ab\ncd", true, true), "ab\ncd");
        assert_eq!(normalize("ab\ncd", true, false), "ab\r\ncd");
        assert_eq!(normalize("ab\r\ncd", true, false), "ab\r\ncd");
        assert_eq!(normalize("ab\r\r\ncd", true, false), "ab cd");
    }
}
