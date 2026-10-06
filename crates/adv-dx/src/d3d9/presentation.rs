//! Whole-backbuffer COPY presentation through the shared GPU snapshot. Partial
//! destination composition requires a front surface and is explicitly unavailable.
use super::*;

fn dirty_region(env: &Env<'_>, dirty: GuestPtr<()>) -> Result<(), u32> {
    if dirty.is_null() {
        return Ok(());
    }
    let header = GuestPtr::<[u32; 8]>::new(dirty.addr())
        .read(env.cpu)
        .map_err(|_| INVALID)?;
    let bytes = header[2].checked_mul(16).ok_or(INVALID)?;
    if header[0] != 32 || header[1] != 1 || (header[3] != 0 && header[3] < bytes) {
        return Err(INVALID);
    }
    let length = 32u32.checked_add(bytes.max(header[3])).ok_or(INVALID)?;
    adv_abi::guest::check_range(env.cpu, dirty.addr(), length, Prot::READ).map_err(|_| INVALID)
}

pub(super) fn validate(
    env: &mut Env<'_>,
    parameters: D3DPRESENT_PARAMETERS,
    source: GuestPtr<RECT>,
    destination: GuestPtr<RECT>,
    override_window: u32,
    dirty: GuestPtr<()>,
) -> Result<(), u32> {
    if parameters.SwapEffect != 3 {
        return if source.is_null()
            && destination.is_null()
            && override_window == 0
            && dirty.is_null()
        {
            Ok(())
        } else {
            Err(INVALID)
        };
    }
    let source = if source.is_null() {
        None
    } else {
        Some(source.read(env.cpu).map_err(|_| INVALID)?)
    };
    let destination = if destination.is_null() {
        None
    } else {
        Some(destination.read(env.cpu).map_err(|_| INVALID)?)
    };
    dirty_region(env, dirty)?;
    let provider = env.ext.get::<State>().provider;
    let window = if override_window == 0 {
        parameters.hDeviceWindow
    } else {
        override_window
    };
    let (width, height) = (provider.window)(env.ext, &env.sys.objects, window).ok_or(INVALID)?;
    for rectangle in [source, destination].into_iter().flatten() {
        if rectangle.right <= rectangle.left || rectangle.bottom <= rectangle.top {
            return Err(INVALID);
        }
    }
    let whole = |r: RECT, width: u32, height: u32| {
        r.left == 0
            && r.top == 0
            && i64::from(r.right) == i64::from(width)
            && i64::from(r.bottom) == i64::from(height)
    };
    if window != parameters.hDeviceWindow
        || source
            .is_some_and(|r| !whole(r, parameters.BackBufferWidth, parameters.BackBufferHeight))
        || destination.is_some_and(|r| !whole(r, width, height))
    {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::Present",
            &format!(
                "COPY partial rectangles or another destination window require front-buffer composition; source={source:?}, destination={destination:?}, window={window:#x}, backbuffer={}x{}, client={width}x{height}",
                parameters.BackBufferWidth, parameters.BackBufferHeight
            ),
        );
        return Err(UNAVAILABLE);
    }
    // RGNDATA is an optimization hint. D3D9 explicitly allows copying the whole
    // source instead of only the hinted pixels. The backend copies to a snapshot
    // without modifying the implicit back buffer, satisfying COPY preservation.
    Ok(())
}

#[cfg(test)]
mod tests;
