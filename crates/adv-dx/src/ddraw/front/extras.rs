//! Secondary DirectDraw/Clipper/Surface queries and explicit capability boundaries.
use super::*;

fn unavailable_device(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    name: &'static str,
    reason: &'static str,
    out: Option<GuestPtr<u32>>,
) -> Outcome {
    if let Some(out) = out
        && out.addr() != 0
    {
        let _ = out.write(env.cpu, 0);
    }
    let state = env.ext.get::<State>();
    let Some(object) = state
        .draw
        .com
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
        .filter(|object| matches!(object.kind, Kind::Device))
    else {
        return Outcome::Return(INVALID);
    };
    if !object.initialized {
        return Outcome::Return(0x800401f0);
    }
    adv_abi::warn_unavailable(env, name, reason);
    Outcome::Return(UNSUPPORTED)
}

fn unavailable_surface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    name: &'static str,
    reason: &'static str,
    out: Option<GuestPtr<u32>>,
) -> Outcome {
    if let Some(out) = out
        && out.addr() != 0
    {
        let _ = out.write(env.cpu, 0);
    }
    if env
        .ext
        .get::<State>()
        .draw
        .surface_id(&env.sys.objects, this.addr())
        .is_none()
    {
        return Outcome::Return(INVALID);
    }
    adv_abi::warn_unavailable(env, name, reason);
    Outcome::Return(UNSUPPORTED)
}

#[win32(ddraw, name = "IDirectDraw7::GetSurfaceFromDC", level = implement)]
fn get_surface_from_dc(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    hdc: Handle,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.addr() == 0 {
        return Outcome::Return(INVALID);
    }
    if output.write(env.cpu, 0).is_err() {
        return Outcome::Return(INVALID);
    }
    let provider = env.ext.get::<State>().provider;
    let (state, store) = env.ext.get2_installed_mut::<State, SurfaceStore>();
    let Some(com) = state.draw.com else {
        return Outcome::Return(INVALID);
    };
    let Some(object) = com.get::<ComState>(&env.sys.objects, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if !matches!(object.kind, Kind::Device) {
        return Outcome::Return(INVALID);
    }
    if !object.initialized {
        return Outcome::Return(0x800401f0);
    }
    let legacy = object.legacy_descriptors.contains(&this.addr());
    let Some(&sid) = state.draw.surface_dcs.get(&hdc.0) else {
        return Outcome::Return(NOT_FOUND);
    };
    let mut mem = (provider.memory)(env.cpu);
    let pointer =
        match state
            .draw
            .surface_interface(&mut env.sys.objects, store, sid, legacy, &mut mem)
        {
            Ok(Some(pointer)) => pointer,
            Ok(None) => return Outcome::Return(0x8007000e),
            Err(error) => return error.outcome(env),
        };
    match output.write(env.cpu, pointer) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Return(INVALID),
    }
}

#[win32(ddraw, name = "IDirectDrawClipper::GetClipList", level = implement)]
fn clipper_get_clip_list(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    rect: GuestPtr<[i32; 4]>,
    clip_list: GuestPtr<u8>,
    size: GuestPtr<u32>,
) -> Outcome {
    if size.addr() == 0 {
        return Outcome::Return(INVALID);
    }
    let rect_filter = if rect.addr() == 0 {
        None
    } else {
        let Ok(r) = rect.read(env.cpu) else {
            return Outcome::Return(INVALID);
        };
        if r[2] <= r[0] || r[3] <= r[1] {
            return Outcome::Return(INVALID);
        }
        Some(r)
    };
    let state = env.ext.get::<State>();
    let Some(object) = state
        .draw
        .com
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
    else {
        return Outcome::Return(INVALID);
    };
    if !matches!(object.kind, Kind::Clipper) {
        return Outcome::Return(INVALID);
    }
    let mut rects: Vec<[i32; 4]> = if let Some(explicit) = state.draw.clip_lists.get(&this.addr()) {
        explicit
            .iter()
            .map(|r| {
                [
                    r.x as i32,
                    r.y as i32,
                    (r.x + r.w) as i32,
                    (r.y + r.h) as i32,
                ]
            })
            .collect()
    } else if let Some(&hwnd) = state
        .draw
        .clipper_windows
        .get(&this.addr())
        .filter(|&&h| h != 0)
    {
        if let Some(geom) = (state.provider.window)(env.ext, &env.sys.objects, hwnd)
            && geom.width > 0
            && geom.height > 0
        {
            vec![[
                geom.origin.0,
                geom.origin.1,
                geom.origin.0 + geom.width as i32,
                geom.origin.1 + geom.height as i32,
            ]]
        } else {
            Vec::new()
        }
    } else {
        return Outcome::Return(NO_CLIP_LIST);
    };
    if let Some([fl, ft, fr, fb]) = rect_filter {
        rects.retain_mut(|r| {
            r[0] = r[0].max(fl);
            r[1] = r[1].max(ft);
            r[2] = r[2].min(fr);
            r[3] = r[3].min(fb);
            r[2] > r[0] && r[3] > r[1]
        });
    }
    let rgn_size = (rects.len() as u32) * 16;
    let needed = 32 + rgn_size;
    if clip_list.addr() == 0 {
        return match size.write(env.cpu, needed) {
            Ok(()) => Outcome::Return(0),
            Err(_) => Outcome::Return(INVALID),
        };
    }
    let Ok(capacity) = size.read(env.cpu) else {
        return Outcome::Return(INVALID);
    };
    if size.write(env.cpu, needed).is_err() {
        return Outcome::Return(INVALID);
    }
    if capacity < needed {
        return Outcome::Return(REGION_TOO_SMALL);
    }
    let bound = rects
        .iter()
        .fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        });
    let bound = if rects.is_empty() { [0; 4] } else { bound };
    let mut buffer = Vec::with_capacity(needed as usize);
    for word in [
        32u32,
        1,
        rects.len() as u32,
        rgn_size,
        bound[0] as u32,
        bound[1] as u32,
        bound[2] as u32,
        bound[3] as u32,
    ] {
        buffer.extend_from_slice(&word.to_le_bytes());
    }
    for r in rects {
        for c in r {
            buffer.extend_from_slice(&c.to_le_bytes());
        }
    }
    if env.cpu.write(clip_list.addr(), &buffer).is_err() {
        return Outcome::Return(INVALID);
    }
    Outcome::Return(0)
}

#[win32(ddraw, name = "IDirectDrawClipper::IsClipListChanged", level = implement)]
fn clipper_is_clip_list_changed(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    changed: GuestPtr<i32>,
) -> Outcome {
    let is_clipper = env
        .ext
        .get::<State>()
        .draw
        .com
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
        .is_some_and(|object| matches!(object.kind, Kind::Clipper));
    if !is_clipper || changed.addr() == 0 {
        return Outcome::Return(INVALID);
    }
    match changed.write(env.cpu, 0) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Return(INVALID),
    }
}

#[win32(ddraw, name = "IDirectDraw7::CreatePalette", level = implement)]
fn create_palette(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    _entries: GuestPtr<()>,
    palette: GuestPtr<u32>,
    _outer: GuestPtr<()>,
) -> Outcome {
    unavailable_device(
        env,
        this,
        "IDirectDraw7::CreatePalette",
        "DirectDraw palette objects are unsupported",
        Some(palette),
    )
}

#[win32(ddraw, name = "IDirectDraw7::DuplicateSurface", level = implement)]
fn duplicate_surface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _source: GuestPtr<()>,
    duplicate: GuestPtr<u32>,
) -> Outcome {
    unavailable_device(
        env,
        this,
        "IDirectDraw7::DuplicateSurface",
        "DirectDraw surface duplication is unsupported",
        Some(duplicate),
    )
}

#[win32(ddraw, name = "IDirectDraw7::EnumSurfaces", level = implement)]
fn enum_surfaces(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    _desc: GuestPtr<()>,
    _context: GuestPtr<()>,
    _callback: GuestPtr<()>,
) -> Outcome {
    unavailable_device(
        env,
        this,
        "IDirectDraw7::EnumSurfaces",
        "DirectDraw surface enumeration is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDraw7::StartModeTest", level = implement)]
fn start_mode_test(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _modes: GuestPtr<()>,
    _count: u32,
    _flags: u32,
) -> Outcome {
    unavailable_device(
        env,
        this,
        "IDirectDraw7::StartModeTest",
        "DirectDraw refresh mode testing is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDraw7::EvaluateMode", level = implement)]
fn evaluate_mode(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    timeout: GuestPtr<u32>,
) -> Outcome {
    unavailable_device(
        env,
        this,
        "IDirectDraw7::EvaluateMode",
        "DirectDraw refresh mode testing is unsupported",
        Some(timeout),
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::AddAttachedSurface", level = implement)]
fn add_attached_surface(env: &mut Env<'_>, this: GuestPtr<()>, _attached: GuestPtr<()>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::AddAttachedSurface",
        "attaching arbitrary DirectDraw surfaces is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::AddOverlayDirtyRect", level = implement)]
fn add_overlay_dirty_rect(env: &mut Env<'_>, this: GuestPtr<()>, _rect: GuestPtr<()>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::AddOverlayDirtyRect",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::BltBatch", level = implement)]
fn blt_batch(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _batch: GuestPtr<()>,
    _count: u32,
    _flags: u32,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::BltBatch",
        "DirectDraw batch blitting is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::DeleteAttachedSurface", level = implement)]
fn delete_attached_surface(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    _attached: GuestPtr<()>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::DeleteAttachedSurface",
        "detaching DirectDraw surfaces is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::EnumAttachedSurfaces", level = implement)]
fn enum_attached_surfaces(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _context: GuestPtr<()>,
    _callback: GuestPtr<()>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::EnumAttachedSurfaces",
        "DirectDraw attached surface enumeration is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::EnumOverlayZOrders", level = implement)]
fn enum_overlay_z_orders(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    _context: GuestPtr<()>,
    _callback: GuestPtr<()>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::EnumOverlayZOrders",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::GetOverlayPosition", level = implement)]
fn get_overlay_position(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _x: GuestPtr<i32>,
    _y: GuestPtr<i32>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::GetOverlayPosition",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::GetPalette", level = implement)]
fn get_palette(env: &mut Env<'_>, this: GuestPtr<()>, palette: GuestPtr<u32>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::GetPalette",
        "DirectDraw palette objects are unsupported",
        Some(palette),
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetOverlayPosition", level = implement)]
fn set_overlay_position(env: &mut Env<'_>, this: GuestPtr<()>, _x: i32, _y: i32) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetOverlayPosition",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetPalette", level = implement)]
fn set_palette(env: &mut Env<'_>, this: GuestPtr<()>, _palette: GuestPtr<()>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetPalette",
        "DirectDraw palette objects are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::UpdateOverlay", level = implement)]
fn update_overlay(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _src_rect: GuestPtr<()>,
    _dst_surface: GuestPtr<()>,
    _dst_rect: GuestPtr<()>,
    _flags: u32,
    _fx: GuestPtr<()>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::UpdateOverlay",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::UpdateOverlayDisplay", level = implement)]
fn update_overlay_display(env: &mut Env<'_>, this: GuestPtr<()>, _flags: u32) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::UpdateOverlayDisplay",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::UpdateOverlayZOrder", level = implement)]
fn update_overlay_z_order(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _flags: u32,
    _reference: GuestPtr<()>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::UpdateOverlayZOrder",
        "DirectDraw hardware overlays are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetSurfaceDesc", level = implement)]
fn set_surface_desc(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _desc: GuestPtr<()>,
    _flags: u32,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetSurfaceDesc",
        "mutating surface backing memory via SetSurfaceDesc is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetPrivateData", level = implement)]
fn set_private_data(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _guid: GuestPtr<()>,
    _data: GuestPtr<()>,
    _size: u32,
    _flags: u32,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetPrivateData",
        "DirectDraw surface private data is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::GetPrivateData", level = implement)]
fn get_private_data(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _guid: GuestPtr<()>,
    _data: GuestPtr<()>,
    _size: GuestPtr<u32>,
) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::GetPrivateData",
        "DirectDraw surface private data is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::FreePrivateData", level = implement)]
fn free_private_data(env: &mut Env<'_>, this: GuestPtr<()>, _guid: GuestPtr<()>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::FreePrivateData",
        "DirectDraw surface private data is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetPriority", level = implement)]
fn set_priority(env: &mut Env<'_>, this: GuestPtr<()>, _priority: u32) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetPriority",
        "DirectDraw managed texture priorities are unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::GetPriority", level = implement)]
fn get_priority(env: &mut Env<'_>, this: GuestPtr<()>, priority: GuestPtr<u32>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::GetPriority",
        "DirectDraw managed texture priorities are unsupported",
        Some(priority),
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::SetLOD", level = implement)]
fn set_lod(env: &mut Env<'_>, this: GuestPtr<()>, _lod: u32) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::SetLOD",
        "DirectDraw managed texture LOD is unsupported",
        None,
    )
}

#[win32(ddraw, name = "IDirectDrawSurface7::GetLOD", level = implement)]
fn get_lod(env: &mut Env<'_>, this: GuestPtr<()>, lod: GuestPtr<u32>) -> Outcome {
    unavailable_surface(
        env,
        this,
        "IDirectDrawSurface7::GetLOD",
        "DirectDraw managed texture LOD is unsupported",
        Some(lod),
    )
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_surface_from_dc_ABI,
        clipper_get_clip_list_ABI,
        clipper_is_clip_list_changed_ABI,
        create_palette_ABI,
        duplicate_surface_ABI,
        enum_surfaces_ABI,
        start_mode_test_ABI,
        evaluate_mode_ABI,
        add_attached_surface_ABI,
        add_overlay_dirty_rect_ABI,
        blt_batch_ABI,
        delete_attached_surface_ABI,
        enum_attached_surfaces_ABI,
        enum_overlay_z_orders_ABI,
        get_overlay_position_ABI,
        get_palette_ABI,
        set_overlay_position_ABI,
        set_palette_ABI,
        update_overlay_ABI,
        update_overlay_display_ABI,
        update_overlay_z_order_ABI,
        set_surface_desc_ABI,
        set_private_data_ABI,
        get_private_data_ABI,
        free_private_data_ABI,
        set_priority_ABI,
        get_priority_ABI,
        set_lod_ABI,
        get_lod_ABI,
    ] {
        registry.register(api);
    }
}
