//! Assembly of the optional compatibility personality and multimedia fronts.
use adv_abi::Registry;

pub fn register(registry: &mut Registry) {
    adv_win32::compat::register(registry);
    adv_dx::dsound::register_unavailable(registry);
}
