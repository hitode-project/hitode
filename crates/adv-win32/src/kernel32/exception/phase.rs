//! Observations of native exception work, owned by its callback continuations.
//! These phases do not pretend to implement the CRT's C++ processingThrow count.
use adv_machine::{CtxId, Env};
use std::{
    collections::BTreeMap,
    rc::{Rc, Weak},
};

pub(in crate::kernel32) enum Phase {
    Dispatch,
    Unwind,
}

#[derive(Default)]
struct Thread {
    phases: Vec<Weak<Phase>>,
    unknown_cpp: bool,
}

#[derive(Default)]
struct Observations(BTreeMap<CtxId, Thread>);

pub(in crate::kernel32) fn enter(env: &mut Env<'_>, phase: Phase, code: u32) -> Rc<Phase> {
    let thread = env
        .ext
        .get_or_default::<Observations>()
        .0
        .entry(env.context)
        .or_default();
    thread.phases.retain(|phase| phase.strong_count() != 0);
    // A Windows exception disposition is not evidence that the CRT entered a
    // matching C++ catch. Do not clear this until a real C++ frontend can do so.
    thread.unknown_cpp |= code == 0xe06d_7363;
    let phase = Rc::new(phase);
    thread.phases.push(Rc::downgrade(&phase));
    phase
}

pub(crate) fn uncaught_query_blocker(env: &Env<'_>) -> Option<&'static str> {
    let thread = env.ext.try_get::<Observations>()?.0.get(&env.context)?;
    if thread.unknown_cpp {
        return Some(
            "C++ exception state is unknown after exception 0xe06d7363; throw/catch tracking is unsupported",
        );
    }
    thread.phases.iter().rev().find_map(Weak::upgrade).map(|phase| {
        match *phase {
            Phase::Dispatch => {
                "query during Windows exception dispatch is unsupported without C++ throw/catch tracking"
            }
            Phase::Unwind => {
                "query during Windows stack unwind is unsupported without C++ throw/catch tracking"
            }
        }
    })
}
