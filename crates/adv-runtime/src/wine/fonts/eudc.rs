//! EUDC linking loads the guest's configured private font into the same owner.
use super::*;

#[win32(gdi32, name = "EnableEUDC", level = state)]
pub(super) fn enable(env: &mut Env<'_>, enable: i32) -> i32 {
    if enable == 0 {
        if let Some(id) = env.ext.get_mut::<Fonts>().eudc.take() {
            let _ = env.ext.get_mut::<Fonts>().text.remove_resource(id);
        }
        return 1;
    }
    if env.ext.get::<Fonts>().eudc.is_some() {
        return 1;
    }
    let Ok(user) = adv_nt::environment::user_key(env) else {
        return 0;
    };
    let key = format!("{user}\\EUDC\\{}", env.ext.get::<Fonts>().codec.acp);
    let Ok(key) = adv_nt::registry::open_for_reading(env, 0, &key) else {
        return 1;
    };
    let value = adv_nt::registry::value(env, key, "SystemDefaultEUDCFont", 65536)
        .ok()
        .flatten();
    let _ = adv_nt::server::close(env, key);
    if let Some((_, bytes)) = value {
        let path = String::from_utf16_lossy(&placement::units(bytes))
            .trim_end_matches('\0')
            .to_owned();
        if let Some(bytes) = resource::read(env, &path)
            && let Ok((id, _)) = env.ext.get_mut::<Fonts>().text.add_resource(&bytes)
        {
            env.ext.get_mut::<Fonts>().eudc = Some(id);
        }
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(enable_ABI);
}
