//! COPY presentation composes rectangles into a device-owned shared front surface.
use super::*;
use adv_device::surface::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Copy {
    width: u32,
    height: u32,
    transfer: Option<Transfer>,
}
impl Copy {
    pub(super) fn key(self) -> u64 {
        super::recording::hash((
            self.width,
            self.height,
            self.transfer
                .map(|t| (super::recording::rect(t.source), t.x, t.y)),
        ))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transfer {
    source: Rect,
    x: u32,
    y: u32,
}

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
) -> Result<Option<Copy>, u32> {
    if parameters.SwapEffect != 3 {
        return if source.is_null()
            && destination.is_null()
            && override_window == 0
            && dirty.is_null()
        {
            Ok(None)
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
    if window != parameters.hDeviceWindow {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::Present",
            "COPY presentation to another destination window is unavailable",
        );
        return Err(UNAVAILABLE);
    }
    let bounds = |width: u32, height: u32| RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || parameters.BackBufferWidth == 0
        || parameters.BackBufferHeight == 0
        || parameters.BackBufferWidth > i32::MAX as u32
        || parameters.BackBufferHeight > i32::MAX as u32
    {
        return Err(INVALID);
    }
    let source =
        source.unwrap_or_else(|| bounds(parameters.BackBufferWidth, parameters.BackBufferHeight));
    let destination = destination.unwrap_or_else(|| bounds(width, height));
    let source_width = i64::from(source.right) - i64::from(source.left);
    let source_height = i64::from(source.bottom) - i64::from(source.top);
    if source_width != i64::from(destination.right) - i64::from(destination.left)
        || source_height != i64::from(destination.bottom) - i64::from(destination.top)
    {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::Present",
            "COPY presentation with rectangle stretching is unavailable",
        );
        return Err(UNAVAILABLE);
    }
    // Equal-size rectangles are a translation. Clip both rectangles together,
    // retaining the original source/destination pixel correspondence.
    let dx = i64::from(destination.left) - i64::from(source.left);
    let dy = i64::from(destination.top) - i64::from(source.top);
    let left = i64::from(source.left).max(0).max(-dx);
    let top = i64::from(source.top).max(0).max(-dy);
    let right = i64::from(source.right)
        .min(i64::from(parameters.BackBufferWidth))
        .min(i64::from(width) - dx);
    let bottom = i64::from(source.bottom)
        .min(i64::from(parameters.BackBufferHeight))
        .min(i64::from(height) - dy);
    let transfer = (left < right && top < bottom).then(|| Transfer {
        source: Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        },
        x: (left + dx) as u32,
        y: (top + dy) as u32,
    });
    // RGNDATA is an optimization hint. D3D9 explicitly allows copying the whole
    // source rectangle instead of only the hinted pixels.
    Ok(Some(Copy {
        width,
        height,
        transfer,
    }))
}

pub(super) fn execute(
    env: &mut Env<'_>,
    gpu: &mut Gpu,
    device: Id,
    back_buffer: u32,
    copy: Copy,
) -> Result<u64, Error> {
    let provider = env.ext.get::<State>().provider;
    let old_front = match com(env).get_identity(&env.sys.objects, device) {
        Some(Payload::Device(d)) => d.front,
        _ => return Err(Error::Invalid("COPY device no longer exists".into())),
    };
    let old_target = old_front.and_then(|surface| (provider.target)(env.ext, surface));
    let source = (provider.target)(env.ext, back_buffer)
        .ok_or_else(|| Error::Invalid("COPY back buffer no longer exists".into()))?;
    let (front, target) = if let (Some(front), Some(target)) = (old_front, old_target.as_ref())
        && (target.image.width(), target.image.height()) == (copy.width, copy.height)
    {
        (front, target.clone())
    } else {
        let target = gpu
            .target(copy.width, copy.height, false)?
            .with_alpha_mode(source.image.alpha_mode());
        gpu.clear(&target, Some([0.0, 0.0, 0.0, 1.0]), None)?;
        if let Some(old) = old_target {
            gpu.copy(
                &old.image,
                Rect {
                    x: 0,
                    y: 0,
                    w: copy.width.min(old.image.width()),
                    h: copy.height.min(old.image.height()),
                },
                &target.image,
                0,
                0,
            )?;
        }
        let front = if let Some(front) = old_front {
            (provider.replace)(env.ext, front, target.clone()).map_err(Error::Invalid)?;
            front
        } else {
            let front = (provider.insert)(env.ext, target.clone()).map_err(Error::Unavailable)?;
            let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device)
            else {
                let _ = (provider.remove)(env.ext, env.cpu, front);
                return Err(Error::Invalid("COPY device no longer exists".into()));
            };
            d.front = Some(front);
            front
        };
        (front, target)
    };
    if let Some(transfer) = copy.transfer {
        gpu.copy(
            &source.image,
            transfer.source,
            &target.image,
            transfer.x,
            transfer.y,
        )?;
    }
    // This callback snapshots, never rotates the front: future partial COPYs
    // must retain pixels outside their destination rectangle.
    (provider.present_copy)(env.ext, front, gpu)
}

pub(super) fn release(env: &mut Env<'_>, front: Option<u32>) {
    if let Some(front) = front {
        let provider = env.ext.get::<State>().provider;
        let _ = (provider.remove)(env.ext, env.cpu, front);
    }
}

#[cfg(test)]
mod tests;
