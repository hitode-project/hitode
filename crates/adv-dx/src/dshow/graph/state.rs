//! The graph's state as quartz reports it: transitions return the first
//! member result that is not S_OK, and GetState polls every member.

use super::*;

const VFW_S_STATE_INTERMEDIATE: u32 = 0x0004_0237;
const INFINITE: u32 = u32::MAX;
/// Milliseconds a member still changing state is given per poll, as quartz.
const POLL_MS: u32 = 10;

/// The first member result that is not S_OK, else S_OK.
pub(super) fn combined(results: &[u32]) -> u32 {
    results
        .iter()
        .copied()
        .find(|&hr| hr != S_OK)
        .unwrap_or(S_OK)
}

/// A transition's reply: the members' combined result.
pub(super) fn reply() -> Then {
    Box::new(|_, results| Outcome::Return(combined(&results)))
}

/// IMediaFilter::GetState: the graph's own state, with S_OK only once every
/// member reports it. A member still pausing makes VFW_S_STATE_INTERMEDIATE,
/// polled until `timeout` milliseconds pass.
pub(super) fn get(env: &mut Env<'_>, graph: u32, timeout: u32, output: u32) -> Outcome {
    if output == 0 {
        return Outcome::Return(E_POINTER);
    }
    let target = g(env, graph).state;
    let _ = env.cpu.write_u32(output, target);
    let Some(scratch) = alloc(env, 4) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    let end = (timeout != INFINITE).then(|| u64::from(env.sys.clock.now_ms()) + u64::from(timeout));
    poll(env, graph, end, scratch)
}

/// The fold of one GetState round.
struct Round {
    hr: u32,
    /// The member still changing state, polled with a timeout next.
    changing: u32,
}

impl Round {
    fn add(&mut self, filter: u32, hr: u32, state: u32, target: u32) {
        if hr == VFW_S_STATE_INTERMEDIATE {
            self.changing = filter;
        }
        if self.hr == S_OK && hr == VFW_S_STATE_INTERMEDIATE {
            self.hr = VFW_S_STATE_INTERMEDIATE;
        } else if hr != S_OK && hr != VFW_S_STATE_INTERMEDIATE {
            self.hr = hr;
        }
        if self.hr == S_OK && state == PAUSED && target != PAUSED {
            self.changing = filter;
            self.hr = VFW_S_STATE_INTERMEDIATE;
        } else if state != target && state != PAUSED {
            self.hr = E_FAIL;
        }
    }
}

fn poll(env: &mut Env<'_>, graph: u32, end: Option<u64>, scratch: u32) -> Outcome {
    let target = g(env, graph).state;
    let mut round = Round {
        hr: S_OK,
        changing: 0,
    };
    let mut guests = VecDeque::new();
    let state = env.ext.get::<State>();
    for member in &state.graphs[&graph].members {
        match member.host {
            Some(host) => round.add(host, S_OK, state.filters[&host].state, target),
            None => guests.push_back(member.filter),
        }
    }
    ask(env, graph, guests, round, end, scratch)
}

fn ask(
    env: &mut Env<'_>,
    graph: u32,
    mut guests: VecDeque<u32>,
    mut round: Round,
    end: Option<u64>,
    scratch: u32,
) -> Outcome {
    let Some(filter) = guests.pop_front() else {
        let expired = end.is_some_and(|end| u64::from(env.sys.clock.now_ms()) >= end);
        if round.hr != VFW_S_STATE_INTERMEDIATE || expired || round.changing == 0 {
            free(env, scratch);
            return Outcome::Return(round.hr);
        }
        return guest_call(
            env,
            round.changing,
            7,
            vec![POLL_MS, scratch],
            move |env, _| {
                if !state(env).graphs.contains_key(&graph) {
                    free(env, scratch);
                    return Outcome::Return(E_UNEXPECTED);
                }
                poll(env, graph, end, scratch)
            },
        );
    };
    let target = g(env, graph).state;
    let _ = env.cpu.write_u32(scratch, target);
    guest_call(env, filter, 7, vec![0, scratch], move |env, hr| {
        let state = env.cpu.read_u32(scratch).unwrap_or(target);
        round.add(filter, hr, state, target);
        ask(env, graph, guests, round, end, scratch)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(target: u32, members: &[(u32, u32)]) -> (u32, u32) {
        let mut round = Round {
            hr: S_OK,
            changing: 0,
        };
        for (filter, &(hr, state)) in (1..).zip(members) {
            round.add(filter, hr, state, target);
        }
        (round.hr, round.changing)
    }

    // spec: MEDIA-GRAPH-18
    #[test]
    fn get_state_folds_member_results_as_quartz_does() {
        assert_eq!(combined(&[S_OK, 1, E_FAIL]), 1);
        assert_eq!(fold(PAUSED, &[(S_OK, PAUSED), (S_OK, PAUSED)]), (S_OK, 0));
        assert_eq!(
            fold(
                PAUSED,
                &[(S_OK, PAUSED), (VFW_S_STATE_INTERMEDIATE, PAUSED)]
            ),
            (VFW_S_STATE_INTERMEDIATE, 2)
        );
        // A member still paused while the graph runs is changing state.
        assert_eq!(
            fold(RUNNING, &[(S_OK, RUNNING), (S_OK, PAUSED)]),
            (VFW_S_STATE_INTERMEDIATE, 2)
        );
        // A member in another settled state fails the graph.
        assert_eq!(fold(RUNNING, &[(S_OK, STOPPED)]), (E_FAIL, 0));
        assert_eq!(fold(STOPPED, &[(E_NOTIMPL, STOPPED)]), (E_NOTIMPL, 0));
    }
}
