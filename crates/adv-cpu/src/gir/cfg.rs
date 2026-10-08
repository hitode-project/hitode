//! Bounded, context-sensitive direct-call discovery over a supplied decode set.
use super::{ir::BlockId, lift::classify, verify::Error};
use iced_x86::{Code, FlowControl, Instruction};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy)]
pub struct Limits {
    pub depth: usize,
    pub instructions: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            depth: 4,
            instructions: 2048,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Boundary {
    Missing,
    Recursive,
    Depth,
    Instructions,
    Indirect,
}
pub struct Planned {
    pub instructions: Vec<u32>,
    pub targets: BTreeMap<u32, BlockId>,
    pub expected_return: Option<u32>,
    pub depth: usize,
}
pub struct Plan {
    pub blocks: Vec<Planned>,
    pub entry: BlockId,
    pub inlined_calls: usize,
    pub call_boundaries: BTreeMap<Boundary, usize>,
}
#[derive(Clone)]
struct Context {
    parent: Option<usize>,
    entry: u32,
    return_pc: Option<u32>,
    depth: usize,
}

fn successors(i: &Instruction) -> Vec<u32> {
    if classify(i).is_err() {
        return vec![];
    }
    match i.flow_control() {
        FlowControl::Next | FlowControl::Call => vec![i.next_ip32()],
        FlowControl::UnconditionalBranch => vec![i.near_branch32()],
        FlowControl::ConditionalBranch => vec![i.near_branch32(), i.next_ip32()],
        _ => vec![],
    }
}
fn body(input: &BTreeMap<u32, Instruction>, entry: u32) -> BTreeSet<u32> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![entry];
    while let Some(pc) = pending.pop() {
        if let Some(i) = input.get(&pc)
            && seen.insert(pc)
        {
            pending.extend(successors(i));
        }
    }
    seen
}
fn recursive(input: &BTreeMap<u32, Instruction>, entry: u32) -> bool {
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    let mut pending = vec![(entry, false)];
    while let Some((function, finished)) = pending.pop() {
        if finished {
            active.remove(&function);
            done.insert(function);
            continue;
        }
        if active.contains(&function) {
            return true;
        }
        if done.contains(&function) {
            continue;
        }
        active.insert(function);
        pending.push((function, true));
        for pc in body(input, function) {
            let i = &input[&pc];
            if i.code() == Code::Call_rel32_32 && input.contains_key(&i.near_branch32()) {
                pending.push((i.near_branch32(), false));
            }
        }
    }
    false
}

pub fn discover(
    input: &BTreeMap<u32, Instruction>,
    start: u32,
    limits: Limits,
) -> Result<Plan, Error> {
    if !input.contains_key(&start) {
        return Err(Error::region("missing entry instruction"));
    }
    // The root keeps the complete decode set for admission census, even if a
    // supplied instruction is unreachable. Each callee gets its own context.
    let mut contexts = vec![Context {
        parent: None,
        entry: start,
        return_pc: None,
        depth: 0,
    }];
    let mut nodes: BTreeMap<(usize, u32), Vec<(usize, u32)>> =
        input.keys().map(|&pc| ((0, pc), vec![])).collect();
    let mut queue: VecDeque<_> = nodes.keys().copied().collect();
    let mut inlined_calls = 0;
    let mut call_boundaries = BTreeMap::new();
    let mut recursion = BTreeMap::new();
    while let Some((cx, pc)) = queue.pop_front() {
        let i = &input[&pc];
        let c = contexts[cx].clone();
        let mut targets: Vec<_> = successors(i)
            .into_iter()
            .filter(|pc| input.contains_key(pc))
            .map(|pc| (cx, pc))
            .collect();
        if classify(i).is_ok() && i.flow_control() == FlowControl::Call {
            let target = i.near_branch32();
            let members = body(input, target);
            let reason = if !input.contains_key(&target) {
                Some(Boundary::Missing)
            } else if *recursion
                .entry(target)
                .or_insert_with(|| recursive(input, target))
                || target == c.entry
            {
                Some(Boundary::Recursive)
            } else if c.depth >= limits.depth {
                Some(Boundary::Depth)
            } else if nodes.len().saturating_add(members.len()) > limits.instructions {
                Some(Boundary::Instructions)
            } else {
                None
            };
            targets.clear();
            if let Some(reason) = reason {
                *call_boundaries.entry(reason).or_default() += 1;
            } else {
                let child = contexts.len();
                contexts.push(Context {
                    parent: Some(cx),
                    entry: target,
                    return_pc: Some(i.next_ip32()),
                    depth: c.depth + 1,
                });
                for member in members {
                    nodes.insert((child, member), vec![]);
                    queue.push_back((child, member));
                }
                targets.push((child, target));
                inlined_calls += 1;
            }
        } else if i.flow_control() == FlowControl::IndirectCall {
            *call_boundaries.entry(Boundary::Indirect).or_default() += 1;
        } else if i.flow_control() == FlowControl::Return
            && let Some(parent) = c.parent
        {
            let ret = c.return_pc.unwrap();
            if input.contains_key(&ret) {
                targets.push((parent, ret));
            }
        }
        *nodes.get_mut(&(cx, pc)).unwrap() = targets;
    }
    // Root successors of calls are retained as independent roots even when the
    // callee cannot be inlined. Only real CFG edges participate in grouping.
    let mut predecessors = BTreeMap::<_, usize>::new();
    for targets in nodes.values() {
        for &t in targets {
            *predecessors.entry(t).or_default() += 1;
        }
    }
    let mut owner = BTreeMap::new();
    let mut groups: Vec<Vec<(usize, u32)>> = vec![];
    for &key in nodes.keys() {
        if owner.contains_key(&key) {
            continue;
        }
        let index = BlockId(groups.len());
        let mut group = vec![];
        let mut current = key;
        loop {
            owner.insert(current, index);
            group.push(current);
            let i = &input[&current.1];
            let targets = &nodes[&current];
            if i.flow_control() != FlowControl::Next || targets.len() != 1 {
                break;
            }
            let target = targets[0];
            if target == (0, start)
                || target.0 != current.0
                || predecessors.get(&target) != Some(&1)
                || owner.contains_key(&target)
            {
                break;
            }
            current = target;
        }
        groups.push(group);
    }
    let blocks = groups
        .into_iter()
        .map(|group| {
            let last = *group.last().unwrap();
            let cx = &contexts[last.0];
            Planned {
                instructions: group.iter().map(|k| k.1).collect(),
                targets: nodes[&last]
                    .iter()
                    .filter_map(|k| owner.get(k).map(|b| (k.1, *b)))
                    .collect(),
                expected_return: cx.return_pc,
                depth: cx.depth,
            }
        })
        .collect();
    Ok(Plan {
        blocks,
        entry: owner[&(0, start)],
        inlined_calls,
        call_boundaries,
    })
}
