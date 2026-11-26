use adv_device::surface::Rect;
use std::hash::Hash;

pub(crate) fn hash(value: impl Hash) -> u128 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    value.hash(&mut h);
    h.digest128()
}
pub(crate) fn rect(r: Rect) -> (u32, u32, u32, u32) {
    (r.x, r.y, r.w, r.h)
}
