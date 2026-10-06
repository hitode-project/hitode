//! File-drop admission is a property of the existing USER window, not a
//! separate shell registration. The frontend can inspect WS_EX_ACCEPTFILES.
use adv_abi::{Handle, Registry, win32};
use adv_machine::Env;

use crate::{Win32, util::set_last_error};

const WS_EX_ACCEPTFILES: u32 = 0x10;

#[win32(shell32, name = "DragAcceptFiles", level = implement)]
fn drag_accept_files(env: &mut Env<'_>, window: Handle, accept: i32) {
    let Some(window) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &window.0)
    else {
        set_last_error(env, 1400); // ERROR_INVALID_WINDOW_HANDLE
        return;
    };
    if accept != 0 {
        window.ex_style |= WS_EX_ACCEPTFILES;
    } else {
        window.ex_style &= !WS_EX_ACCEPTFILES;
    }
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(drag_accept_files_ABI);
}
