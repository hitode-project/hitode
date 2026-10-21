//! The guest has no IME provider, HIMC owner or host composition bridge.
//! Query/set fronts preserve native absent-context errors; disabling an already
//! absent association needs no fabricated handle or per-window context cache.
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_os::global::Globals;

const UNAVAILABLE: &str = "guest IME/input-context provider and host composition are unavailable";

fn warn(env: &mut Env<'_>, api: &str) {
    adv_abi::warn_unavailable(env, api, UNAVAILABLE);
}

pub(crate) fn get_context(env: &mut Env<'_>, window: Handle) -> Handle {
    // No context exists for an invalid HWND either. Native NULL HWND fails
    // without changing LastError, rather than inventing a context association.
    if env
        .ext
        .get::<Win32>()
        .user
        .is_window(&env.sys.objects, window.0)
    {
        warn(env, "imm32.dll!ImmGetContext");
    }
    Handle(0)
}

fn getter_failure(env: &mut Env<'_>, context: Handle, api: &str) -> i32 {
    if context.0 == 0 {
        warn(env, api);
        // Both native NULL-HIMC getters leave the caller's LastError intact.
    } else {
        set_last_error(env, 6);
    }
    0
}

#[win32(imm32, name = "ImmGetConversionStatus", level = safe_fail)]
fn get_conversion(
    env: &mut Env<'_>,
    context: Handle,
    _conversion: GuestPtr<u32>,
    _sentence: GuestPtr<u32>,
) -> i32 {
    // Validate HIMC before either output. No valid context can exist in this
    // profile, so even NULL/readonly/wrapping outputs are never accessed.
    getter_failure(env, context, "imm32.dll!ImmGetConversionStatus")
}

#[win32(imm32, name = "ImmGetOpenStatus", level = state)]
fn get_open(env: &mut Env<'_>, context: Handle) -> i32 {
    getter_failure(env, context, "imm32.dll!ImmGetOpenStatus")
}

fn setter_failure(env: &mut Env<'_>, api: &str) -> i32 {
    warn(env, api);
    // Native setters reject NULL HIMC with ERROR_INVALID_HANDLE, including
    // attempts to close/clear the modes. Invalid context precedes mode flags.
    set_last_error(env, 6);
    0
}

#[win32(imm32, name = "ImmSetConversionStatus", level = safe_fail)]
fn set_conversion(env: &mut Env<'_>, _context: Handle, _conversion: u32, _sentence: u32) -> i32 {
    setter_failure(env, "imm32.dll!ImmSetConversionStatus")
}

#[win32(imm32, name = "ImmSetOpenStatus", level = safe_fail)]
fn set_open(env: &mut Env<'_>, _context: Handle, _open: i32) -> i32 {
    setter_failure(env, "imm32.dll!ImmSetOpenStatus")
}

#[win32(imm32, name = "ImmReleaseContext", level = state)]
fn release(_env: &mut Env<'_>, _window: Handle, _context: Handle) -> i32 {
    // Native ReleaseContext is a no-op TRUE even for invalid HWND/HIMC and
    // preserves LastError. There are no borrowed HIMC references to release.
    // In particular, an unrelated Objects handle is never closed here.
    1
}

#[win32(imm32, name = "ImmAssociateContext", level = state)]
fn associate(env: &mut Env<'_>, window: Handle, context: Handle) -> Handle {
    if context.0 != 0 {
        warn(env, "imm32.dll!ImmAssociateContext");
        set_last_error(env, 6);
        return Handle(0);
    }
    if env
        .ext
        .get::<Win32>()
        .user
        .is_window(&env.sys.objects, window.0)
    {
        // This is a real, already-disabled guest association with previous
        // context NULL. No host IME, handle or input route is created.
        set_last_error(env, 0);
    }
    Handle(0)
}

#[win32(user32, name = "SendIMEMessageExA", level = safe_fail)]
fn send_legacy_a(env: &mut Env<'_>, _window: Handle, message: i32) -> i32 {
    legacy_message(env, message as u32, "user32.dll!SendIMEMessageExA")
}

#[win32(user32, name = "SendIMEMessageExW", level = safe_fail)]
fn send_legacy_w(env: &mut Env<'_>, _window: Handle, message: i32) -> i32 {
    legacy_message(env, message as u32, "user32.dll!SendIMEMessageExW")
}

fn legacy_message(env: &mut Env<'_>, message: u32, api: &str) -> i32 {
    // LPARAM is an HGLOBAL containing the SDK's 32-byte x86 IMESTRUCT,
    // not a pointer to the structure. Borrow the existing global-memory owner;
    // this absent-provider path neither changes lock counts nor takes ownership.
    let Some((pointer, size)) = env
        .ext
        .try_get::<Globals>()
        .and_then(|globals| globals.get(&env.sys.objects, message))
        .map(|block| (block.pointer, block.size))
    else {
        set_last_error(env, 6);
        return 0;
    };
    if pointer == 0 {
        set_last_error(env, 157); // ERROR_DISCARDED, matching GlobalLock.
        return 0;
    }
    if size < 32 {
        set_last_error(env, 87);
        return 0;
    }
    if adv_abi::guest::check_range(env.cpu, pointer, 32, Prot::READ).is_err()
        || adv_abi::guest::check_range(env.cpu, pointer + 4, 4, Prot::WRITE).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    let Ok(function) = env.cpu.read_u32(pointer) else {
        set_last_error(env, 998);
        return 0;
    };
    // ime.h specifies IME_RS_NOIME (2) in wParam when no IME is installed.
    // Zero is a failed result, not a fabricated open state or conversion result.
    if env.cpu.write_u32(pointer + 4, 2).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    adv_abi::warn_unavailable(
        env,
        api,
        &format!("{UNAVAILABLE}; legacy IME subfunction {function:#x}"),
    );
    0
}

/// Composition, candidate and font fronts with no HIMC: NULL getters keep
/// LastError like the status getters, everything else is ERROR_INVALID_HANDLE.
/// Outputs and inputs are never touched. Committed text still arrives as
/// WM_IME_CHAR (WIN32-USER-12).
fn no_context(env: &mut Env<'_>, context: Handle, getter: bool) -> i32 {
    if !(getter && context.0 == 0) {
        set_last_error(env, 6);
    }
    0
}

#[win32(imm32, name = "ImmGetCompositionStringA", level = safe_fail)]
fn get_composition_a(
    env: &mut Env<'_>,
    context: Handle,
    _index: u32,
    _buffer: GuestPtr<()>,
    _length: u32,
) -> i32 {
    getter_failure(env, context, "imm32.dll!ImmGetCompositionStringA")
}

#[win32(imm32, name = "ImmGetCompositionStringW", level = safe_fail)]
fn get_composition_w(
    env: &mut Env<'_>,
    context: Handle,
    _index: u32,
    _buffer: GuestPtr<()>,
    _length: u32,
) -> i32 {
    getter_failure(env, context, "imm32.dll!ImmGetCompositionStringW")
}

/// Cancelling or completing a composition that cannot exist needs no warning.
#[win32(imm32, name = "ImmNotifyIME", level = safe_fail)]
fn notify(env: &mut Env<'_>, context: Handle, _action: u32, _index: u32, _value: u32) -> i32 {
    no_context(env, context, false)
}

#[win32(imm32, name = "ImmSetCompositionWindow", level = safe_fail)]
fn set_composition_window(env: &mut Env<'_>, context: Handle, _form: GuestPtr<()>) -> i32 {
    no_context(env, context, false)
}

#[win32(imm32, name = "ImmGetCompositionWindow", level = safe_fail)]
fn get_composition_window(env: &mut Env<'_>, context: Handle, _form: GuestPtr<()>) -> i32 {
    no_context(env, context, true)
}

#[win32(imm32, name = "ImmSetCandidateWindow", level = safe_fail)]
fn set_candidate_window(env: &mut Env<'_>, context: Handle, _form: GuestPtr<()>) -> i32 {
    no_context(env, context, false)
}

#[win32(imm32, name = "ImmSetCompositionFontA", level = safe_fail)]
fn set_composition_font_a(env: &mut Env<'_>, context: Handle, _font: GuestPtr<()>) -> i32 {
    no_context(env, context, false)
}

#[win32(imm32, name = "ImmSetCompositionFontW", level = safe_fail)]
fn set_composition_font_w(env: &mut Env<'_>, context: Handle, _font: GuestPtr<()>) -> i32 {
    no_context(env, context, false)
}

#[win32(imm32, name = "ImmGetCompositionFontA", level = safe_fail)]
fn get_composition_font_a(env: &mut Env<'_>, context: Handle, _font: GuestPtr<()>) -> i32 {
    no_context(env, context, true)
}

#[win32(imm32, name = "ImmGetCompositionFontW", level = safe_fail)]
fn get_composition_font_w(env: &mut Env<'_>, context: Handle, _font: GuestPtr<()>) -> i32 {
    no_context(env, context, true)
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_composition_a_ABI,
        get_composition_w_ABI,
        notify_ABI,
        set_composition_window_ABI,
        get_composition_window_ABI,
        set_candidate_window_ABI,
        set_composition_font_a_ABI,
        set_composition_font_w_ABI,
        get_composition_font_a_ABI,
        get_composition_font_w_ABI,
        get_conversion_ABI,
        get_open_ABI,
        set_conversion_ABI,
        set_open_ABI,
        release_ABI,
        associate_ABI,
        send_legacy_a_ABI,
        send_legacy_w_ABI,
    ] {
        registry.register(api);
    }
}
