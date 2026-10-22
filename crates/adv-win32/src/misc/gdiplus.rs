//! Guest GDI+ identities over the device's owned offscreen raster service.
use crate::Win32;
use adv_abi::{GuestPtr, Handle, Registry, WideStr, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_device::{
    gdiplus::{Create, Kind, Object, Op, Reply, Runtime},
    surface::{PixelFormat, Surface, SurfaceStore},
};
use adv_machine::{Env, Namespace, Outcome};
use adv_os::{Heaps, com::task_alloc, gdi::GdiObj};
use std::sync::Arc;
mod front;
mod properties;
const INVALID: i32 = 2;
const NOT_IMPLEMENTED: i32 = 6;
const MAX_BYTES: usize = adv_device::image::LIMIT;
#[derive(Clone, Copy)]
struct Scan {
    pointer: u32,
    stride: i32,
    format: i32,
}
struct Image {
    object: Arc<Object>,
    width: u32,
    height: u32,
    scan: Option<Scan>,
}
#[derive(Clone)]
struct Graphics {
    object: Arc<Object>,
    image: Arc<Image>,
    dc: Option<u32>,
}
enum Value {
    Token(Arc<Runtime>),
    Object(Arc<Object>),
    Image(Arc<Image>),
    Graphics(Graphics),
}
#[derive(Default)]
struct State {
    namespace: Option<Namespace>,
}
fn namespace(env: &mut Env<'_>) -> Option<Namespace> {
    let state = env.ext.get_or_default::<State>();
    if state.namespace.is_none() {
        state.namespace = env.sys.objects.new_namespace_in(0x0c000004, 0x0cfffffc);
    }
    state.namespace
}
fn value<'a>(env: &'a Env<'_>, handle: u32) -> Option<&'a Value> {
    let id = env
        .sys
        .objects
        .lookup_alias::<Value>(env.ext.try_get::<State>()?.namespace?, handle)?;
    env.sys.objects.owned(id)
}
fn insert(env: &mut Env<'_>, value: Value) -> Option<u32> {
    let ns = namespace(env)?;
    if env.sys.objects.alias_count(ns) >= 1024 {
        return None;
    }
    // Graphics can retain an image after its public alias is disposed. Count
    // distinct backing owners, rather than aliases, including those references.
    let mut images = std::collections::BTreeMap::new();
    for (_, value) in env.sys.objects.hosts_in::<Value>(ns) {
        image_extent(value, &mut images);
    }
    image_extent(&value, &mut images);
    if images.values().copied().sum::<u64>() > 4 * MAX_BYTES as u64 {
        return None;
    }
    env.sys.objects.insert_host_in(ns, value)
}
fn image_extent(value: &Value, images: &mut std::collections::BTreeMap<usize, u64>) {
    let image = match value {
        Value::Image(i) => i,
        Value::Graphics(g) => &g.image,
        _ => return,
    };
    images.insert(
        Arc::as_ptr(image) as usize,
        u64::from(image.width) * u64::from(image.height) * 4,
    );
}
fn runtime(env: &Env<'_>) -> Result<Arc<Runtime>, i32> {
    let ns = env
        .ext
        .try_get::<State>()
        .and_then(|s| s.namespace)
        .ok_or(18)?;
    env.sys
        .objects
        .hosts_in::<Value>(ns)
        .find_map(|(_, v)| match v {
            Value::Token(r) => Some(Arc::clone(r)),
            _ => None,
        })
        .ok_or(18)
}
fn object(env: &Env<'_>, handle: u32) -> Result<Arc<Object>, i32> {
    match value(env, handle) {
        Some(Value::Object(o)) => Ok(Arc::clone(o)),
        Some(Value::Image(i)) => Ok(Arc::clone(&i.object)),
        Some(Value::Graphics(g)) => Ok(Arc::clone(&g.object)),
        _ => Err(INVALID),
    }
}
fn image(env: &Env<'_>, handle: u32) -> Result<Arc<Image>, i32> {
    match value(env, handle) {
        Some(Value::Image(i)) => Ok(Arc::clone(i)),
        _ => Err(INVALID),
    }
}
fn format(env: &Env<'_>, handle: u32) -> Result<Option<Arc<Object>>, i32> {
    if handle == 0 {
        Ok(None)
    } else {
        object(env, handle).map(Some)
    }
}
fn clear(env: &mut Env<'_>, out: u32) -> bool {
    out != 0 && env.cpu.write_u32(out, 0).is_ok()
}
fn publish(env: &mut Env<'_>, out: u32, value: Result<Value, i32>) -> Outcome {
    let value = match value {
        Ok(v) => v,
        Err(6) => return missing_device_operation(env),
        Err(status) => return Outcome::Return(status as u32),
    };
    let Some(handle) = insert(env, value) else {
        return Outcome::Return(3);
    };
    if env.cpu.write_u32(out, handle).is_err() {
        remove(env, handle);
        return Outcome::Return(2);
    }
    Outcome::Return(0)
}
fn remove(env: &mut Env<'_>, handle: u32) -> bool {
    let Some(ns) = env.ext.try_get::<State>().and_then(|s| s.namespace) else {
        return false;
    };
    env.sys.objects.close_alias(ns, handle).is_some()
}
fn delete(env: &mut Env<'_>, handle: u32, kind: Kind) -> i32 {
    let valid = object(env, handle)
        .is_ok_and(|o| o.kind == kind || kind == Kind::Brush && o.kind == Kind::Gradient);
    if valid && remove(env, handle) {
        0
    } else {
        INVALID
    }
}
fn fail(env: &mut Env<'_>, api: &str, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    Outcome::Return(NOT_IMPLEMENTED as u32)
}
fn missing_device_operation(env: &mut Env<'_>) -> Outcome {
    let api = format!("gdiplus.dll!{}", env.sys.thunks.get(env.thunk).name);
    fail(
        env,
        &api,
        "the GDI+ device backend does not implement this operation",
    )
}
fn result(result: Result<Reply, i32>, env: &mut Env<'_>) -> Outcome {
    match result {
        Err(6) => missing_device_operation(env),
        other => Outcome::Return(other.map_or_else(|v| v as u32, |_| 0)),
    }
}
fn units(env: &Env<'_>, pointer: u32, length: i32) -> Result<Vec<u16>, i32> {
    if pointer == 0 || !(-1..=16384).contains(&length) {
        return Err(INVALID);
    }
    if length == -1 {
        return WideStr(GuestPtr::new(pointer))
            .read(env.cpu, 16384)
            .map_err(|_| INVALID);
    }
    let mut bytes = vec![0; length as usize * 2];
    if length != 0 && env.cpu.read(pointer, &mut bytes).is_err() {
        return Err(INVALID);
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect())
}
fn rectangle(env: &Env<'_>, pointer: u32) -> Result<[f32; 4], i32> {
    let mut bytes = [0; 16];
    env.cpu.read(pointer, &mut bytes).map_err(|_| INVALID)?;
    let mut r = [0.; 4];
    for (i, b) in bytes.as_chunks::<4>().0.iter().enumerate() {
        r[i] = f32::from_le_bytes(*b);
    }
    if r.iter().any(|x| !x.is_finite()) {
        return Err(INVALID);
    }
    Ok(r)
}
fn put_rect(env: &mut Env<'_>, pointer: u32, rect: [f32; 4]) -> bool {
    pointer != 0
        && env
            .cpu
            .write(
                pointer,
                &rect
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .is_ok()
}
fn image_pixels(env: &Env<'_>, width: u32, height: u32, scan: Scan) -> Result<Vec<u8>, i32> {
    let bpp = match scan.format {
        0x21808 => 3,
        0x22009 | 0x26200a | 0xe200b => 4,
        _ => return Err(NOT_IMPLEMENTED),
    };
    let len = u64::from(width) * u64::from(height) * 4;
    if width == 0
        || height == 0
        || len > MAX_BYTES as u64
        || scan.stride.unsigned_abs() < width.checked_mul(bpp).ok_or(INVALID)?
    {
        return Err(INVALID);
    }
    let mut pixels = vec![0; len as usize];
    let mut row = vec![0; width as usize * bpp as usize];
    for y in 0..height {
        let ptr = u32::try_from(i64::from(scan.pointer) + i64::from(scan.stride) * i64::from(y))
            .map_err(|_| INVALID)?;
        env.cpu.read(ptr, &mut row).map_err(|_| INVALID)?;
        for x in 0..width as usize {
            let src = &row[x * bpp as usize..];
            let dst = &mut pixels[(y as usize * width as usize + x) * 4..][..4];
            dst[..3].copy_from_slice(&src[..3]);
            dst[3] = if scan.format == 0x26200a || scan.format == 0xe200b {
                src[3]
            } else {
                255
            };
            if scan.format == 0xe200b {
                let a = u32::from(dst[3]);
                for v in &mut dst[..3] {
                    *v = (u32::from(*v) * 255).checked_div(a).unwrap_or(0).min(255) as u8;
                }
            }
        }
    }
    Ok(pixels)
}
fn refresh_image(env: &Env<'_>, image: &Image) -> Result<(), i32> {
    if let Some(scan) = image.scan {
        image.object.execute(Op::Upload(&image_pixels(
            env,
            image.width,
            image.height,
            scan,
        )?))?;
    }
    Ok(())
}
fn finish_image(env: &mut Env<'_>, image: &Image) -> Result<(), i32> {
    let Some(scan) = image.scan else {
        return Ok(());
    };
    let Reply::Pixels(pixels) = image.object.execute(Op::Pixels)? else {
        return Err(INVALID);
    };
    let bpp = if scan.format == 0x21808 { 3 } else { 4 };
    let mut row = vec![0; image.width as usize * bpp];
    for y in 0..image.height {
        for x in 0..image.width as usize {
            let mut p: [u8; 4] = pixels[(y as usize * image.width as usize + x) * 4..][..4]
                .try_into()
                .unwrap();
            if scan.format == 0xe200b {
                let a = u16::from(p[3]);
                for v in &mut p[..3] {
                    *v = (u16::from(*v) * a / 255) as u8;
                }
            }
            row[x * bpp..][..bpp].copy_from_slice(&p[..bpp]);
        }
        let ptr = u32::try_from(i64::from(scan.pointer) + i64::from(scan.stride) * i64::from(y))
            .map_err(|_| INVALID)?;
        env.cpu.write(ptr, &row).map_err(|_| INVALID)?;
    }
    Ok(())
}
fn surface(env: &Env<'_>, dc: u32) -> Result<(u32, Surface), i32> {
    let Some(GdiObj::Dc {
        selected, modes, ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc)
    else {
        return Err(INVALID);
    };
    if modes.map != 1 || modes.window_origin != [0, 0] || modes.viewport_origin != [0, 0] {
        return Err(NOT_IMPLEMENTED);
    }
    let target = selected[3];
    let s = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(target))
        .ok_or(NOT_IMPLEMENTED)?
        .clone();
    if s.pixels == 0 || s.format == PixelFormat::Indexed8 {
        return Err(NOT_IMPLEMENTED);
    }
    Ok((target, s))
}
fn surface_pixels(env: &Env<'_>, s: &Surface) -> Result<Vec<u8>, i32> {
    let len = u64::from(s.width) * u64::from(s.height) * 4;
    if len > MAX_BYTES as u64 {
        return Err(3);
    }
    let mut pixels = vec![0; len as usize];
    let bpp = s.format.bytes() as usize;
    let mut row = vec![0; s.width as usize * bpp];
    for y in 0..s.height {
        let row_y = if s.bottom_up { s.height - 1 - y } else { y };
        env.cpu
            .read(s.pixels + row_y * s.pitch, &mut row)
            .map_err(|_| INVALID)?;
        for x in 0..s.width as usize {
            let mut p = s.format.decode(&row[x * bpp..]);
            if s.format != PixelFormat::Bgra32 {
                p[3] = 0;
            }
            pixels[(y as usize * s.width as usize + x) * 4..][..4].copy_from_slice(&p);
        }
    }
    Ok(pixels)
}
fn graphics(env: &mut Env<'_>, handle: u32) -> Result<Graphics, i32> {
    let Some(Value::Graphics(g)) = value(env, handle) else {
        return Err(INVALID);
    };
    let g = g.clone();
    if let Some(dc) = g.dc {
        let (_, s) = surface(env, dc)?;
        if (s.width, s.height) != (g.image.width, g.image.height) {
            return Err(NOT_IMPLEMENTED);
        }
        g.image
            .object
            .execute(Op::Upload(&surface_pixels(env, &s)?))?;
    } else {
        refresh_image(env, &g.image)?;
    }
    Ok(g)
}
fn draw(env: &mut Env<'_>, handle: u32, operation: Op<'_>) -> Outcome {
    let g = match graphics(env, handle) {
        Ok(g) => g,
        Err(6) => {
            return fail(
                env,
                "gdiplus.dll!drawing",
                "HDC has no drawable shared bitmap or supported coordinate mapping",
            );
        }
        Err(v) => return Outcome::Return(v as u32),
    };
    if let Err(v) = g.object.execute(operation) {
        return if v == 6 {
            missing_device_operation(env)
        } else {
            Outcome::Return(v as u32)
        };
    }
    if let Some(dc) = g.dc {
        let (target, s) = match surface(env, dc) {
            Ok(v) => v,
            Err(v) => return Outcome::Return(v as u32),
        };
        let Reply::Pixels(pixels) = (match g.image.object.execute(Op::Pixels) {
            Ok(v) => v,
            Err(v) => return Outcome::Return(v as u32),
        }) else {
            return Outcome::Return(2);
        };
        let clip = crate::gdi32::raster::clip::snapshot(env, dc);
        let bpp = s.format.bytes() as usize;
        let mut changed = false;
        let mut damage = adv_device::surface::Rect::default();
        for y in 0..s.height {
            let row_y = if s.bottom_up { s.height - 1 - y } else { y };
            for (left, right) in clip.spans(y as i32, s.width) {
                let mut bytes = Vec::with_capacity((right - left) as usize * bpp);
                for x in left..right {
                    let p = pixels[(y as usize * s.width as usize + x as usize) * 4..][..4]
                        .try_into()
                        .unwrap();
                    bytes.extend_from_slice(&s.format.encode(p)[..bpp]);
                }
                let address = s.pixels + row_y * s.pitch + left * bpp as u32;
                let mut before = vec![0; bytes.len()];
                if env.cpu.read(address, &mut before).is_err() {
                    return Outcome::Return(2);
                }
                if before != bytes {
                    if env.cpu.write(address, &bytes).is_err() {
                        return Outcome::Return(2);
                    }
                    changed = true;
                    damage = damage.union(adv_device::surface::Rect {
                        x: left,
                        y,
                        w: right - left,
                        h: 1,
                    });
                }
            }
        }
        if changed {
            let window = env
                .ext
                .get::<Win32>()
                .gdi
                .is_window_dc(&env.sys.objects, dc);
            let Some(provider) = crate::gdi32::raster::front::provider(env.ext) else {
                return Outcome::Return(6);
            };
            let store = env.ext.get_mut::<SurfaceStore>();
            store.get_mut(target).unwrap().version += 1;
            if window {
                if let Err(error) =
                    store.present_from(env.context, target, &(provider.memory)(env.cpu), damage)
                {
                    return Outcome::Fail(error);
                }
                let presented = store.stats.presentations;
                if let Some(outcome) = (provider.completed)(env, presented) {
                    return outcome;
                }
            }
        }
        Outcome::Return(0)
    } else {
        Outcome::Return(finish_image(env, &g.image).map_or_else(|v| v as u32, |_| 0))
    }
}
fn read_stream(env: &mut Env<'_>, stream: u32, out: u32, icm: bool) -> Outcome {
    if !clear(env, out) {
        return Outcome::Return(2);
    }
    if icm {
        return fail(
            env,
            "gdiplus.dll!GdipCreateBitmapFromStreamICM",
            "color-managed image decoding is unavailable",
        );
    }
    if runtime(env).is_err() {
        return Outcome::Return(18);
    }
    let table = match env.cpu.read_u32(stream) {
        Ok(v) => v,
        Err(_) => return Outcome::Return(2),
    };
    let Some(seek) = env
        .cpu
        .read_u32(table + 20)
        .ok()
        .filter(|&p| env.cpu.accessible(p, 1, Prot::EXEC))
    else {
        return Outcome::Return(2);
    };
    let scratch = match env
        .ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, 4100, true)
    {
        Some(v) => v,
        None => return Outcome::Return(3),
    };
    Outcome::call(seek, vec![stream, 0, 0, 0, 0], move |env, result| {
        if result.value() & 0x80000000 != 0 {
            return stream_done(env, scratch, out, Err(2));
        }
        stream_chunk(env, stream, table, scratch, out, Vec::new())
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture};
    use adv_cpu::backend::Guest;

    fn startup(f: &mut Fixture) -> u32 {
        f.m.cpu.write_u32(DATA + 64, 1).unwrap();
        assert_eq!(
            f.call("gdiplus.dll", "GdiplusStartup", &[DATA, DATA + 64, 0]),
            0
        );
        f.m.cpu.read_u32(DATA).unwrap()
    }

    #[test]
    // spec: DEV-IMAGE-2
    fn image_graphics_retain_backing_and_clones_have_independent_pixels() {
        let mut f = Fixture::new("gdiplus-images");
        let token = startup(&mut f);
        f.m.cpu.write(DATA + 128, &[0, 0, 255, 255]).unwrap();
        assert_eq!(
            f.call(
                "gdiplus.dll",
                "GdipCreateBitmapFromScan0",
                &[1, 1, 4, 0x26200a, DATA + 128, DATA + 4]
            ),
            0
        );
        let image = f.m.cpu.read_u32(DATA + 4).unwrap();
        assert_eq!(
            f.call("gdiplus.dll", "GdipCloneImage", &[image, DATA + 8]),
            0
        );
        let clone = f.m.cpu.read_u32(DATA + 8).unwrap();
        assert_eq!(
            f.call(
                "gdiplus.dll",
                "GdipGetImageGraphicsContext",
                &[image, DATA + 12]
            ),
            0
        );
        let graphics = f.m.cpu.read_u32(DATA + 12).unwrap();
        assert_eq!(f.call("gdiplus.dll", "GdipDisposeImage", &[image]), 0);
        assert_eq!(
            f.call("gdiplus.dll", "GdipGraphicsClear", &[graphics, 0xff00ff00]),
            0
        );
        let mut pixels = [0; 4];
        f.m.cpu.read(DATA + 128, &mut pixels).unwrap();
        assert_eq!(pixels, [0, 255, 0, 255]);
        let cloned = super::image(&crate::misc::test_support::env(&mut f.m), clone).unwrap();
        let Reply::Pixels(pixels) = cloned.object.execute(Op::Pixels).unwrap() else {
            panic!()
        };
        assert_eq!(pixels, [0, 0, 255, 255]);
        assert_eq!(f.call("gdiplus.dll", "GdipDeleteGraphics", &[graphics]), 0);
        assert_eq!(f.call("gdiplus.dll", "GdipDeleteGraphics", &[graphics]), 2);
        assert_eq!(f.call("gdiplus.dll", "GdipDisposeImage", &[clone]), 0);
        assert_eq!(f.call("gdiplus.dll", "GdiplusShutdown", &[token]), 0);
        f.cleanup();
    }

    #[test]
    // spec: DEV-IMAGE-2
    fn hdc_drawing_preserves_gdi_alpha_and_writes_the_selected_guest_bitmap() {
        let mut f = Fixture::new("gdiplus-hdc");
        let token = startup(&mut f);
        let dc = f.call("gdi32.dll", "CreateCompatibleDC", &[0]);
        assert_ne!(dc, 0);
        for (offset, word) in [(0, 40), (4, 2), (8, (-2i32) as u32), (12, 0x200001)] {
            f.m.cpu.write_u32(DATA + 256 + offset, word).unwrap();
        }
        let bitmap = f.call(
            "gdi32.dll",
            "CreateDIBSection",
            &[dc, DATA + 256, 0, DATA + 16, 0, 0],
        );
        assert_ne!(bitmap, 0);
        let bits = f.m.cpu.read_u32(DATA + 16).unwrap();
        let previous = f.call("gdi32.dll", "SelectObject", &[dc, bitmap]);
        assert_ne!(previous, 0);
        assert_eq!(
            f.call("gdiplus.dll", "GdipCreateFromHDC", &[dc, DATA + 4]),
            0
        );
        let graphics = f.m.cpu.read_u32(DATA + 4).unwrap();
        f.m.cpu.write(DATA + 128, &[0, 0, 255, 128]).unwrap();
        assert_eq!(
            f.call(
                "gdiplus.dll",
                "GdipCreateBitmapFromScan0",
                &[1, 1, 4, 0x26200a, DATA + 128, DATA + 8]
            ),
            0
        );
        let image = f.m.cpu.read_u32(DATA + 8).unwrap();
        assert_eq!(
            f.call(
                "gdiplus.dll",
                "GdipDrawImageRectI",
                &[graphics, image, 0, 0, 1, 1]
            ),
            0
        );
        let mut pixel = [0; 4];
        f.m.cpu.read(bits, &mut pixel).unwrap();
        assert_eq!(pixel[0..2], [0, 0]);
        assert!((127..=128).contains(&pixel[2]), "{pixel:?}");
        assert_eq!(pixel[3], 128);
        assert_eq!(f.call("gdiplus.dll", "GdipDeleteGraphics", &[graphics]), 0);
        assert_eq!(f.call("gdiplus.dll", "GdipDisposeImage", &[image]), 0);
        f.call("gdi32.dll", "SelectObject", &[dc, previous]);
        assert_eq!(f.call("gdi32.dll", "DeleteObject", &[bitmap]), 1);
        assert_eq!(f.call("gdi32.dll", "DeleteDC", &[dc]), 1);
        f.call("gdiplus.dll", "GdiplusShutdown", &[token]);
        f.cleanup();
    }
}
#[cfg_attr(
    windows,
    allow(
        clippy::arc_with_non_send_sync,
        reason = "GDI handles are shared only within the owning execution thread"
    )
)]
fn stream_done(env: &mut Env<'_>, scratch: u32, out: u32, bytes: Result<Vec<u8>, i32>) -> Outcome {
    env.ext
        .get_mut::<Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, scratch);
    let value = (|| {
        let bytes = bytes?;
        let info = adv_device::image::inspect(&bytes).map_err(|e| match e {
            adv_device::image::Error::Invalid => 2,
            adv_device::image::Error::Unsupported(_) => 6,
        })?;
        let pixels = adv_device::image::decode(&bytes, info.file_format).map_err(|_| 2)?;
        let object = runtime(env)?.create(Create::Image(info.width, info.height, &pixels))?;
        Ok(Value::Image(Arc::new(Image {
            object,
            width: info.width,
            height: info.height,
            scan: None,
        })))
    })();
    publish(env, out, value)
}
fn stream_chunk(
    env: &mut Env<'_>,
    stream: u32,
    table: u32,
    scratch: u32,
    out: u32,
    mut bytes: Vec<u8>,
) -> Outcome {
    let Some(read) = env
        .cpu
        .read_u32(table + 12)
        .ok()
        .filter(|&p| env.cpu.accessible(p, 1, Prot::EXEC))
    else {
        return stream_done(env, scratch, out, Err(2));
    };
    let _ = env.cpu.write_u32(scratch + 4096, 0);
    Outcome::call(
        read,
        vec![stream, scratch, 4096, scratch + 4096],
        move |env, result| {
            let n = env.cpu.read_u32(scratch + 4096).unwrap_or(u32::MAX) as usize;
            if result.value() & 0x80000000 != 0 || n > 4096 || bytes.len() + n > MAX_BYTES {
                return stream_done(env, scratch, out, Err(2));
            }
            if n == 0 {
                return stream_done(env, scratch, out, Ok(bytes));
            }
            let start = bytes.len();
            bytes.resize(start + n, 0);
            if env.cpu.read(scratch, &mut bytes[start..]).is_err() {
                return stream_done(env, scratch, out, Err(2));
            }
            stream_chunk(env, stream, table, scratch, out, bytes)
        },
    )
}
pub(super) fn register(reg: &mut Registry) {
    front::register(reg);
}
