//! Color copies and full-source LINEAR scaling share the GPU and COM owners.
//! Cropped scaling and depth/MSAA remain unavailable.
use super::*;
use adv_device::surface::Rect;

pub(super) fn register(registry: &mut Registry) {
    registry.register(stretch_ABI);
    registry.register(color_fill_ABI);
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
    let from = match rectangle(env, source_rect, src.desc.Width, src.desc.Height) {
        Ok(rect) => rect,
        Err(error) => return Outcome::Return(error),
    };
    let to = match rectangle(env, destination_rect, dst.desc.Width, dst.desc.Height) {
        Ok(rect) => rect,
        Err(error) => return Outcome::Return(error),
    };
    let source_description = src.desc;
    let destination_description = dst.desc;
    let unavailable = |env: &mut Env<'_>, reason: &str| {
        // Actual requested rectangles belong in the bounded warning evidence.
        // Equal-size hot calls avoid allocating this diagnostic string.
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::StretchRect",
            &format!(
                "{reason}: source={}x{} format={} rect=({},{},{},{}); destination={}x{} format={} rect=({},{},{},{}); filter={filter}",
                source_description.Width,
                source_description.Height,
                source_description.Format,
                from.x,
                from.y,
                from.x + from.w,
                from.y + from.h,
                destination_description.Width,
                destination_description.Height,
                destination_description.Format,
                to.x,
                to.y,
                to.x + to.w,
                to.y + to.h,
            ),
        );
        Outcome::Return(UNAVAILABLE)
    };
    if src.depth.is_some()
        || dst.depth.is_some()
        || src.desc.MultiSampleType != 0
        || dst.desc.MultiSampleType != 0
        || src.desc.MultiSampleQuality != 0
        || dst.desc.MultiSampleQuality != 0
    {
        return unavailable(env, "depth-stencil and multisample blits are unavailable");
    }
    // RGBX-to-ARGB conversion samples the existing opaque image interpretation;
    // it must not expose the physical unused X byte as stored alpha.
    if !matches!(
        (src.desc.Format, dst.desc.Format),
        (21, 21) | (22, 22) | (21, 22) | (22, 21)
    ) {
        return unavailable(
            env,
            "color blits support 21/22; other format conversions are unavailable",
        );
    }
    if dst.desc.Usage == 0 && src.desc.Usage != 0 {
        return unavailable(
            env,
            "render-target to offscreen-plain blits are unavailable",
        );
    }
    if (from.w, from.h) != (to.w, to.h) {
        if from.x != 0 || from.y != 0 || from.w != src.desc.Width || from.h != src.desc.Height {
            return unavailable(env, "cropped-source scaling is unavailable");
        }
        if dst.desc.Usage & 1 == 0 {
            return unavailable(env, "scaled blits require a render-target destination");
        }
        if filter != 2 {
            return unavailable(env, "scaling currently requires LINEAR filtering");
        }
    }
    let source_id = com(env)
        .identity::<Payload>(&env.sys.objects, source.addr())
        .unwrap();
    let destination_id = com(env)
        .identity::<Payload>(&env.sys.objects, destination.addr())
        .unwrap();
    if dst.container.is_some() && dst.desc.Usage & 1 == 0 {
        return unavailable(env, "texture destinations require render-target usage");
    }
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
    let Some(destination) = dst.surface.and_then(|id| (provider.target)(env.ext, id)) else {
        return unavailable(env, "destination surface has no shared GPU image");
    };
    // Equal-size copies stay exact. Scaled copies use the isolated neutral
    // scaling state; guest scene/blend/alpha/depth state is not involved.
    let resources = [
        resources::binding_owner(env, source_id),
        resources::binding_owner(env, destination_id),
    ];
    dispatch(
        env,
        owner,
        Operation::Copy {
            source,
            rectangle: from,
            destination,
            region: to,
            filter: if filter == 2 {
                adv_device::gpu::Filter::Linear
            } else {
                adv_device::gpu::Filter::Nearest
            },
            resources,
        },
    )
}

/// A fill uploads host pixels: readback and frame recording see an ordinary
/// write of the rectangle, as for an unlocked surface.
#[win32(d3d9, name = "IDirect3DDevice9::ColorFill", level = implement)]
fn color_fill(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    surface: GuestPtr<()>,
    rect: GuestPtr<RECT>,
    color: u32,
) -> Outcome {
    let Some(owner) = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .filter(|_| device(env, this.addr()).is_some())
    else {
        return Outcome::Return(INVALID);
    };
    let Some(r) = resources::resource(env, surface.addr()) else {
        return Outcome::Return(INVALID);
    };
    // Render targets, offscreen plain surfaces and render-target texture
    // levels in the default pool accept fills.
    if r.kind != resources::Kind::Surface
        || r.device != owner
        || r.desc.Pool != 0
        || r.depth.is_some()
        || (r.container.is_some() && r.desc.Usage & 1 == 0)
        || resources::locked(env, r)
    {
        return Outcome::Return(INVALID);
    }
    let to = match rectangle(env, rect, r.desc.Width, r.desc.Height) {
        Ok(rect) => rect,
        Err(error) => return Outcome::Return(error),
    };
    if !matches!(r.desc.Format, 21 | 22) || r.desc.MultiSampleType != 0 {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DDevice9::ColorFill",
            &format!(
                "fills support single-sample formats 21/22: format={} multisample={}",
                r.desc.Format, r.desc.MultiSampleType
            ),
        );
        return Outcome::Return(UNAVAILABLE);
    }
    let provider = env.ext.get::<State>().provider;
    let Some(target) = r.surface.and_then(|id| (provider.target)(env.ext, id)) else {
        return Outcome::Return(INVALID);
    };
    let (resource, level) = match r.container {
        Some(container) => (container, Some(r.level)),
        None => (
            com(env)
                .identity::<Payload>(&env.sys.objects, surface.addr())
                .unwrap(),
            None,
        ),
    };
    let mut texel = color.to_le_bytes();
    if r.desc.Format == 22 {
        texel[3] = 255;
    }
    let pixels = texel.repeat(to.w as usize * to.h as usize);
    dispatch(
        env,
        owner,
        Operation::Upload {
            resource,
            level,
            target,
            pixels: Pixels::Host(Arc::new(pixels)),
            pitch: to.w * 4,
            rectangle: to,
        },
    )
}

/// Format interpretation belongs to this frontend; neutral drawing already
/// samples opaque images with alpha one and never uses guest render state.
pub(super) fn copy(
    gpu: &mut Gpu,
    source: &adv_device::gpu::Image,
    from: Rect,
    destination: &Target,
    to: Rect,
    filter: adv_device::gpu::Filter,
) -> Result<(), adv_device::gpu::Error> {
    use adv_device::gpu::{AlphaMode, DrawState, Filter, TextureOp, Vertex};
    if source.alpha_mode() != AlphaMode::Opaque
        || destination.image.alpha_mode() != AlphaMode::Stored
    {
        return gpu.blit(source, from, &destination.image, to, filter);
    }
    let coordinates = |offset: u32, size: u32, total: u32, output: u32| {
        if size == output {
            (
                offset as f32 / total as f32,
                (offset + size) as f32 / total as f32,
            )
        } else if output <= size {
            (0., 1.)
        } else if size == 1 {
            (0.5, 0.5)
        } else {
            (
                0.,
                output as f32 * (size - 1) as f32 / (size as f32 * (output - 1) as f32),
            )
        }
    };
    let (u0, u1) = coordinates(from.x, from.w, source.width(), to.w);
    let (v0, v1) = coordinates(from.y, from.h, source.height(), to.h);
    let (w, h) = (destination.image.width(), destination.image.height());
    let (x0, x1) = (
        to.x as f32 * 2. / w as f32 - 1.,
        (to.x + to.w) as f32 * 2. / w as f32 - 1.,
    );
    let (y0, y1) = (
        1. - to.y as f32 * 2. / h as f32,
        1. - (to.y + to.h) as f32 * 2. / h as f32,
    );
    let vertex = |x, y, u, v| Vertex {
        position: [x, y, 0., 1.],
        color: [1.; 4],
        uv: [u, v],
    };
    let vertices = [
        vertex(x0, y0, u0, v0),
        vertex(x1, y0, u1, v0),
        vertex(x1, y1, u1, v1),
        vertex(x0, y0, u0, v0),
        vertex(x1, y1, u1, v1),
        vertex(x0, y1, u0, v1),
    ];
    let mut state = DrawState::new(w, h);
    state.texture_op = TextureOp::Texture;
    state.alpha_texture_op = TextureOp::Texture;
    state.sampler.min = if filter == Filter::Linear {
        Filter::Linear
    } else {
        Filter::Nearest
    };
    state.sampler.mag = state.sampler.min;
    state.scissor = Some(to);
    gpu.draw(
        &destination.clone().with_depth(None)?,
        &vertices,
        Some(source),
        state,
    )
}

#[cfg(test)]
pub(super) mod tests;
