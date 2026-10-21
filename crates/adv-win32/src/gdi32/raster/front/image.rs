//! MSIMG32 and GDI32 aliases share the same DCs, pixels and compositing service.
use super::*;
use adv_abi::types::BLENDFUNCTION;
use adv_device::surface::{Composite, PixelFormat};

pub(super) fn register(registry: &mut Registry) {
    for (api, alias) in [
        (alpha_ABI, "GdiAlphaBlend"),
        (transparent_ABI, "GdiTransparentBlt"),
        (gradient_ABI, "GdiGradientFill"),
    ] {
        registry.register(api);
        registry.register(adv_abi::Api {
            module: "gdi32.dll",
            name: alias,
            ..api
        });
    }
}

fn current_api(env: &Env<'_>, fallback: &'static str) -> &'static str {
    match env.sys.thunks.get(env.thunk).name.as_str() {
        "GdiAlphaBlend" => "gdi32.dll!GdiAlphaBlend",
        "GdiTransparentBlt" => "gdi32.dll!GdiTransparentBlt",
        "GdiGradientFill" => "gdi32.dll!GdiGradientFill",
        _ => fallback,
    }
}

#[allow(clippy::too_many_arguments)]
#[win32(msimg32, name = "AlphaBlend", level = implement)]
fn alpha(
    env: &mut Env<'_>,
    dst: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    src: Handle,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    blend: BLENDFUNCTION,
) -> Outcome {
    if blend.BlendOp != 0 || blend.BlendFlags != 0 || blend.AlphaFormat > 1 {
        return fail(env, 87);
    }
    let api = current_api(env, "msimg32.dll!AlphaBlend");
    transfer(
        env,
        dst,
        [x, y, width, height],
        src,
        [sx, sy, sw, sh],
        Composite::Alpha {
            constant: blend.SourceConstantAlpha,
            source_alpha: blend.AlphaFormat == 1,
        },
        api,
    )
}

#[allow(clippy::too_many_arguments)]
#[win32(msimg32, name = "TransparentBlt", level = implement)]
fn transparent(
    env: &mut Env<'_>,
    dst: Handle,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    src: Handle,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    color: u32,
) -> Outcome {
    let api = current_api(env, "msimg32.dll!TransparentBlt");
    transfer(
        env,
        dst,
        [x, y, width, height],
        src,
        [sx, sy, sw, sh],
        Composite::ColorKey([(color >> 16) as u8, (color >> 8) as u8, color as u8]),
        api,
    )
}

fn transfer(
    env: &mut Env<'_>,
    dst: Handle,
    destination: [i32; 4],
    src: Handle,
    source: [i32; 4],
    operation: Composite,
    api: &str,
) -> Outcome {
    if destination[2..]
        .iter()
        .chain(&source[2..])
        .any(|&size| size <= 0)
    {
        return fail(env, 87);
    }
    if unavailable_pixels(env, dst.0, api) || unavailable_pixels(env, src.0, api) {
        return Outcome::Return(0);
    }
    if !available(env) {
        return fail(env, 6);
    }
    raster_call_named(env, Some(api), |raster, surfaces, dcs, objects, memory| {
        let (Some(dm), Some(sm)) = (dcs.modes(objects, dst.0), dcs.modes(objects, src.0)) else {
            return Ok(raster.fail(6));
        };
        if dm.map != 1 || sm.map != 1 {
            return Ok(raster.fail(50));
        }
        let (Some([x, y]), Some([sx, sy])) = (
            dm.device_point(destination[0], destination[1]),
            sm.device_point(source[0], source[1]),
        ) else {
            return Ok(raster.fail(50));
        };
        let src_id = if dcs.is_window_dc(objects, src.0) {
            surfaces.flush(memory, FlushReason::Immediate)?;
            surfaces.primary(memory)?
        } else {
            dcs.get(objects, src.0).unwrap_or(0)
        };
        let dst_id = if dcs.is_window_dc(objects, dst.0) {
            surfaces.primary(memory)?
        } else {
            dcs.get(objects, dst.0).unwrap_or(0)
        };
        let (Some(s), Some(d)) = (surfaces.get(src_id), surfaces.get(dst_id)) else {
            return Ok(raster.fail(6));
        };
        let [sw, sh] = [source[2] as u32, source[3] as u32];
        if sx < 0
            || sy < 0
            || i64::from(sx) + i64::from(sw) > i64::from(s.width)
            || i64::from(sy) + i64::from(sh) > i64::from(s.height)
            || (matches!(
                operation,
                Composite::Alpha {
                    source_alpha: true,
                    ..
                }
            ) && s.format != PixelFormat::Bgra32)
        {
            return Ok(raster.fail(87));
        }
        if src_id == dst_id
            && i64::from(x) < i64::from(sx) + i64::from(sw)
            && i64::from(sx) < i64::from(x) + i64::from(destination[2])
            && i64::from(y) < i64::from(sy) + i64::from(sh)
            && i64::from(sy) < i64::from(y) + i64::from(destination[3])
        {
            return Ok(raster.fail(87));
        }
        if s.pixels == 0 || d.pixels == 0 {
            return Ok(raster.fail(50));
        }
        let clip = dcs.clip(objects, dst.0);
        let mut clipped = clip::ClippedMemory::new(memory, d.clone(), &clip);
        surfaces.composite(
            src_id,
            Rect {
                x: sx as u32,
                y: sy as u32,
                w: sw,
                h: sh,
            },
            dst_id,
            BlitRect {
                x: i64::from(x),
                y: i64::from(y),
                w: destination[2] as u32,
                h: destination[3] as u32,
            },
            operation,
            &mut clipped,
        )?;
        if dcs.is_window_dc(objects, dst.0) {
            surfaces.present_from(raster.context, dst_id, &clipped, clipped.damage())?;
        }
        Ok(1)
    })
}

#[win32(msimg32, name = "GradientFill", level = implement)]
fn gradient(
    env: &mut Env<'_>,
    dc: Handle,
    vertices: GuestPtr<adv_abi::types::TRIVERTEX>,
    vertex_count: u32,
    mesh: GuestPtr<()>,
    mesh_count: u32,
    mode: u32,
) -> Outcome {
    use adv_device::surface::GradientVertex;
    let api = current_api(env, "msimg32.dll!GradientFill");
    if mode > 2 || vertex_count == 0 || mesh_count == 0 {
        return fail(env, 87);
    }
    let indices = if mode == 2 { 3u32 } else { 2 };
    let Some(vertex_bytes) = vertex_count.checked_mul(16) else {
        return fail(env, 87);
    };
    let Some(mesh_bytes) = mesh_count.checked_mul(indices * 4) else {
        return fail(env, 87);
    };
    if adv_abi::guest::check_range(env.cpu, vertices.addr(), vertex_bytes, Prot::READ).is_err()
        || adv_abi::guest::check_range(env.cpu, mesh.addr(), mesh_bytes, Prot::READ).is_err()
    {
        return fail(env, 87);
    }
    if unavailable_pixels(env, dc.0, api) {
        return Outcome::Return(0);
    }
    if !available(env) {
        return fail(env, 6);
    }
    let mut points = Vec::new();
    let mut shapes = Vec::new();
    if points.try_reserve_exact(vertex_count as usize).is_err()
        || shapes.try_reserve_exact(mesh_count as usize).is_err()
    {
        return fail(env, 8);
    }
    for i in 0..vertex_count {
        let Ok(v) =
            GuestPtr::<adv_abi::types::TRIVERTEX>::new(vertices.addr() + i * 16).read(env.cpu)
        else {
            return fail(env, 87);
        };
        points.push(GradientVertex {
            x: v.x,
            y: v.y,
            color: [v.Blue, v.Green, v.Red],
        });
    }
    for i in 0..mesh_count {
        let mut shape = [0usize; 3];
        for j in 0..indices {
            let Ok(index) = env.cpu.read_u32(mesh.addr() + (i * indices + j) * 4) else {
                return fail(env, 87);
            };
            if index >= vertex_count {
                return fail(env, 87);
            }
            shape[j as usize] = index as usize;
        }
        shapes.push(shape);
    }
    let target = match env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0) {
        Some(adv_os::gdi::GdiObj::Dc { selected, .. }) => selected[3],
        _ => return fail(env, 6),
    };
    if env
        .ext
        .get::<SurfaceStore>()
        .get(target)
        .is_some_and(|s| !matches!(s.format, PixelFormat::Bgra32 | PixelFormat::Bgr24))
    {
        adv_abi::warn_unavailable(
            env,
            api,
            "gradient dithering for indexed and low-color surfaces is unavailable",
        );
        return fail(env, 50);
    }
    raster_call_named(env, Some(api), |raster, surfaces, dcs, objects, memory| {
        let Some(modes) = dcs.modes(objects, dc.0) else {
            return Ok(raster.fail(6));
        };
        if modes.map != 1 {
            return Ok(raster.fail(50));
        }
        for p in &mut points {
            let Some([x, y]) = modes.device_point(p.x, p.y) else {
                return Ok(raster.fail(50));
            };
            p.x = x;
            p.y = y;
        }
        let id = if dcs.is_window_dc(objects, dc.0) {
            surfaces.primary(memory)?
        } else {
            dcs.get(objects, dc.0).unwrap_or(0)
        };
        let Some(surface) = surfaces.get(id).cloned() else {
            return Ok(raster.fail(6));
        };
        if surface.pixels == 0 {
            return Ok(raster.fail(50));
        }
        let clip = dcs.clip(objects, dc.0);
        let mut clipped = clip::ClippedMemory::new(memory, surface, &clip);
        for [a, b, c] in shapes {
            if mode == 2 {
                surfaces.gradient_triangle(id, [points[a], points[b], points[c]], &mut clipped)?;
            } else {
                surfaces.gradient_rectangle(id, [points[a], points[b]], mode == 0, &mut clipped)?;
            }
        }
        if dcs.is_window_dc(objects, dc.0) {
            surfaces.present_from(raster.context, id, &clipped, clipped.damage())?;
        }
        Ok(1)
    })
}
