//! Structural, type, dominance, recovery, and effect-order validation.
use super::ir::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub block: Option<BlockId>,
    pub operation: Option<usize>,
    pub message: String,
}
impl Error {
    pub fn region(message: &str) -> Self {
        Self {
            block: None,
            operation: None,
            message: message.into(),
        }
    }
    fn at(block: usize, operation: usize, message: &str) -> Self {
        Self {
            block: Some(BlockId(block)),
            operation: Some(operation),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "block {:?}, operation {:?}: {}",
            self.block, self.operation, self.message
        )
    }
}

pub fn verify(r: &Region) -> Result<(), Error> {
    if r.semantic_version != 1 || r.entry.0 >= r.blocks.len() {
        return Err(Error::region("semantic version or entry"));
    }
    if r.recovery.len() != r.snapshots.len() {
        return Err(Error::region("missing recovery DAG"));
    }
    for (dag, snapshot) in r.recovery.iter().zip(&r.snapshots) {
        dag.verify(r, &snapshot.state)?;
    }
    let inlined = r
        .blocks
        .iter()
        .filter(|b| b.inline_depth > 0)
        .flat_map(|b| &b.operations)
        .filter(|op| matches!(op, Operation::Commit { .. }))
        .count();
    if inlined > r.inline_budget {
        return Err(Error::region("inline instruction budget exceeded"));
    }
    let mut defs = vec![None; r.values.len()];
    let mut memories = vec![false; r.memory.len()];
    let mut commits = vec![None; r.commits.len()];
    let mut predecessors = vec![vec![]; r.blocks.len()];
    let observed_snapshots: BTreeSet<_> = r
        .memory
        .iter()
        .map(|m| m.snapshot)
        .chain(r.blocks.iter().filter_map(|b| match b.terminator {
            Terminator::Guard { failed, .. } => Some(failed),
            _ => None,
        }))
        .collect();
    for (b, block) in r.blocks.iter().enumerate() {
        let mut define = |id: ValueId, position: usize| -> Result<(), Error> {
            let Some(slot) = defs.get_mut(id.0) else {
                return Err(Error::at(b, position, "value index"));
            };
            if slot.replace((b, position)).is_some() {
                return Err(Error::at(b, position, "duplicate SSA definition"));
            }
            Ok(())
        };
        if block.inline_depth > r.inline_limit {
            return Err(Error::at(b, 0, "inline depth exceeds region limit"));
        }
        if BlockId(b) == r.entry && block.parameters.len() != 10 {
            return Err(Error::at(b, 0, "entry ABI parameter count"));
        }
        for (index, &id) in block.parameters.iter().enumerate() {
            define(id, 0)?;
            if r.values[id.0].kind
                != (ValueKind::Parameter {
                    block: BlockId(b),
                    index,
                })
                || (BlockId(b) == r.entry
                    && r.values[id.0].width != if index == 9 { Width::Token } else { Width::B32 })
            {
                return Err(Error::at(b, 0, "parameter identity or width"));
            }
        }
        for (n, op) in block.operations.iter().enumerate() {
            let pos = n + 1;
            match op {
                Operation::Define(id) => {
                    define(*id, pos)?;
                    if matches!(
                        r.values[id.0].kind,
                        ValueKind::Parameter { .. } | ValueKind::Load(_) | ValueKind::Effect(_)
                    ) {
                        return Err(Error::at(b, pos, "invalid pure definition"));
                    }
                }
                Operation::Memory(id) => {
                    let Some(m) = r.memory.get(id.0) else {
                        return Err(Error::at(b, pos, "memory index"));
                    };
                    if std::mem::replace(&mut memories[id.0], true) {
                        return Err(Error::at(b, pos, "duplicate memory operation"));
                    }
                    define(m.output, pos)?;
                    if r.values[m.output.0]
                        != (Value {
                            width: Width::Token,
                            kind: ValueKind::Effect(*id),
                        })
                    {
                        return Err(Error::at(b, pos, "effect result identity"));
                    }
                    if let Some(result) = m.result {
                        define(result, pos)?;
                        if r.values[result.0]
                            != (Value {
                                width: m.width,
                                kind: ValueKind::Load(*id),
                            })
                        {
                            return Err(Error::at(b, pos, "load result identity"));
                        }
                    }
                }
                Operation::Commit { point, .. } => {
                    let Some(slot) = commits.get_mut(point.0) else {
                        return Err(Error::at(b, pos, "commit index"));
                    };
                    if slot.replace((b, pos)).is_some() {
                        return Err(Error::at(b, pos, "duplicate commit"));
                    }
                }
            }
        }
        for edge in block.terminator.edges() {
            let Some(target) = r.blocks.get(edge.block.0) else {
                return Err(Error::at(
                    b,
                    block.operations.len() + 1,
                    "terminator target",
                ));
            };
            if target.parameters.len() != edge.arguments.len() {
                return Err(Error::at(
                    b,
                    block.operations.len() + 1,
                    "edge parameter count",
                ));
            }
            let valid_depth = match block.terminator {
                Terminator::Call { .. } => target.inline_depth == block.inline_depth + 1,
                Terminator::Return { .. } => target.inline_depth + 1 == block.inline_depth,
                _ => target.inline_depth == block.inline_depth,
            };
            if !valid_depth {
                return Err(Error::at(b, 0, "inline context edge depth"));
            }
            predecessors[edge.block.0].push(b);
        }
    }
    if defs.iter().any(Option::is_none)
        || memories.contains(&false)
        || commits.iter().any(Option::is_none)
    {
        return Err(Error::region("orphan value or memory definition"));
    }
    let mut reachable = BTreeSet::new();
    let mut pending = vec![r.entry.0];
    while let Some(b) = pending.pop() {
        if reachable.insert(b) {
            pending.extend(r.blocks[b].terminator.edges().iter().map(|e| e.block.0));
        }
    }
    let mut dom: Vec<_> = (0..r.blocks.len())
        .map(|b| {
            if b == r.entry.0 || !reachable.contains(&b) {
                BTreeSet::from([b])
            } else {
                reachable.clone()
            }
        })
        .collect();
    loop {
        let mut changed = false;
        for &b in &reachable {
            if b == r.entry.0 {
                continue;
            }
            let mut intersection = reachable.clone();
            for &p in &predecessors[b] {
                if reachable.contains(&p) {
                    intersection = intersection.intersection(&dom[p]).copied().collect();
                }
            }
            intersection.insert(b);
            if intersection != dom[b] {
                dom[b] = intersection;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let width = |id: ValueId| r.values.get(id.0).map(|v| v.width);
    for (b, block) in r.blocks.iter().enumerate() {
        let check_use = |id: ValueId, pos: usize| -> Result<(), Error> {
            let Some(Some((owner, at))) = defs.get(id.0) else {
                return Err(Error::at(b, pos, "undefined value"));
            };
            if (*owner == b && *at >= pos) || (*owner != b && !dom[b].contains(owner)) {
                return Err(Error::at(b, pos, "SSA use is not dominated by definition"));
            }
            Ok(())
        };
        let state_use = |state: &StateMap, pos: usize| -> Result<(), Error> {
            for id in state.values() {
                check_use(id, pos)?;
                if width(id) != Some(Width::B32) {
                    return Err(Error::at(b, pos, "incomplete architectural state width"));
                }
            }
            Ok(())
        };
        let mut state = block.entry_state.clone();
        let mut token = block.entry_token;
        let mut last_commit = None;
        let mut pending_commit = None;
        let mut wrote = false;
        let snapshot_use = |id: SnapshotId, pos: usize, state: &StateMap| -> Result<(), Error> {
            let Some(s) = r.snapshots.get(id.0) else {
                return Err(Error::at(b, pos, "missing pre-operation snapshot"));
            };
            if s.state != *state {
                return Err(Error::at(
                    b,
                    pos,
                    "snapshot is not the complete pre-commit state",
                ));
            }
            state_use(&s.state, pos)?;
            for capture in r.recovery[id.0].captures() {
                check_use(capture, pos)?;
            }
            if s.commit.0 >= r.commits.len() {
                return Err(Error::at(b, pos, "snapshot commit index"));
            }
            for &id in &s.recipes {
                check_use(id, pos)?;
                if !width(id).is_some_and(Width::integer) {
                    return Err(Error::at(b, pos, "recovery recipe type"));
                }
            }
            if !s.private_slots.is_empty() || s.lease.is_some() {
                return Err(Error::at(
                    b,
                    pos,
                    "private slots and leases require a later semantic version",
                ));
            }
            for id in &s.dependencies {
                if id.0 >= r.dependencies.len() {
                    return Err(Error::at(b, pos, "snapshot dependency index"));
                }
            }
            Ok(())
        };
        for (n, op) in block.operations.iter().enumerate() {
            let pos = n + 1;
            match op {
                Operation::Define(id) => {
                    let value = &r.values[id.0];
                    for input in value.kind.inputs() {
                        check_use(input, pos)?;
                    }
                    if !type_valid(r, *id) {
                        return Err(Error::at(b, pos, "pure value type or flags contract"));
                    }
                }
                Operation::Memory(id) => {
                    let m = &r.memory[id.0];
                    check_use(m.address, pos)?;
                    check_use(m.input, pos)?;
                    if width(m.address) != Some(Width::B32)
                        || m.input != token
                        || width(m.input) != Some(Width::Token)
                    {
                        return Err(Error::at(b, pos, "address width or nonlinear effect token"));
                    }
                    if !matches!(m.width, Width::B8 | Width::B16 | Width::B32) || m.proof.is_some()
                    {
                        return Err(Error::at(b, pos, "memory width or unimplemented proof"));
                    }
                    snapshot_use(m.snapshot, pos, &state)?;
                    if r.snapshots[m.snapshot.0].commit != m.commit
                        || commits
                            .get(m.commit.0)
                            .copied()
                            .flatten()
                            .is_none_or(|(owner, at)| owner != b || at <= pos)
                    {
                        return Err(Error::at(b, pos, "memory must precede its commit"));
                    }
                    if wrote {
                        return Err(Error::at(
                            b,
                            pos,
                            "fallible operation after a visible write",
                        ));
                    }
                    if pending_commit.is_some_and(|old| old != m.commit) {
                        return Err(Error::at(b, pos, "interleaved instruction effects"));
                    }
                    pending_commit = Some(m.commit);
                    match m.access {
                        Access::Read if m.result.is_none() => {
                            return Err(Error::at(b, pos, "read needs result"));
                        }
                        Access::Read => {}
                        Access::Write { value, enabled } => {
                            check_use(value, pos)?;
                            if width(value) != Some(m.width) || m.result.is_some() {
                                return Err(Error::at(b, pos, "write operand width"));
                            }
                            if let Some(enabled) = enabled {
                                check_use(enabled, pos)?;
                                if width(enabled) != Some(Width::B1) {
                                    return Err(Error::at(b, pos, "conditional write type"));
                                }
                            }
                            wrote = true;
                        }
                    }
                    token = m.output;
                }
                Operation::Commit {
                    point,
                    state: after,
                    next_pc,
                } => {
                    let p = &r.commits[point.0];
                    for (index, s) in r.snapshots.iter().enumerate() {
                        if s.commit == *point && !observed_snapshots.contains(&SnapshotId(index)) {
                            snapshot_use(SnapshotId(index), pos, &state)?;
                        }
                    }
                    if p.units != 1
                        || p.subprogress != 0
                        || last_commit.is_some_and(|old| p.instruction <= old)
                        || pending_commit.is_some_and(|old| old != *point)
                    {
                        return Err(Error::at(b, pos, "nonmonotonic or mismatched commit point"));
                    }
                    state_use(after, pos)?;
                    check_use(*next_pc, pos)?;
                    if width(*next_pc) != Some(Width::B32) {
                        return Err(Error::at(b, pos, "commit PC type"));
                    }
                    last_commit = Some(p.instruction);
                    pending_commit = None;
                    wrote = false;
                    state = after.clone();
                }
            }
        }
        let pos = block.operations.len() + 1;
        if pending_commit.is_some() {
            return Err(Error::at(b, pos, "uncommitted memory effect"));
        }
        let exit_use = |e: &Exit| -> Result<(), Error> {
            state_use(&e.state, pos)?;
            check_use(e.pc, pos)?;
            check_use(e.token, pos)?;
            if e.state != state || width(e.pc) != Some(Width::B32) || e.token != token {
                return Err(Error::at(b, pos, "incomplete exit observation or token"));
            }
            Ok(())
        };
        let condition_use = |id| -> Result<(), Error> {
            check_use(id, pos)?;
            if width(id) != Some(Width::B1) {
                return Err(Error::at(b, pos, "branch condition type"));
            }
            Ok(())
        };
        let target_use = |t: &Target| match t {
            Target::Exit(e) => exit_use(e),
            Target::Block(_) => Ok(()),
        };
        for e in block.terminator.edges() {
            for (&argument, &parameter) in e.arguments.iter().zip(&r.blocks[e.block.0].parameters) {
                check_use(argument, pos)?;
                if width(argument) != width(parameter) {
                    return Err(Error::at(b, pos, "edge argument width"));
                }
            }
            let target = &r.blocks[e.block.0];
            let replacements: std::collections::BTreeMap<_, _> = target
                .parameters
                .iter()
                .copied()
                .zip(e.arguments.iter().copied())
                .collect();
            let resolve = |id: ValueId| replacements.get(&id).copied().unwrap_or(id);
            if target.entry_state.gpr.map(resolve) != state.gpr
                || resolve(target.entry_token) != token
                || !super::state_ssa::same_flags(
                    r,
                    target.entry_state.flags,
                    state.flags,
                    &replacements,
                )
            {
                return Err(Error::at(b, pos, "edge loses state or final token"));
            }
        }
        match &block.terminator {
            Terminator::Exit(e) => exit_use(e)?,
            Terminator::Jump(t) | Terminator::Call { callee: t, .. } => target_use(t)?,
            Terminator::Branch {
                condition,
                taken,
                fallthrough,
            } => {
                condition_use(*condition)?;
                target_use(taken)?;
                target_use(fallthrough)?;
            }
            Terminator::Return {
                actual, mismatch, ..
            } => {
                check_use(*actual, pos)?;
                if width(*actual) != Some(Width::B32) {
                    return Err(Error::at(b, pos, "return target type"));
                }
                if mismatch.pc != *actual {
                    return Err(Error::at(
                        b,
                        pos,
                        "return mismatch loses actual guest target",
                    ));
                }
                exit_use(mismatch)?;
            }
            Terminator::Guard {
                condition,
                failed,
                token: end,
                ..
            } => {
                condition_use(*condition)?;
                snapshot_use(*failed, pos, &state)?;
                if *end != token {
                    return Err(Error::at(b, pos, "guard token"));
                }
            }
        }
    }
    for s in &r.snapshots {
        if s.commit.0 >= r.commits.len()
            || s.state.values().any(|v| width(v) != Some(Width::B32))
            || s.dependencies.iter().any(|d| d.0 >= r.dependencies.len())
            || !s.private_slots.is_empty()
            || s.lease.is_some()
            || s.recipes
                .iter()
                .any(|v| !width(*v).is_some_and(Width::integer))
        {
            return Err(Error::region("invalid recovery table"));
        }
    }
    Ok(())
}

fn type_valid(r: &Region, id: ValueId) -> bool {
    let value = &r.values[id.0];
    let w = value.width;
    let width = |id: ValueId| r.values.get(id.0).map(|v| v.width);
    if !w.integer() {
        return false;
    }
    match &value.kind {
        ValueKind::Constant(n) => *n & !w.mask() == 0,
        ValueKind::Pure(p) => match p {
            Pure::Binary(kind, a, b) => {
                width(*a) == Some(w)
                    && if matches!(
                        kind,
                        Binary::Shl | Binary::Shr | Binary::Sar | Binary::Rol | Binary::Ror
                    ) {
                        matches!(w, Width::B8 | Width::B16 | Width::B32)
                            && width(*b) == Some(Width::B8)
                    } else {
                        width(*b) == Some(w)
                    }
            }
            Pure::Extract { value, lsb } => width(*value)
                .is_some_and(|old| old.integer() && u32::from(*lsb) + w.bits() <= old.bits()),
            Pure::Insert { base, value, lsb } => {
                width(*base) == Some(w)
                    && width(*value).is_some_and(|part| {
                        part.integer() && u32::from(*lsb) + part.bits() <= w.bits()
                    })
            }
            Pure::Extend { value, .. } => {
                width(*value).is_some_and(|old| old.integer() && old.bits() < w.bits())
            }
            Pure::Select { condition, yes, no } => {
                width(*condition) == Some(Width::B1)
                    && width(*yes) == Some(w)
                    && width(*no) == Some(w)
            }
            Pure::Condition { flags, code } => {
                w == Width::B1
                    && width(*flags) == Some(Width::B32)
                    && *code != iced_x86::ConditionCode::None
            }
            Pure::Nonzero(value) => w == Width::B1 && width(*value).is_some_and(Width::integer),
            Pure::FlagBit { flags, bit } => {
                w == Width::B1 && width(*flags) == Some(Width::B32) && FLAG_BITS.contains(bit)
            }
            Pure::FlagPack { bits, rest } => {
                w == Width::B32
                    && width(*rest) == Some(Width::B32)
                    && bits.iter().all(|v| width(*v) == Some(Width::B1))
            }
        },
        ValueKind::Flags(f) => {
            let expected = match f.kind {
                FlagKind::Add | FlagKind::Sub | FlagKind::Logic => ARITHMETIC,
                FlagKind::Shl | FlagKind::Shr | FlagKind::Sar => ARITHMETIC & !AF,
                _ => CF | OF,
            };
            w == Width::B32
                && matches!(f.width, Width::B8 | Width::B16 | Width::B32)
                && width(f.preserved) == Some(Width::B32)
                && width(f.lhs) == Some(f.width)
                && width(f.result) == Some(f.width)
                && width(f.carry) == Some(f.width)
                && width(f.rhs)
                    == Some(
                        if matches!(
                            f.kind,
                            FlagKind::Shl
                                | FlagKind::Shr
                                | FlagKind::Sar
                                | FlagKind::Rol
                                | FlagKind::Ror
                        ) {
                            Width::B8
                        } else {
                            f.width
                        },
                    )
                && (f.defined == expected
                    || matches!(f.kind, FlagKind::Add | FlagKind::Sub)
                        && f.defined == expected & !CF)
        }
        _ => false,
    }
}
