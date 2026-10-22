//! Layout properties share the existing GDI+ format objects on every backend.
use super::*;
mod arrays;

fn generic(env: &mut Env<'_>, out: GuestPtr<()>, typographic: bool) -> Outcome {
    if !clear(env, out.addr()) {
        return Outcome::Return(2);
    }
    let value = runtime(env)
        .and_then(|runtime| runtime.create(Create::GenericFormat(typographic)))
        .map(Value::Object);
    publish(env, out.addr(), value)
}
#[win32(gdiplus, name = "GdipStringFormatGetGenericDefault", level = implement)]
fn generic_default(env: &mut Env<'_>, out: GuestPtr<()>) -> Outcome {
    generic(env, out, false)
}
#[win32(gdiplus, name = "GdipStringFormatGetGenericTypographic", level = implement)]
fn generic_typographic(env: &mut Env<'_>, out: GuestPtr<()>) -> Outcome {
    generic(env, out, true)
}
fn change(env: &mut Env<'_>, format: u32, operation: Op<'_>) -> i32 {
    object(env, format)
        .and_then(|format| format.execute(operation))
        .err()
        .unwrap_or(0)
}
#[win32(gdiplus, name = "GdipSetStringFormatDigitSubstitution", level = implement)]
fn set_digits(env: &mut Env<'_>, format: GuestPtr<()>, language: u32, method: i32) -> i32 {
    change(
        env,
        format.addr(),
        Op::Digits(Some((language as u16, method))),
    )
}
#[win32(gdiplus, name = "GdipGetStringFormatDigitSubstitution", level = implement)]
fn get_digits(
    env: &mut Env<'_>,
    format: GuestPtr<()>,
    language: GuestPtr<u16>,
    method: GuestPtr<i32>,
) -> i32 {
    let call = (|| {
        for (pointer, size) in [(language.addr(), 2), (method.addr(), 4)] {
            if pointer != 0 {
                adv_abi::guest::check_range(env.cpu, pointer, size, Prot::WRITE).map_err(|_| 2)?;
            }
        }
        match object(env, format.addr())?.execute(Op::Digits(None))? {
            Reply::Digits(lang, sub) => {
                if !language.is_null() {
                    language.write(env.cpu, lang).map_err(|_| 2)?;
                }
                if !method.is_null() {
                    method.write(env.cpu, sub).map_err(|_| 2)?;
                }
                Ok(())
            }
            _ => Err(2),
        }
    })();
    call.err().unwrap_or(0)
}
#[win32(gdiplus, name = "GdipSetStringFormatHotkeyPrefix", level = implement)]
fn set_hotkey(env: &mut Env<'_>, format: GuestPtr<()>, value: i32) -> i32 {
    change(env, format.addr(), Op::Hotkey(value))
}
#[win32(gdiplus, name = "GdipGetStringFormatHotkeyPrefix", level = implement)]
fn get_hotkey(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    properties::property(env, format.addr(), out, Op::StringProperty(4))
}
#[win32(gdiplus, name = "GdipSetStringFormatTrimming", level = implement)]
fn set_trimming(env: &mut Env<'_>, format: GuestPtr<()>, value: i32) -> i32 {
    change(env, format.addr(), Op::Trimming(value))
}
#[win32(gdiplus, name = "GdipGetStringFormatTrimming", level = implement)]
fn get_trimming(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    properties::property(env, format.addr(), out, Op::StringProperty(5))
}
#[win32(gdiplus, name = "GdipGetStringFormatMeasurableCharacterRangeCount", level = implement)]
fn range_count(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    properties::property(env, format.addr(), out, Op::StringProperty(6))
}
#[win32(gdiplus, name = "GdipGetStringFormatTabStopCount", level = implement)]
fn tab_count(env: &mut Env<'_>, format: GuestPtr<()>, out: GuestPtr<i32>) -> i32 {
    properties::property(env, format.addr(), out, Op::StringProperty(7))
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        generic_default_ABI,
        generic_typographic_ABI,
        set_digits_ABI,
        get_digits_ABI,
        set_hotkey_ABI,
        get_hotkey_ABI,
        set_trimming_ABI,
        get_trimming_ABI,
        range_count_ABI,
        tab_count_ABI,
    ] {
        registry.register(api);
    }
    arrays::register(registry);
}
