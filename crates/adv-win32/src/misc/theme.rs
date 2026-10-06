//! The guest UI has classic controls/colors, without visual styles or theme handles.
//! This capability never queries or changes the host Windows theme.
use adv_abi::{Env, Registry, win32};

const VISUAL_STYLES_AVAILABLE: bool = false;

fn visual_styles(env: &mut Env<'_>, api: &str) -> i32 {
    adv_abi::warn_unavailable(
        env,
        api,
        "classic guest controls/colors; visual styles and theme handles are unavailable",
    );
    // FALSE is the supported capability result, not an API failure. In
    // particular, neither query changes the guest thread's LastError.
    i32::from(VISUAL_STYLES_AVAILABLE)
}

#[win32(uxtheme, name = "IsAppThemed", level = state)]
fn is_app_themed(env: &mut Env<'_>) -> i32 {
    visual_styles(env, "uxtheme.dll!IsAppThemed")
}

#[win32(uxtheme, name = "IsThemeActive", level = state)]
fn is_theme_active(env: &mut Env<'_>) -> i32 {
    visual_styles(env, "uxtheme.dll!IsThemeActive")
}

pub(super) fn register(reg: &mut Registry) {
    reg.register(is_app_themed_ABI);
    reg.register(is_theme_active_ABI);
}
