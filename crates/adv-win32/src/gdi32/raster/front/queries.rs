//! Device origins and palette queries read the shared GDI and USER owners.
use super::*;
#[win32(gdi32,name="GetDCOrgEx",level=implement)]
fn dc_origin(env: &mut Env<'_>, dc: Handle, out: GuestPtr<[i32; 2]>) -> Outcome {
    if !matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0),
        Some(adv_os::gdi::GdiObj::Dc { .. })
    ) {
        return fail(env, 6);
    }
    let lease = env
        .ext
        .get::<State>()
        .window_leases
        .iter()
        .rev()
        .find(|lease| lease.dc == dc.0);
    let origin = if let Some(lease) = lease.filter(|lease| lease.hwnd != 0) {
        let u = &env.ext.get::<Win32>().user;
        if lease.usage == WindowDcUse::Window {
            u.screen_rect(&env.sys.objects, lease.hwnd)
                .map(|r| [r.left, r.top])
                .unwrap_or([0, 0])
        } else {
            let (x, y) = u.client_origin(&env.sys.objects, lease.hwnd);
            [x, y]
        }
    } else if let Some((&hwnd, _)) = env
        .ext
        .get::<State>()
        .private_dcs
        .iter()
        .find(|(_, handle)| **handle == dc.0)
    {
        let (x, y) = env
            .ext
            .get::<Win32>()
            .user
            .client_origin(&env.sys.objects, hwnd);
        [x, y]
    } else {
        [0, 0]
    };
    if adv_abi::guest::check_range(env.cpu, out.addr(), 8, Prot::WRITE).is_err()
        || out.write(env.cpu, origin).is_err()
    {
        return fail(env, 87);
    }
    Outcome::Return(1)
}
#[win32(gdi32,name="GetSystemPaletteEntries",level=implement)]
fn system_palette(
    env: &mut Env<'_>,
    dc: Handle,
    _start: u32,
    _count: u32,
    _entries: GuestPtr<()>,
) -> u32 {
    if !matches!(
        env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0),
        Some(adv_os::gdi::GdiObj::Dc { .. })
    ) {
        set_last_error(env, 6);
    }
    // The common display profile has no physical palette. Logical palettes
    // retain their independent shared GDI payloads and are queried separately.
    0
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(dc_origin_ABI);
    registry.register(system_palette_ABI);
}
