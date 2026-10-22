//! Counts, dimensions, colors and copies mutate the existing Objects payloads.
use super::*;

fn list<'a>(env: &'a Env<'_>, handle: i32) -> Option<&'a ImageList> {
    env.ext
        .try_get::<ImageLists>()?
        .get(&env.sys.objects, handle as u32)
}
fn list_mut<'a>(env: &'a mut Env<'_>, handle: i32) -> Option<&'a mut ImageList> {
    env.ext
        .try_get::<ImageLists>()?
        .get_mut(&mut env.sys.objects, handle as u32)
}
#[win32(comctl32, name = "ImageList_GetImageCount", level = state)]
fn count(env: &mut Env<'_>, handle: i32) -> i32 {
    list(env, handle).map_or(0, |list| list.images.len() as i32)
}
#[win32(comctl32, name = "ImageList_GetBkColor", level = state)]
fn get_background(env: &mut Env<'_>, handle: i32) -> u32 {
    list(env, handle).map_or(u32::MAX, |list| list.background)
}
#[win32(comctl32, name = "ImageList_SetBkColor", level = state)]
fn set_background(env: &mut Env<'_>, handle: i32, color: u32) -> u32 {
    list_mut(env, handle).map_or(u32::MAX, |list| {
        std::mem::replace(&mut list.background, color)
    })
}
#[win32(comctl32, name = "ImageList_Remove", level = state)]
fn remove(env: &mut Env<'_>, handle: i32, index: i32) -> i32 {
    let Some(list) = list_mut(env, handle) else {
        return 0;
    };
    if index == -1 {
        list.images.clear();
    } else if index >= 0 && (index as usize) < list.images.len() {
        list.images.remove(index as usize);
    } else {
        return 0;
    }
    1
}
#[win32(comctl32, name = "ImageList_SetIconSize", level = state)]
fn set_size(env: &mut Env<'_>, handle: i32, width: i32, height: i32) -> i32 {
    if width <= 0 || height <= 0 {
        return 0;
    }
    if list(env, handle).is_none() {
        return 0;
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return unsupported(
            env,
            "comctl32.dll!ImageList_SetIconSize",
            "image dimensions exceed the bounded pixel owner",
            8,
            0,
        );
    }
    let list = list_mut(env, handle).unwrap();
    list.images.clear();
    list.width = width as u32;
    list.height = height as u32;
    1
}
pub(super) fn blank(width: u32, height: u32, mask: bool) -> Result<Image, ()> {
    let pixels = width as usize * height as usize;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(pixels * 4).map_err(|_| ())?;
    rgba.resize(pixels * 4, 0);
    let mask = if mask {
        let mut bits = Vec::new();
        bits.try_reserve_exact(pixels).map_err(|_| ())?;
        bits.resize(pixels, 1);
        Some(bits)
    } else {
        None
    };
    Ok(Image { rgba, mask })
}
#[win32(comctl32, name = "ImageList_SetImageCount", level = state)]
fn set_count(env: &mut Env<'_>, handle: i32, count: u32) -> i32 {
    let Some(list) = list(env, handle) else {
        return 0;
    };
    let old = list.images.len();
    if count as usize <= old {
        list_mut(env, handle)
            .unwrap()
            .images
            .truncate(count as usize);
        return 1;
    }
    let (width, height, mask) = (list.width, list.height, list.mask);
    let added = count as usize - old;
    let needed = u64::from(width) * u64::from(height) * added as u64 * if mask { 5 } else { 4 };
    let bytes = env.ext.get::<ImageLists>().bytes(&env.sys.objects);
    if count as usize > MAX_ENTRIES || needed + bytes as u64 > MAX_PIXEL_BYTES as u64 {
        return unsupported(
            env,
            "comctl32.dll!ImageList_SetImageCount",
            "image count exceeds the bounded pixel owner",
            8,
            0,
        );
    }
    let mut images = Vec::new();
    if images.try_reserve_exact(added).is_err() {
        return fail(env, 8, 0);
    }
    for _ in 0..added {
        let Ok(image) = blank(width, height, mask) else {
            return fail(env, 8, 0);
        };
        images.push(image);
    }
    let list = list_mut(env, handle).unwrap();
    if list.images.try_reserve_exact(added).is_err() {
        return fail(env, 8, 0);
    }
    list.images.extend(images);
    1
}
#[win32(comctl32, name = "ImageList_Duplicate", level = implement)]
fn duplicate(env: &mut Env<'_>, handle: i32) -> i32 {
    let Some(source) = list(env, handle) else {
        return 0;
    };
    let lists = env.ext.get::<ImageLists>();
    let needed: usize = source.images.iter().map(Image::bytes).sum();
    if lists.bytes(&env.sys.objects) + needed > MAX_PIXEL_BYTES {
        return unsupported(
            env,
            "comctl32.dll!ImageList_Duplicate",
            "duplicate exceeds the bounded pixel owner",
            8,
            0,
        );
    }
    let copy = (|| {
        let mut images = Vec::new();
        images
            .try_reserve_exact(source.images.len())
            .map_err(|_| ())?;
        for image in &source.images {
            images.push(Image {
                rgba: copy_bytes(&image.rgba)?,
                mask: image.mask.as_deref().map(copy_bytes).transpose()?,
            });
        }
        Ok::<_, ()>(ImageList {
            width: source.width,
            height: source.height,
            mask: source.mask,
            grow: source.grow,
            images,
            background: source.background,
        })
    })();
    let Ok(copy) = copy else {
        return fail(env, 8, 0);
    };
    match env
        .ext
        .get_mut::<ImageLists>()
        .insert(&mut env.sys.objects, copy)
    {
        Some(handle) => handle as i32,
        None => unsupported(
            env,
            "comctl32.dll!ImageList_Duplicate",
            "image-list object capacity is exhausted",
            8,
            0,
        ),
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        count_ABI,
        get_background_ABI,
        set_background_ABI,
        remove_ABI,
        set_size_ABI,
        set_count_ABI,
        duplicate_ABI,
    ] {
        registry.register(api);
    }
}
