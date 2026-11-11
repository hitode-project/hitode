//! Identify frames timed by the guest's one-shot WinMM event.
use std::collections::HashMap;

use adv_machine::{CtxId, Env, Outcome};
use adv_nt::win32k::MessageWaitEvents;

use super::{ORIGINAL, imported};

const TIME_PERIODIC: u32 = 1;
const TIME_CALLBACK_EVENT_SET: u32 = 0x10;
const TIME_CALLBACK_EVENT_PULSE: u32 = 0x20;

#[derive(Default)]
struct Armed(HashMap<CtxId, (u32, u32)>);

pub(super) fn call(env: &mut Env<'_>, name: &str, args: Vec<u32>) -> Outcome {
    let import = match imported(env, ORIGINAL, name) {
        Ok(import) => import,
        Err(error) => return Outcome::Fail(error),
    };
    let arm = match args.as_slice() {
        &[_, _, event, _, flags]
            if flags & (TIME_PERIODIC | TIME_CALLBACK_EVENT_SET | TIME_CALLBACK_EVENT_PULSE)
                == TIME_CALLBACK_EVENT_SET =>
        {
            Some(event)
        }
        _ => None,
    };
    let cancel = (name == "timeKillEvent").then_some(args[0]);
    import.call(env, args, move |env, result| {
        let result = result.value();
        if let Some(event) = arm.filter(|_| result != 0) {
            env.ext.get_or_default::<MessageWaitEvents>();
            env.ext
                .get_or_default::<Armed>()
                .0
                .insert(env.context, (event, result));
        }
        if let Some(id) = cancel.filter(|_| result == 0)
            && let Some(armed) = env.ext.try_get_mut::<Armed>()
        {
            armed.0.retain(|_, &mut (_, timer)| timer != id);
        }
        Outcome::Return(result)
    })
}

/// Consume the observations for one Present. The same event must have
/// participated in the preceding message wait and been armed for this frame.
pub(in crate::wine) fn take(env: &mut Env<'_>) -> bool {
    let wait = env
        .ext
        .try_get_mut::<MessageWaitEvents>()
        .and_then(|waits| waits.take(env.context));
    let armed = env
        .ext
        .try_get_mut::<Armed>()
        .and_then(|armed| armed.0.remove(&env.context));
    armed.is_some_and(|(event, _)| wait == Some(event))
}
