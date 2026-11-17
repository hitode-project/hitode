//! The graph's reference clock: one held reference, handed to every guest
//! member. Until one is set explicitly, leaving Stopped picks the default as
//! quartz does: the newest member's own clock, else the system clock.
use super::*;
use crate::dshow::host::create_wine;

const IID_IREFERENCECLOCK: Iid = quartz(0x56a8_6897);
const CLSID_SYSTEMCLOCK: Iid = amovie(0xe436_ebb1);

/// IMediaFilter::SetSyncSource: an explicit clock ends the default choice.
pub(super) fn set(env: &mut Env<'_>, graph: u32, clock: u32) -> Outcome {
    g(env, graph).default_clock = false;
    assign(env, graph, clock, Box::new(|_, hr| Outcome::Return(hr[0])))
}

/// Hand `clock` to every guest member and keep it, queuing EC_CLOCK_CHANGED.
/// If one refuses, every member gets the old clock back and the graph keeps it.
pub(super) fn assign(env: &mut Env<'_>, graph: u32, clock: u32, then: Then) -> Outcome {
    let guests: Vec<u32> = g(env, graph)
        .members
        .iter()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .collect();
    let calls = guests.iter().map(|&f| Call(f, 8, vec![clock])).collect();
    chain(
        env,
        calls,
        Vec::new(),
        Box::new(move |env, results| {
            if let Some(&hr) = results.iter().find(|&&hr| hr & 0x8000_0000 != 0) {
                let old = g(env, graph).clock;
                let calls = guests.iter().map(|&f| Call(f, 8, vec![old])).collect();
                return chain(
                    env,
                    calls,
                    Vec::new(),
                    Box::new(move |env, _| then(env, vec![hr])),
                );
            }
            let mut calls = VecDeque::new();
            if clock != 0 {
                calls.push_back(Call(clock, 1, vec![]));
            }
            let old = std::mem::replace(&mut g(env, graph).clock, clock);
            if old != 0 {
                calls.push_back(Call(old, 2, vec![]));
            }
            // quartz reports every clock it sets to the application.
            super::notify(env, graph, super::EC_CLOCK_CHANGED, 0, 0);
            chain(
                env,
                calls,
                Vec::new(),
                Box::new(move |env, _| then(env, vec![S_OK])),
            )
        }),
    )
}

/// IFilterGraph::SetDefaultSyncSource: the first member added that exposes a
/// clock, as Windows chooses (not quartz's newest first).
pub(super) fn set_default(env: &mut Env<'_>, graph: u32, then: Then) -> Outcome {
    let oldest_first = g(env, graph)
        .members
        .iter()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .collect();
    member_clock(env, graph, oldest_first, then)
}

/// Before `next`, choose the default clock if the graph still has none.
pub(super) fn ensure_default(env: &mut Env<'_>, graph: u32, next: Then) -> Outcome {
    let entry = g(env, graph);
    if entry.default_clock && entry.clock == 0 {
        set_default(env, graph, next)
    } else {
        next(env, Vec::new())
    }
}

/// Removing the member that provided the default clock clears it.
pub(super) fn leave(env: &mut Env<'_>, graph: u32, filter: u32, next: Then) -> Outcome {
    let entry = g(env, graph);
    if !entry.default_clock || entry.clock_provider != filter {
        return next(env, Vec::new());
    }
    entry.clock_provider = 0;
    assign(env, graph, 0, next)
}

fn member_clock(env: &mut Env<'_>, graph: u32, mut filters: VecDeque<u32>, then: Then) -> Outcome {
    let Some(filter) = filters.pop_front() else {
        return create_wine(
            env,
            CLSID_SYSTEMCLOCK,
            0,
            IID_IREFERENCECLOCK,
            move |env, clock, hr| {
                if clock == 0 {
                    return then(env, vec![hr]);
                }
                adopt(env, graph, clock, 0, then)
            },
        );
    };
    let Some(scratch) = alloc(env, 20) else {
        return then(env, vec![E_OUTOFMEMORY]);
    };
    if env.cpu.write(scratch, &IID_IREFERENCECLOCK).is_err() {
        free(env, scratch);
        return then(env, vec![E_OUTOFMEMORY]);
    }
    guest_call(
        env,
        filter,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let clock = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            if hr == S_OK && clock != 0 {
                adopt(env, graph, clock, filter, then)
            } else {
                member_clock(env, graph, filters, then)
            }
        },
    )
}

/// Keep `clock`, whose creation or query reference the graph then drops.
fn adopt(env: &mut Env<'_>, graph: u32, clock: u32, provider: u32, then: Then) -> Outcome {
    assign(
        env,
        graph,
        clock,
        Box::new(move |env, hr| {
            let entry = g(env, graph);
            entry.default_clock = true;
            entry.clock_provider = provider;
            guest_call(env, clock, 2, vec![], move |env, _| then(env, hr))
        }),
    )
}
