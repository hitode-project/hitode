//! Equal-size color blits use the same GPU copy and COM owners as presentation.
//! Scaling, texture-source profiles and depth/MSAA copies remain unavailable.
use super::*;
use adv_device::surface::Rect;

pub(super) fn register(registry: &mut Registry) {
    registry.register(stretch_ABI);
}

fn rectangle(env: &Env<'_>, pointer: GuestPtr<RECT>, width: u32, height: u32) -> Result<Rect, u32> {
    let value = if pointer.is_null() {
        RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        }
    } else {
        pointer.read(env.cpu).map_err(|_| INVALID)?
    };
    if value.left < 0
        || value.top < 0
        || value.right <= value.left
        || value.bottom <= value.top
        || value.right as u32 > width
        || value.bottom as u32 > height
    {
        return Err(INVALID);
    }
    Ok(Rect {
        x: value.left as u32,
        y: value.top as u32,
        w: (value.right - value.left) as u32,
        h: (value.bottom - value.top) as u32,
    })
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, "IDirect3DDevice9::StretchRect", reason);
    Outcome::Return(UNAVAILABLE)
}

#[win32(d3d9, name = "IDirect3DDevice9::StretchRect", level = implement)]
fn stretch(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    source: GuestPtr<()>,
    source_rect: GuestPtr<RECT>,
    destination: GuestPtr<()>,
    destination_rect: GuestPtr<RECT>,
    filter: i32,
) -> Outcome {
    let Some(owner) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| device(env, this.addr()).is_some())
    else {
        return Outcome::Return(INVALID);
    };
    let Some(src) = resources::resource(env, source.addr()) else {
        return Outcome::Return(INVALID);
    };
    let Some(dst) = resources::resource(env, destination.addr()) else {
        return Outcome::Return(INVALID);
    };
    if src.kind != resources::Kind::Surface
        || dst.kind != resources::Kind::Surface
        || src.device != owner
        || dst.device != owner
        || src.desc.Pool != 0
        || dst.desc.Pool != 0
        || resources::locked(env, src)
        || resources::locked(env, dst)
        || !(0..=2).contains(&filter)
    {
        return Outcome::Return(INVALID);
    }
    if src.depth.is_some()
        || dst.depth.is_some()
        || src.desc.MultiSampleType != 0
        || dst.desc.MultiSampleType != 0
        || src.desc.MultiSampleQuality != 0
        || dst.desc.MultiSampleQuality != 0
    {
        return unavailable(env, "depth-stencil and multisample blits are unavailable");
    }
    if src.container.is_some() || dst.container.is_some() {
        return unavailable(
            env,
            "texture-level StretchRect requires an unsupported texture-source profile",
        );
    }
    // The shared color images implement these byte-compatible formats. X in
    // format 22 is unused. Conversion 22 -> 21 would need defined alpha.
    if !matches!(
        (src.desc.Format, dst.desc.Format),
        (21, 21) | (22, 22) | (21, 22)
    ) {
        return unavailable(
            env,
            "color blits support 21/22 and 21-to-22; other format conversions are unavailable",
        );
    }
    if dst.desc.Usage == 0 && src.desc.Usage != 0 {
        return unavailable(
            env,
            "render-target to offscreen-plain blits are unavailable",
        );
    }
    let from = match rectangle(env, source_rect, src.desc.Width, src.desc.Height) {
        Ok(rect) => rect,
        Err(error) => return Outcome::Return(error),
    };
    let to = match rectangle(env, destination_rect, dst.desc.Width, dst.desc.Height) {
        Ok(rect) => rect,
        Err(error) => return Outcome::Return(error),
    };
    if (from.w, from.h) != (to.w, to.h) {
        return unavailable(
            env,
            "rectangle dimensions differ; StretchRect scaling is unavailable",
        );
    }
    let source_id = com(env)
        .identity::<Payload>(&env.sys.objects, source.addr())
        .unwrap();
    let destination_id = com(env)
        .identity::<Payload>(&env.sys.objects, destination.addr())
        .unwrap();
    if source_id == destination_id {
        return unavailable(env, "same-surface StretchRect is unavailable");
    }
    let provider = env.ext.get::<State>().provider;
    let Some(source) = src
        .surface
        .and_then(|id| (provider.target)(env.ext, id))
        .map(|t| t.image)
    else {
        return unavailable(env, "source surface has no shared GPU image");
    };
    let Some(destination) = dst
        .surface
        .and_then(|id| (provider.target)(env.ext, id))
        .map(|t| t.image)
    else {
        return unavailable(env, "destination surface has no shared GPU image");
    };
    // Integer, equal-size rectangles map texel centers exactly. NONE, POINT
    // and LINEAR are equivalent here, as confirmed by the authored native
    // color oracle both inside and outside BeginScene. No blend/state is used.
    dispatch(
        env,
        owner,
        Operation::Copy {
            source,
            rectangle: from,
            destination,
            x: to.x,
            y: to.y,
            resources: [source_id, destination_id],
        },
    )
}

#[cfg(test)]
mod tests;
