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

type Settled = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

/// IMediaFilter::GetState: the graph's own state, with S_OK only once every
/// member reports it. A member still pausing makes VFW_S_STATE_INTERMEDIATE,
/// polled until `timeout` milliseconds pass.
pub(super) fn get(env: &mut Env<'_>, graph: u32, timeout: u32, output: u32) -> Outcome {
    if output == 0 {
        return Outcome::Return(E_POINTER);
    }
    let target = g(env, graph).state;
    let _ = env.cpu.write_u32(output, target);
    let end = (timeout != INFINITE).then(|| u64::from(env.sys.clock.now_ms()) + u64::from(timeout));
    let poll = Poll {
        graph,
        target,
        end,
        stop: |_, _| false,
    };
    poll.start(env, Box::new(|_, hr| Outcome::Return(hr)))
}

/// Wait until every member has paused for a pending IMediaControl::Run; S_OK
/// when they have, otherwise the failing member result. Ends early when the
/// graph no longer waits to run.
pub(super) fn settle(env: &mut Env<'_>, graph: u32, then: Settled) -> Outcome {
    let poll = Poll {
        graph,
        target: PAUSED,
        end: None,
        stop: |env, graph| {
            env.ext
                .get::<State>()
                .graphs
                .get(&graph)
                .is_none_or(|g| g.pending_run.is_none())
        },
    };
    poll.start(env, then)
}

/// After IMediaControl::Run paused the members: whether to wait for them
/// asynchronously. A member answering GetState(0) with anything other than
/// S_OK or VFW_S_STATE_INTERMEDIATE (VFW_S_CANT_CUE) means quartz runs at once.
pub(super) fn cue(env: &mut Env<'_>, graph: u32, then: Decided) -> Outcome {
    let mut cued = true;
    let mut guests = VecDeque::new();
    for (filter, host) in members(env, graph) {
        match host {
            Some(host) => {
                cued &= matches!(host_state(env, host).0, S_OK | VFW_S_STATE_INTERMEDIATE);
            }
            None => guests.push_back(filter),
        }
    }
    let Some(scratch) = alloc(env, 4) else {
        return then(env, cued);
    };
    cue_guests(env, guests, cued, scratch, then)
}

fn cue_guests(
    env: &mut Env<'_>,
    mut guests: VecDeque<u32>,
    cued: bool,
    scratch: u32,
    then: Decided,
) -> Outcome {
    let Some(filter) = guests.pop_front() else {
        free(env, scratch);
        return then(env, cued);
    };
    guest_call(env, filter, 7, vec![0, scratch], move |env, hr| {
        let cued = cued && matches!(hr, S_OK | VFW_S_STATE_INTERMEDIATE);
        cue_guests(env, guests, cued, scratch, then)
    })
}

/// Each member with its host filter, if any.
fn members(env: &Env<'_>, graph: u32) -> Vec<(u32, Option<u32>)> {
    env.ext.get::<State>().graphs[&graph]
        .members
        .iter()
        .map(|m| (m.filter, m.host))
        .collect()
}

/// A host member's GetState answer and state.
fn host_state(env: &mut Env<'_>, host: u32) -> (u32, u32) {
    let filter = &env.ext.get::<State>().filters[&host];
    let (state, vmr) = (filter.state, filter.vmr.is_some());
    let hr = if vmr {
        super::super::vmr::state_result(env, host)
    } else {
        S_OK
    };
    (hr, state)
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

/// Rounds of member GetState(0) until none is changing state, waiting
/// [`POLL_MS`] on the member that is between rounds.
#[derive(Clone, Copy)]
struct Poll {
    graph: u32,
    target: u32,
    /// Milliseconds clock deadline; none waits as long as members change.
    end: Option<u64>,
    /// Whether the wait no longer matters.
    stop: fn(&Env<'_>, u32) -> bool,
}

impl Poll {
    fn start(self, env: &mut Env<'_>, then: Settled) -> Outcome {
        let Some(scratch) = alloc(env, 4) else {
            return then(env, E_OUTOFMEMORY);
        };
        self.round(env, scratch, then)
    }

    fn round(self, env: &mut Env<'_>, scratch: u32, then: Settled) -> Outcome {
        if !state(env).graphs.contains_key(&self.graph) {
            free(env, scratch);
            return then(env, E_UNEXPECTED);
        }
        let mut round = Round {
            hr: S_OK,
            changing: 0,
        };
        let mut guests = VecDeque::new();
        for (filter, host) in members(env, self.graph) {
            match host {
                Some(host) => {
                    let (hr, state) = host_state(env, host);
                    round.add(host, hr, state, self.target);
                }
                None => guests.push_back(filter),
            }
        }
        self.ask(env, guests, round, scratch, then)
    }

    fn ask(
        self,
        env: &mut Env<'_>,
        mut guests: VecDeque<u32>,
        mut round: Round,
        scratch: u32,
        then: Settled,
    ) -> Outcome {
        if let Some(filter) = guests.pop_front() {
            let target = self.target;
            let _ = env.cpu.write_u32(scratch, target);
            return guest_call(env, filter, 7, vec![0, scratch], move |env, hr| {
                let state = env.cpu.read_u32(scratch).unwrap_or(target);
                round.add(filter, hr, state, target);
                self.ask(env, guests, round, scratch, then)
            });
        }
        let now = u64::from(env.sys.clock.now_ms());
        let expired = self.end.is_some_and(|end| now >= end);
        if round.hr != VFW_S_STATE_INTERMEDIATE
            || expired
            || round.changing == 0
            || (self.stop)(env, self.graph)
        {
            free(env, scratch);
            return then(env, round.hr);
        }
        let wait = self
            .end
            .map_or(POLL_MS, |end| (end - now).min(u64::from(POLL_MS)) as u32);
        // A host member changes state on its own streaming; wait and look again.
        if env.ext.get::<State>().filters.contains_key(&round.changing) {
            return Outcome::block(
                Wait::Until(env.sys.clock.now_us() + u64::from(wait) * 1000),
                move |env, _| self.round(env, scratch, then),
            );
        }
        guest_call(
            env,
            round.changing,
            7,
            vec![wait, scratch],
            move |env, _| self.round(env, scratch, then),
        )
    }
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
