//! Members in the order quartz transitions them: sorted topologically from
//! their pins' connections at each change, downstream first, starting from
//! the newest member. Host members take their place among guest ones.
use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::dshow::filter::INPUT;

/// Each guest member and the filters its connected inputs come from.
type Edges = Vec<(u32, u32)>;
pub(super) type Sorted = Box<dyn FnOnce(&mut Env<'_>, Vec<u32>) -> Outcome>;

/// QueryPinInfo's PIN_INFO, then QueryDirection's and the pointer outputs.
const PIN_INFO: u32 = 264;

/// A member to transition: a host filter by its canonical pointer, or a guest
/// filter by the pointer it joined with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Node {
    Host(u32),
    Guest(u32),
}

pub(super) type Ordered = Box<dyn FnOnce(&mut Env<'_>, Vec<Node>) -> Outcome>;

/// Every member, downstream first.
pub(super) fn members(env: &mut Env<'_>, graph: u32, then: Ordered) -> Outcome {
    let joined: Vec<(u32, u32, Node)> = g(env, graph)
        .members
        .iter()
        .rev()
        .map(|m| match m.host {
            Some(host) => (host, m.filter, Node::Host(host)),
            None => (m.filter, m.filter, Node::Guest(m.filter)),
        })
        .collect();
    let nodes: Vec<(u32, Node)> = joined.iter().map(|&(key, _, node)| (key, node)).collect();
    let keys: Vec<u32> = nodes.iter().map(|&(key, _)| key).collect();
    let Some(scratch) = alloc(env, PIN_INFO + 12) else {
        return then(env, nodes.into_iter().map(|(_, node)| node).collect());
    };
    let pending = joined
        .iter()
        .map(|&(key, pointer, node)| (key, pointer, matches!(node, Node::Host(_))))
        .collect();
    upstreams(
        env,
        pending,
        scratch,
        Vec::new(),
        Box::new(move |env, edges| {
            free(env, scratch);
            let sorted = sort(&keys, &edges)
                .into_iter()
                .filter_map(|key| nodes.iter().find(|&&(k, _)| k == key).map(|&(_, n)| n))
                .collect();
            then(env, sorted)
        }),
    )
}

/// Guest members alone, in the order of [`members`].
pub(super) fn guest_members(env: &mut Env<'_>, graph: u32, then: Sorted) -> Outcome {
    members(
        env,
        graph,
        Box::new(move |env, nodes| {
            let guests = nodes
                .into_iter()
                .filter_map(|node| match node {
                    Node::Guest(filter) => Some(filter),
                    Node::Host(_) => None,
                })
                .collect();
            then(env, guests)
        }),
    )
}

fn sort(members: &[u32], edges: &Edges) -> Vec<u32> {
    let mut upstream: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &(filter, from) in edges {
        if members.contains(&from) {
            upstream.entry(filter).or_default().push(from);
        }
    }
    fn visit(
        filter: u32,
        upstream: &BTreeMap<u32, Vec<u32>>,
        seen: &mut BTreeSet<u32>,
        sorted: &mut VecDeque<u32>,
    ) {
        if !seen.insert(filter) {
            return;
        }
        for &from in upstream.get(&filter).into_iter().flatten() {
            visit(from, upstream, seen, sorted);
        }
        sorted.push_front(filter);
    }
    let mut seen = BTreeSet::new();
    let mut sorted = VecDeque::new();
    for &filter in members {
        visit(filter, &upstream, &mut seen, &mut sorted);
    }
    sorted.into()
}

type Found = Box<dyn FnOnce(&mut Env<'_>, Edges) -> Outcome>;

fn upstreams(
    env: &mut Env<'_>,
    mut pending: VecDeque<(u32, u32, bool)>,
    scratch: u32,
    edges: Edges,
    then: Found,
) -> Outcome {
    let Some((filter, pointer, host)) = pending.pop_front() else {
        return then(env, edges);
    };
    if host {
        // A host filter's own pins are known; only a guest peer's owner is asked.
        let state = env.ext.get::<State>();
        let peers: VecDeque<u32> = state.filters[&filter]
            .pins
            .iter()
            .map(|p| &state.pins[p])
            .filter(|p| p.dir == INPUT && p.peer != 0)
            .map(|p| p.peer)
            .collect();
        return host_inputs(
            env,
            filter,
            peers,
            scratch,
            edges,
            Box::new(move |env, edges| upstreams(env, pending, scratch, edges, then)),
        );
    }
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, pointer, 10, vec![scratch], move |env, hr| {
        let pins = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || pins == 0 {
            return upstreams(env, pending, scratch, edges, then);
        }
        inputs(
            env,
            filter,
            pins,
            scratch,
            edges,
            Box::new(move |env, edges| upstreams(env, pending, scratch, edges, then)),
        )
    })
}

fn host_inputs(
    env: &mut Env<'_>,
    filter: u32,
    mut peers: VecDeque<u32>,
    scratch: u32,
    mut edges: Edges,
    then: Found,
) -> Outcome {
    let Some(peer) = peers.pop_front() else {
        return then(env, edges);
    };
    if let Some(pin) = crate::dshow::filter::host_pin(env, peer) {
        edges.push((filter, env.ext.get::<State>().pins[&pin].filter));
        return host_inputs(env, filter, peers, scratch, edges, then);
    }
    let _ = env.cpu.write_u32(scratch, 0);
    guest_call(env, peer, 8, vec![scratch], move |env, hr| {
        let from = env.cpu.read_u32(scratch).unwrap_or(0);
        if hr != S_OK || from == 0 {
            return host_inputs(env, filter, peers, scratch, edges, then);
        }
        edges.push((filter, from));
        guest_call(env, from, 2, vec![], move |env, _| {
            host_inputs(env, filter, peers, scratch, edges, then)
        })
    })
}

/// Walk one filter's pins, recording the owner of each connected input's peer.
fn inputs(
    env: &mut Env<'_>,
    filter: u32,
    pins: u32,
    scratch: u32,
    edges: Edges,
    then: Found,
) -> Outcome {
    let (pin_out, dir_out, peer_out) = (
        scratch + PIN_INFO,
        scratch + PIN_INFO + 4,
        scratch + PIN_INFO + 8,
    );
    let _ = env.cpu.write_u32(pin_out, 0);
    guest_call(env, pins, 3, vec![1, pin_out, 0], move |env, hr| {
        let pin = env.cpu.read_u32(pin_out).unwrap_or(0);
        if hr != S_OK || pin == 0 {
            return guest_call(env, pins, 2, vec![], move |env, _| then(env, edges));
        }
        let next: Found = Box::new(move |env, edges| {
            guest_call(env, pin, 2, vec![], move |env, _| {
                inputs(env, filter, pins, scratch, edges, then)
            })
        });
        let _ = env.cpu.write_u32(dir_out, !INPUT);
        guest_call(env, pin, 9, vec![dir_out], move |env, hr| {
            if hr != S_OK || env.cpu.read_u32(dir_out).unwrap_or(!INPUT) != INPUT {
                return next(env, edges);
            }
            let _ = env.cpu.write_u32(peer_out, 0);
            guest_call(env, pin, 6, vec![peer_out], move |env, hr| {
                let peer = env.cpu.read_u32(peer_out).unwrap_or(0);
                if hr != S_OK || peer == 0 {
                    return next(env, edges);
                }
                let _ = env.cpu.write_u32(scratch, 0);
                guest_call(env, peer, 8, vec![scratch], move |env, hr| {
                    let from = env.cpu.read_u32(scratch).unwrap_or(0);
                    let mut edges = edges;
                    let release_peer = move |env: &mut Env<'_>, edges: Edges| {
                        guest_call(env, peer, 2, vec![], move |env, _| next(env, edges))
                    };
                    if hr != S_OK || from == 0 {
                        return release_peer(env, edges);
                    }
                    edges.push((filter, crate::dshow::canonical(env, from).unwrap_or(from)));
                    guest_call(env, from, 2, vec![], move |env, _| release_peer(env, edges))
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::sort;

    #[test]
    // spec: MEDIA-GRAPH-25
    fn downstream_members_come_before_the_filters_feeding_them() {
        // Newest first: 3 feeds 1, 1 feeds 2; 4 is unconnected.
        let members = [4, 3, 2, 1];
        let edges = vec![(1, 3), (2, 1)];
        assert_eq!(sort(&members, &edges), vec![2, 1, 3, 4]);
    }
}
