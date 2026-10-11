//! Non-stretching scan-band uploads over the existing DIB and surface owners.
use super::*;

#[win32(gdi32, name = "SetDIBitsToDevice", level = implement)]
fn set_dibits_to_device(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    sx: i32,
    sy: i32,
    start: u32,
    count: u32,
    bits: GuestPtr<()>,
    info: GuestPtr<()>,
    usage: u32,
) -> Outcome {
    const API: &str = "gdi32.dll!SetDIBitsToDevice";
    if unavailable_pixels(env, dc.0, API) {
        return Outcome::Return(0);
    }
    if !available(env) {
        return fail(env, 6);
    }
    let provider = provider(env.ext).unwrap();
    let logical = super::super::super::palette::dc_entries(env, dc.0);
    let decoded = match super::super::dib::read(
        &(provider.memory)(env.cpu),
        info.addr(),
        usage,
        Some(&logical),
    ) {
        Ok(Some(dib)) => dib,
        _ => return fail(env, 87),
    };
    if width == 0 || height == 0 || count == 0 {
        return Outcome::Return(0);
    }
    // Windows accepts some out-of-range top-down bands with driver-dependent
    // buffer addressing. Keep that range explicit until it has an oracle.
    if !decoded.bottom_up && u64::from(start) + u64::from(count) > u64::from(decoded.height) {
        adv_abi::warn_unavailable(
            env,
            API,
            "top-down scan bands outside the DIB are unavailable",
        );
        return fail(env, 50);
    }
    let lines = count.min(decoded.height.saturating_sub(start));
    if lines != 0
        && (bits.is_null()
            || adv_abi::guest::check_range(env.cpu, bits.addr(), decoded.pitch * lines, Prot::READ)
                .is_err())
    {
        return fail(env, 87);
    }
    raster_call_named(env, Some(API), |r, s, d, o, m| {
        r.set_dibits_to_device(
            s,
            d,
            o,
            m,
            dc.0,
            x,
            y,
            width,
            height,
            sx,
            sy,
            start,
            lines,
            bits.addr(),
            decoded,
        )
    })
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(set_dibits_to_device_ABI);
}
