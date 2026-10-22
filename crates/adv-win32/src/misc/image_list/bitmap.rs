//! Bitmap strips are copied into the existing image-list payloads before mutation.
use super::*;
use crate::gdi32::raster::bitmap::snapshot;
fn list<'a>(env: &'a Env<'_>, handle: i32) -> Option<&'a ImageList> {
    env.ext
        .try_get::<ImageLists>()?
        .get(&env.sys.objects, handle as u32)
}
fn prepare(
    env: &mut Env<'_>,
    handle: i32,
    bitmap: u32,
    mask: u32,
    key: Option<u32>,
    replace: bool,
) -> Result<Vec<Image>, u32> {
    let target = list(env, handle).ok_or(6u32)?;
    let (width, height, masked) = (target.width, target.height, target.mask);
    let source = snapshot(env, bitmap)?;
    if key.is_some() && source.depth > 8 {
        return Err(87);
    }
    let count = if replace { 1 } else { source.width / width };
    if count == 0 {
        return Err(87);
    }
    if count as usize > MAX_ENTRIES {
        return Err(50);
    }
    let alpha_mask = if masked && mask != 0 && key.is_none() {
        Some(snapshot(env, mask)?)
    } else {
        None
    };
    if alpha_mask
        .as_ref()
        .is_some_and(|m| m.depth != 1 || m.width < source.width || m.height < source.height)
    {
        return Err(87);
    }
    let key = key.map(|color| {
        if color == 0xff000000 {
            u32::from(source.rgba[0])
                | u32::from(source.rgba[1]) << 8
                | u32::from(source.rgba[2]) << 16
        } else {
            color & 0xffffff
        }
    });
    let total =
        u64::from(width) * u64::from(height) * u64::from(count) * if masked { 5 } else { 4 };
    if total > MAX_PIXEL_BYTES as u64 {
        return Err(50);
    }
    let mut images = Vec::new();
    images.try_reserve_exact(count as usize).map_err(|_| 8u32)?;
    for index in 0..count {
        let mut image = state::blank(width, height, masked).map_err(|_| 8u32)?;
        for y in 0..height.min(source.height) {
            for x in 0..width {
                let sx = index * width + x;
                if sx >= source.width {
                    continue;
                }
                let p = (u64::from(y) * u64::from(source.width) + u64::from(sx)) as usize;
                let destination = (y * width + x) as usize;
                let pixel: &[u8] = &source.rgba[p * 4..p * 4 + 4];
                let transparent = masked
                    && (key.is_some_and(|key| {
                        key == (u32::from(pixel[0])
                            | u32::from(pixel[1]) << 8
                            | u32::from(pixel[2]) << 16)
                    }) || alpha_mask.as_ref().is_some_and(|mask| {
                        let at = ((y * mask.width + sx) * 4) as usize;
                        mask.rgba[at] != 0
                    }));
                image.rgba[destination * 4..destination * 4 + 4].copy_from_slice(
                    if transparent && key.is_some() {
                        &[0, 0, 0, 0]
                    } else {
                        pixel
                    },
                );
                if let Some(mask) = &mut image.mask {
                    mask[destination] = u8::from(transparent);
                }
            }
        }
        images.push(image);
    }
    Ok(images)
}
fn add(env: &mut Env<'_>, handle: i32, bitmap: u32, mask: u32, key: Option<u32>) -> i32 {
    let images = match prepare(env, handle, bitmap, mask, key, false) {
        Ok(images) => images,
        Err(50) => {
            let api = format!("comctl32.dll!{}", env.sys.thunks.get(env.thunk).name);
            return unsupported(
                env,
                &api,
                "source bitmap palette or size exceeds the retained pixel representation",
                50,
                -1,
            );
        }
        Err(error) => return fail(env, error, -1),
    };
    let list = list(env, handle).unwrap();
    let index = list.images.len();
    let bytes: usize = images.iter().map(Image::bytes).sum();
    if index + images.len() > MAX_ENTRIES
        || env.ext.get::<ImageLists>().bytes(&env.sys.objects) + bytes > MAX_PIXEL_BYTES
    {
        let api = format!("comctl32.dll!{}", env.sys.thunks.get(env.thunk).name);
        return unsupported(
            env,
            &api,
            "image count or storage exceeds the bounded list capacity",
            8,
            -1,
        );
    }
    let list = env
        .ext
        .get::<ImageLists>()
        .get_mut(&mut env.sys.objects, handle as u32)
        .unwrap();
    if list.images.try_reserve(images.len()).is_err() {
        return fail(env, 8, -1);
    }
    list.images.extend(images);
    index as i32
}
#[win32(comctl32,name="ImageList_Add",level=implement)]
fn add_bitmap(env: &mut Env<'_>, handle: i32, bitmap: Handle, mask: Handle) -> i32 {
    add(env, handle, bitmap.0, mask.0, None)
}
#[win32(comctl32,name="ImageList_AddMasked",level=implement)]
fn add_masked(env: &mut Env<'_>, handle: i32, bitmap: Handle, color: u32) -> i32 {
    add(env, handle, bitmap.0, 0, Some(color))
}
#[win32(comctl32,name="ImageList_Replace",level=implement)]
fn replace(env: &mut Env<'_>, handle: i32, index: i32, bitmap: Handle, mask: Handle) -> i32 {
    if !list(env, handle).is_some_and(|l| index >= 0 && (index as usize) < l.images.len()) {
        return fail(env, 6, 0);
    }
    let mut images = match prepare(env, handle, bitmap.0, mask.0, None, true) {
        Ok(images) => images,
        Err(50) => {
            return unsupported(
                env,
                "comctl32.dll!ImageList_Replace",
                "source bitmap palette or size exceeds the retained pixel representation",
                50,
                0,
            );
        }
        Err(error) => return fail(env, error, 0),
    };
    let lists = env.ext.get::<ImageLists>();
    let old = list(env, handle).unwrap().images[index as usize].bytes();
    let new = images[0].bytes();
    if lists.bytes(&env.sys.objects) - old + new > MAX_PIXEL_BYTES {
        return unsupported(
            env,
            "comctl32.dll!ImageList_Replace",
            "replacement exceeds the bounded list storage",
            8,
            0,
        );
    }
    lists
        .get_mut(&mut env.sys.objects, handle as u32)
        .unwrap()
        .images[index as usize] = images.remove(0);
    1
}
pub(super) fn register(registry: &mut Registry) {
    for api in [add_bitmap_ABI, add_masked_ABI, replace_ABI] {
        registry.register(api);
    }
}
