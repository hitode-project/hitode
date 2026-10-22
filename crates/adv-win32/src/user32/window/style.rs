//! Explicit style writes notify the live procedure around shared-window mutation.
use super::*;

pub(super) fn set(env: &mut Env<'_>, hwnd: u32, index: i32, new: u32) -> Outcome {
    let Some(window) = user(env).windows.get(&env.sys.objects, &hwnd) else {
        set_last_error(env, 1400);
        return Outcome::Return(0);
    };
    let old = if index == GWL_STYLE {
        window.style
    } else {
        window.ex_style
    };
    let data = temp_alloc(env, 8);
    if data == 0 {
        set_last_error(env, 8);
        return Outcome::Return(0);
    }
    if env.cpu.write_u32(data, old).is_err() || env.cpu.write_u32(data + 4, new).is_err() {
        temp_free(env, data);
        set_last_error(env, 8);
        return Outcome::Return(0);
    }
    send(
        env,
        hwnd,
        wm::STYLECHANGING,
        index as u32,
        data,
        move |env, _| {
            let Some(new) = env.cpu.read_u32(data + 4).ok() else {
                temp_free(env, data);
                set_last_error(env, 87);
                return Outcome::Return(0);
            };
            let Some(window) = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            else {
                temp_free(env, data);
                set_last_error(env, 1400);
                return Outcome::Return(0);
            };
            if index == GWL_STYLE {
                window.style = new;
            } else {
                window.ex_style = new;
            }
            // STYLESTRUCT.styleOld is notification data; only styleNew is editable.
            if env.cpu.write_u32(data, old).is_err() {
                temp_free(env, data);
                set_last_error(env, 87);
                return Outcome::Return(0);
            }
            send(
                env,
                hwnd,
                wm::STYLECHANGED,
                index as u32,
                data,
                move |env, _| {
                    temp_free(env, data);
                    Outcome::Return(old)
                },
            )
        },
    )
}
