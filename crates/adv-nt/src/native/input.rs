//! Queue keyboard state from the in-process Wine server.

use adv_machine::Env;

// spec: NAT-BODY-12
pub(super) fn key_state(env: &mut Env<'_>) -> Option<u64> {
    let tid = crate::server::current_tid(env).ok()?;
    let state = adv_wineserver::key_state(tid, env.arg(0))?;
    Some(u64::from((state & 0x81) as i8 as i32 as u32))
}
