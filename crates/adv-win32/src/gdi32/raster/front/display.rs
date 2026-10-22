//! A/W display drawing/information contexts share device validation and GDI ownership.
use super::*;
use adv_abi::{AnsiStr, WideStr};

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_a_ABI);
    registry.register(create_w_ABI);
    registry.register(information_a_ABI);
    registry.register(information_w_ABI);
}
fn ansi(env: &Env<'_>, value: AnsiStr) -> Result<Option<String>, ()> {
    if value.0.is_null() {
        return Ok(None);
    }
    let bytes = value.read(env.cpu, 4096).map_err(|_| ())?;
    Ok(Some(env.ext.get::<Win32>().codec.decode(&bytes)))
}
fn wide(env: &Env<'_>, value: WideStr) -> Result<Option<String>, ()> {
    if value.0.is_null() {
        return Ok(None);
    }
    Ok(Some(String::from_utf16_lossy(
        &value.read(env.cpu, 4096).map_err(|_| ())?,
    )))
}
fn is_display(name: &str) -> bool {
    name.eq_ignore_ascii_case("DISPLAY") || name.eq_ignore_ascii_case(r"\\.\DISPLAY1")
}
fn create(
    env: &mut Env<'_>,
    api: &str,
    driver: Option<String>,
    device: Option<String>,
    mode: GuestPtr<()>,
    information: bool,
) -> Outcome {
    if driver.is_none() && device.is_none() {
        return fail(env, 87);
    }
    if driver.as_deref().is_some_and(|name| !is_display(name))
        || device.as_deref().is_some_and(|name| !is_display(name))
    {
        // The configured spooler owns no printers. A missing printer name
        // is an ordinary catalog lookup failure, not a missing raster service.
        let unknown_driver = driver
            .as_deref()
            .is_some_and(|name| !is_display(name) && !name.eq_ignore_ascii_case("WINSPOOL"));
        return fail(env, if unknown_driver { 1797 } else { 1801 });
    }
    // SDK DISPLAY initialization uses the current display mode. Matching or
    // custom DEVMODE profiles need validation beyond this default-mode path.
    if !mode.is_null() {
        adv_abi::warn_unavailable(
            env,
            api,
            "non-NULL DEVMODE display initialization is unavailable",
        );
        return fail(env, 50);
    }
    if information {
        // Query-only contexts have the common font/palette/DC payload without
        // a raster bitmap, surface allocation or presentation side effect.
        let dc = super::super::super::window_dc(env, 0);
        return if dc == 0 {
            fail(env, 8)
        } else {
            Outcome::Return(dc)
        };
    }
    if !available(env) {
        adv_abi::warn_unavailable(env, api, "display raster provider is not installed");
        return fail(env, 50);
    }
    match raster_call(env, |r, s, d, o, m| r.create_display_dc(s, d, o, m)) {
        Outcome::Fail(_) => {
            adv_abi::warn_unavailable(env, api, "display surface allocation is unavailable");
            fail(env, 8)
        }
        result => result,
    }
}

#[win32(gdi32, name = "CreateDCA", level = implement)]
fn create_a(
    env: &mut Env<'_>,
    driver: AnsiStr,
    device: AnsiStr,
    _port: AnsiStr,
    mode: GuestPtr<()>,
) -> Outcome {
    let (Ok(driver), Ok(device)) = (ansi(env, driver), ansi(env, device)) else {
        return fail(env, 87);
    };
    // The obsolete port argument is ignored by the SDK, even if non-NULL.
    create(env, "gdi32.dll!CreateDCA", driver, device, mode, false)
}
#[win32(gdi32, name = "CreateDCW", level = implement)]
fn create_w(
    env: &mut Env<'_>,
    driver: WideStr,
    device: WideStr,
    _port: WideStr,
    mode: GuestPtr<()>,
) -> Outcome {
    let (Ok(driver), Ok(device)) = (wide(env, driver), wide(env, device)) else {
        return fail(env, 87);
    };
    create(env, "gdi32.dll!CreateDCW", driver, device, mode, false)
}

#[win32(gdi32, name = "CreateICA", level = state)]
fn information_a(
    env: &mut Env<'_>,
    driver: AnsiStr,
    device: AnsiStr,
    _port: AnsiStr,
    mode: GuestPtr<()>,
) -> Outcome {
    let (Ok(driver), Ok(device)) = (ansi(env, driver), ansi(env, device)) else {
        return fail(env, 87);
    };
    create(env, "gdi32.dll!CreateICA", driver, device, mode, true)
}

#[win32(gdi32, name = "CreateICW", level = state)]
fn information_w(
    env: &mut Env<'_>,
    driver: WideStr,
    device: WideStr,
    _port: WideStr,
    mode: GuestPtr<()>,
) -> Outcome {
    let (Ok(driver), Ok(device)) = (wide(env, driver), wide(env, device)) else {
        return fail(env, 87);
    };
    create(env, "gdi32.dll!CreateICW", driver, device, mode, true)
}

#[cfg(test)]
mod tests;
