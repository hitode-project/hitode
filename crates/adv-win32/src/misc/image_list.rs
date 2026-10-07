//! Bounded color image lists own pixel snapshots in the ordinary Objects table.
//! Headless icon identities without decoded pixels are never appended as images.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Namespace, Objects};

use crate::{Win32, util::set_last_error};

const COLOR32: u32 = 0x20;
const MASK: u32 = 1;
const MAX_DIMENSION: i32 = 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_LISTS: usize = 256;
const MAX_PIXEL_BYTES: usize = 64 * 1024 * 1024;

struct Image {
    rgba: Vec<u8>,
    mask: Option<Vec<u8>>,
}

impl Image {
    fn bytes(&self) -> usize {
        self.rgba.len() + self.mask.as_ref().map_or(0, Vec::len)
    }
}

struct ImageList {
    width: u32,
    height: u32,
    mask: bool,
    grow: usize,
    images: Vec<Image>,
}

#[derive(Default)]
struct ImageLists {
    // The namespace is an alias view only. Payloads have a single Objects owner.
    namespace: Option<Namespace>,
}

impl ImageLists {
    fn get<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a ImageList> {
        let id = objects.lookup_alias::<ImageList>(self.namespace?, handle)?;
        objects.owned(id)
    }

    fn get_mut<'a>(&self, objects: &'a mut Objects, handle: u32) -> Option<&'a mut ImageList> {
        let id = objects.lookup_alias::<ImageList>(self.namespace?, handle)?;
        objects.owned_mut(id)
    }

    fn bytes(&self, objects: &Objects) -> usize {
        self.namespace.map_or(0, |namespace| {
            objects
                .hosts_in::<ImageList>(namespace)
                .flat_map(|(_, list)| &list.images)
                .map(Image::bytes)
                .sum()
        })
    }

    fn insert(&mut self, objects: &mut Objects, list: ImageList) -> Option<u32> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0a00_0004, 0x0aff_fffc);
        }
        let namespace = self.namespace?;
        if objects.alias_count(namespace) >= MAX_LISTS {
            return None;
        }
        objects.insert_host_in(namespace, list)
    }
}

fn fail(env: &mut Env<'_>, error: u32, result: i32) -> i32 {
    set_last_error(env, error);
    result
}

fn unsupported(env: &mut Env<'_>, api: &str, reason: &str, error: u32, result: i32) -> i32 {
    adv_abi::warn_unavailable(env, api, reason);
    fail(env, error, result)
}

#[win32(comctl32, name = "ImageList_Create", level = state)]
fn create(env: &mut Env<'_>, cx: i32, cy: i32, flags: u32, initial: i32, grow: i32) -> i32 {
    if cx <= 0 || cy <= 0 || initial < 0 || grow < 0 {
        return fail(env, 87, 0);
    }
    if flags & !MASK != COLOR32 {
        return unsupported(
            env,
            "comctl32.dll!ImageList_Create",
            "only COLOR32 lists with optional MASK have a decoded pixel representation",
            50,
            0,
        );
    }
    if cx > MAX_DIMENSION
        || cy > MAX_DIMENSION
        || initial as usize > MAX_ENTRIES
        || grow as usize > MAX_ENTRIES
    {
        return unsupported(
            env,
            "comctl32.dll!ImageList_Create",
            "image lists are bounded to 1024-pixel dimensions and 4096 entries/growth",
            8,
            0,
        );
    }
    let mut images = Vec::new();
    // cInitial is storage capacity, not the current image count (native SDK oracle).
    if images.try_reserve_exact(initial as usize).is_err() {
        return fail(env, 8, 0);
    }
    let list = ImageList {
        width: cx as u32,
        height: cy as u32,
        mask: flags & MASK != 0,
        grow: (grow as usize).max(1),
        images,
    };
    let lists = env.ext.get_or_default::<ImageLists>();
    if let Some(handle) = lists.insert(&mut env.sys.objects, list) {
        handle as i32
    } else {
        unsupported(
            env,
            "comctl32.dll!ImageList_Create",
            "image list owner/alias capacity is exhausted (at most 256 live lists)",
            8,
            0,
        )
    }
}

#[win32(comctl32, name = "ImageList_Destroy", level = state)]
fn destroy(env: &mut Env<'_>, handle: i32) -> i32 {
    let Some(lists) = env.ext.try_get::<ImageLists>() else {
        return 0;
    };
    if lists.get(&env.sys.objects, handle as u32).is_none() {
        return 0;
    }
    i32::from(
        env.sys
            .objects
            .close_alias(lists.namespace.unwrap(), handle as u32)
            .is_some(),
    )
}

#[win32(comctl32, name = "ImageList_GetIconSize", level = state)]
fn get_size(env: &mut Env<'_>, handle: i32, cx: GuestPtr<i32>, cy: GuestPtr<i32>) -> i32 {
    let Some(list) = env
        .ext
        .try_get::<ImageLists>()
        .and_then(|lists| lists.get(&env.sys.objects, handle as u32))
    else {
        return 0;
    };
    // Native requires both outputs. Preflight both before publishing either.
    if cx.is_null() || cy.is_null() {
        return 0;
    }
    if check_range(env.cpu, cx.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, cy.addr(), 4, Prot::WRITE).is_err()
    {
        return fail(env, 998, 0);
    }
    let (width, height) = (list.width, list.height);
    if env.cpu.write_u32(cx.addr(), width).is_err() || env.cpu.write_u32(cy.addr(), height).is_err()
    {
        return fail(env, 998, 0);
    }
    1
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len()).map_err(|_| ())?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

#[win32(comctl32, name = "ImageList_ReplaceIcon", level = implement)]
fn replace_icon(env: &mut Env<'_>, handle: i32, index: i32, icon: Handle) -> i32 {
    let Some(lists) = env.ext.try_get::<ImageLists>() else {
        return fail(env, 6, -1);
    };
    let Some(list) = lists.get(&env.sys.objects, handle as u32) else {
        return fail(env, 6, -1);
    };
    if index < -1 || (index >= 0 && index as usize >= list.images.len()) {
        // The native common-controls oracle reports ERROR_INVALID_HANDLE here.
        return fail(env, 6, -1);
    }
    if index == -1 && list.images.len() >= MAX_ENTRIES {
        return unsupported(
            env,
            "comctl32.dll!ImageList_ReplaceIcon",
            "image lists are bounded to 4096 entries",
            8,
            -1,
        );
    }
    let resources = &env.ext.get::<Win32>().user.objects;
    let Some(source) = resources.image(&env.sys.objects, icon.0) else {
        if resources.contains(&env.sys.objects, icon.0) {
            return unsupported(
                env,
                "comctl32.dll!ImageList_ReplaceIcon",
                "icon/cursor has only a headless resource identity; decoded pixels are unavailable",
                50,
                -1,
            );
        }
        return fail(env, 6, -1);
    };
    if (source.width, source.height) != (list.width, list.height) {
        return unsupported(
            env,
            "comctl32.dll!ImageList_ReplaceIcon",
            "icon-to-image-list resizing is unavailable; decoded source dimensions must match",
            50,
            -1,
        );
    }
    let bytes = source.pixels.len() + if list.mask { source.mask.len() } else { 0 };
    let old_bytes = if index == -1 {
        0
    } else {
        list.images[index as usize].bytes()
    };
    if lists.bytes(&env.sys.objects) - old_bytes + bytes > MAX_PIXEL_BYTES {
        return unsupported(
            env,
            "comctl32.dll!ImageList_ReplaceIcon",
            "image list pixel snapshots are bounded to 64MiB per guest process",
            8,
            -1,
        );
    }
    // Build both copies before mutating the list. No source handle is retained.
    let Ok(rgba) = copy_bytes(&source.pixels) else {
        return fail(env, 8, -1);
    };
    let mask = if list.mask {
        let Ok(mask) = copy_bytes(&source.mask) else {
            return fail(env, 8, -1);
        };
        Some(mask)
    } else {
        None
    };
    let image = Image { rgba, mask };
    let list = env
        .ext
        .get::<ImageLists>()
        .get_mut(&mut env.sys.objects, handle as u32)
        .unwrap();
    if index == -1 {
        if list.images.len() == list.images.capacity() {
            let reserve = list.grow.min(MAX_ENTRIES - list.images.len());
            if list.images.try_reserve_exact(reserve).is_err() {
                return fail(env, 8, -1);
            }
        }
        let index = list.images.len() as i32;
        list.images.push(image);
        index
    } else {
        list.images[index as usize] = image;
        index
    }
}

#[win32(comctl32, name = "ImageList_Read", level = safe_fail)]
fn read(env: &mut Env<'_>, _stream: GuestPtr<()>) -> i32 {
    unsupported(
        env,
        "comctl32.dll!ImageList_Read",
        "COM IStream image-list deserialization is unavailable",
        50,
        0,
    )
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_ABI);
    registry.register(destroy_ABI);
    registry.register(get_size_ABI);
    registry.register(replace_icon_ABI);
    registry.register(read_ABI);
}

#[cfg(test)]
mod tests;
