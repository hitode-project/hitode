//! Estimated code-generation work, bounded before any background submission.
//! Units are empirical complexity weights, never a wall-clock deadline.
use std::collections::{BTreeMap, BTreeSet};

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind};

use crate::{CompileStrategy, Region, compile::Snapshot};

/// Suggested starting limit for interactive owners; empirical, not real time.
pub const RESPONSIVE_WORK: usize = 128;
/// Slicing a loop makes every iteration cross regions, which costs far more
/// than one longer compile. A region with a back edge may use this multiple of
/// the slice limit as one unit, including its Speed recompilation.
const LOOP_ALLOWANCE: usize = 8;

/// Every whole Fast loop admitted by responsive(256) costs at most 2048;
/// its Speed recompilation costs at most four times as much. This is an
/// absolute work cap for dedicated compiler threads, never a frame deadline.
pub(crate) const DEDICATED_SPEED_WORK: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    limit: usize,
    loops: usize,
    speed_fallback: bool,
}

/// Use the same selection when planning and immediately before submission.
/// Call only when the service has a responsive limit: unrestricted clients
/// retain their policy. A dedicated Speed job stays whole but remains bounded;
/// shared-worker Speed and every Fast job keep the ordinary loop allowance.
pub(crate) fn background_budget(
    strategy: CompileStrategy,
    responsive_limit: usize,
    dedicated: bool,
) -> Budget {
    if dedicated && strategy == CompileStrategy::Speed {
        Budget {
            limit: DEDICATED_SPEED_WORK,
            loops: 1,
            speed_fallback: false,
        }
    } else {
        Budget {
            limit: responsive_limit,
            loops: LOOP_ALLOWANCE,
            speed_fallback: true,
        }
    }
}

fn successors(instruction: &Instruction) -> Vec<u32> {
    let mut next = Vec::with_capacity(2);
    if instruction.op0_kind() == OpKind::NearBranch32 {
        next.push(instruction.near_branch32());
    }
    if !instruction.is_invalid()
        && !matches!(
            instruction.flow_control(),
            FlowControl::UnconditionalBranch
                | FlowControl::IndirectBranch
                | FlowControl::Return
                | FlowControl::Exception
        )
    {
        next.push(instruction.next_ip32());
    }
    next
}

// Match the compiler's linear scan plus independently decoded entry/target graph.
// This includes authored regions and overlapping x86 instruction boundaries.
fn decoded(input: &Snapshot) -> BTreeMap<u32, Instruction> {
    let region = &input.region;
    let bytes = input.bytes();
    let mut instructions = BTreeMap::new();
    if !region.reachable {
        let mut decoder =
            Decoder::with_ip(32, bytes, u64::from(region.start), DecoderOptions::NONE);
        while decoder.can_decode() && decoder.ip() < u64::from(region.start) + region.len as u64 {
            let instruction = decoder.decode();
            instructions.insert(instruction.ip32(), instruction);
        }
    }
    let private_callee = |pc: u32| {
        region.reachable
            && region.callees.contains(&pc)
            && pc != region.start
            && !region.entries.contains(&pc)
    };
    let mut pending = region.entries.clone();
    if region.reachable && pending.is_empty() {
        pending.push(region.start);
    }
    if !region.reachable {
        pending.extend(
            instructions
                .values()
                .filter(|i| i.op0_kind() == OpKind::NearBranch32)
                .map(Instruction::near_branch32),
        );
    }
    while let Some(target) = pending.pop() {
        if instructions.contains_key(&target)
            || region.frontier.contains(&target)
            || private_callee(target)
        {
            continue;
        }
        let Some(offset) = target
            .checked_sub(region.start)
            .map(|n| n as usize)
            .filter(|&n| n < region.len)
        else {
            continue;
        };
        let offset = offset + (region.start - input.origin()) as usize;
        let mut decoder = Decoder::with_ip(
            32,
            &bytes[offset..],
            u64::from(target),
            DecoderOptions::NONE,
        );
        while decoder.can_decode()
            && decoder.ip() < u64::from(region.start) + region.len as u64
            && !instructions.contains_key(&(decoder.ip() as u32))
            && !region.frontier.contains(&(decoder.ip() as u32))
            && !private_callee(decoder.ip() as u32)
        {
            let i = decoder.decode();
            if i.op0_kind() == OpKind::NearBranch32 {
                pending.push(i.near_branch32());
            }
            instructions.insert(i.ip32(), i);
            if region.reachable
                && (i.is_invalid()
                    || matches!(
                        i.flow_control(),
                        FlowControl::UnconditionalBranch
                            | FlowControl::IndirectBranch
                            | FlowControl::Return
                            | FlowControl::Exception
                    ))
            {
                break;
            }
        }
    }
    // Callee code is bounded by `Snapshot::callee_code` and not budgeted:
    // adding it must neither split nor refuse the unit that observed it.
    instructions
}

fn weight(i: &Instruction) -> usize {
    let memory = i.op_kinds().any(|kind| {
        matches!(
            kind,
            OpKind::Memory
                | OpKind::MemorySegSI
                | OpKind::MemorySegESI
                | OpKind::MemorySegRSI
                | OpKind::MemorySegDI
                | OpKind::MemorySegEDI
                | OpKind::MemorySegRDI
                | OpKind::MemoryESDI
                | OpKind::MemoryESEDI
                | OpKind::MemoryESRDI
        )
    });
    let vector = (0..i.op_count()).any(|op| {
        let register = i.op_register(op);
        register.is_vector_register() || register.is_mm()
    });
    if crate::strings::operation(i).is_some() {
        return 32;
    }
    if vector {
        return if memory { 32 } else { 8 };
    }
    if matches!(i.mnemonic(), Mnemonic::Div | Mnemonic::Idiv) {
        return 32;
    }
    if memory {
        return if i.op0_kind() == OpKind::Memory
            && matches!(
                i.mnemonic(),
                Mnemonic::Add
                    | Mnemonic::Adc
                    | Mnemonic::Sub
                    | Mnemonic::Sbb
                    | Mnemonic::And
                    | Mnemonic::Or
                    | Mnemonic::Xor
                    | Mnemonic::Inc
                    | Mnemonic::Dec
                    | Mnemonic::Neg
                    | Mnemonic::Not
                    | Mnemonic::Shl
                    | Mnemonic::Shr
                    | Mnemonic::Sar
                    | Mnemonic::Rol
                    | Mnemonic::Ror
                    | Mnemonic::Rcl
                    | Mnemonic::Rcr
                    | Mnemonic::Xchg
                    | Mnemonic::Xadd
                    | Mnemonic::Cmpxchg
                    | Mnemonic::Bts
                    | Mnemonic::Btr
                    | Mnemonic::Btc
                    | Mnemonic::Shld
                    | Mnemonic::Shrd
            ) {
            16
        } else {
            8
        };
    }
    if i.flow_control() != FlowControl::Next {
        return 8;
    }
    1
}

fn total(instructions: &BTreeMap<u32, Instruction>, strategy: CompileStrategy) -> usize {
    let multiplier = if strategy == CompileStrategy::Speed {
        4
    } else {
        1
    };
    instructions
        .values()
        .map(weight)
        .sum::<usize>()
        .saturating_mul(multiplier)
}

fn has_loop(instructions: &BTreeMap<u32, Instruction>) -> bool {
    instructions.iter().any(|(&pc, i)| {
        i.op0_kind() == OpKind::NearBranch32
            && i.near_branch32() <= pc
            && instructions.contains_key(&i.near_branch32())
    })
}

fn allowance(instructions: &BTreeMap<u32, Instruction>, limit: usize, loops: usize) -> usize {
    if has_loop(instructions) {
        limit.saturating_mul(loops)
    } else {
        limit
    }
}

#[cfg(test)]
pub(crate) fn work(input: &Snapshot) -> usize {
    total(&decoded(input), input.region.compile_strategy)
}

/// The input's work and the most a single shared-worker job may take.
#[cfg(test)]
pub(crate) fn work_and_allowance(input: &Snapshot, limit: usize) -> (usize, usize) {
    work_and_allowance_with_budget(
        input,
        background_budget(input.region.compile_strategy, limit, false),
    )
}

pub(crate) fn work_and_allowance_with_budget(input: &Snapshot, budget: Budget) -> (usize, usize) {
    let instructions = decoded(input);
    let work = total(&instructions, input.region.compile_strategy);
    (work, allowance(&instructions, budget.limit, budget.loops))
}

/// Partition Fast work while retaining exact guest instruction/edge semantics.
/// Speed is deliberately never partially substituted for a live larger Fast unit.
#[cfg(test)]
pub(crate) fn plan(input: Snapshot, limit: usize) -> Result<Vec<Snapshot>, String> {
    let budget = background_budget(input.region.compile_strategy, limit, false);
    plan_with_budget(input, budget)
}

pub(crate) fn plan_with_budget(input: Snapshot, budget: Budget) -> Result<Vec<Snapshot>, String> {
    plan_within(input, budget.limit, budget.loops, budget.speed_fallback)
}

fn plan_within(
    input: Snapshot,
    limit: usize,
    loops: usize,
    speed_fallback: bool,
) -> Result<Vec<Snapshot>, String> {
    let instructions = decoded(&input);
    if let Some(entry) = input
        .region
        .entries
        .iter()
        .find(|pc| !instructions.contains_key(pc))
    {
        return Err(format!("entry {entry:#x} is not an instruction boundary"));
    }
    let amount = total(&instructions, input.region.compile_strategy);
    let whole = allowance(&instructions, limit, loops);
    if amount <= whole {
        return Ok(vec![input]);
    }
    if input.region.compile_strategy == CompileStrategy::Speed {
        // Keeping a hot unit whole with its callees saves more than Speed.
        if speed_fallback
            && !input.region.callees.is_empty()
            && total(&instructions, CompileStrategy::Fast) <= whole
        {
            let mut input = input;
            input.region.compile_strategy = CompileStrategy::Fast;
            return Ok(vec![input]);
        }
        return Err(format!(
            "Speed work {amount} exceeds background budget {whole}"
        ));
    }
    // Fragments start their own windows; they do not keep callees.
    if !input.region.callees.is_empty() {
        let mut input = input;
        input.region.callees.clear();
        return plan_within(input, limit, loops, speed_fallback);
    }
    let mut groups: Vec<BTreeSet<u32>> = Vec::new();
    let mut current = BTreeSet::new();
    let mut used = 0;
    for (&pc, instruction) in &instructions {
        let cost = weight(instruction);
        if cost > limit {
            return Err(format!(
                "instruction work {cost} exceeds background budget {limit}"
            ));
        }
        if used + cost > limit {
            groups.push(std::mem::take(&mut current));
            used = 0;
        }
        current.insert(pc);
        used += cost;
    }
    if !current.is_empty() {
        groups.push(current);
    }
    let owner: BTreeMap<_, _> = groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| group.iter().map(move |&pc| (pc, index)))
        .collect();
    let mut roots: Vec<_> = groups
        .iter()
        .map(|group| BTreeSet::from([*group.first().unwrap()]))
        .collect();
    let mut frontiers: Vec<BTreeSet<u32>> = groups
        .iter()
        .map(|_| input.region.frontier.iter().copied().collect())
        .collect();
    for pc in &input.region.entries {
        if let Some(&index) = owner.get(pc) {
            roots[index].insert(*pc);
        }
    }
    // Assign each edge once. In particular, a 16-KiB authored NOP region must
    // not scan all instructions again for each of its 128 small fragments.
    for (&pc, instruction) in &instructions {
        let from = owner[&pc];
        for next in successors(instruction) {
            let to = owner.get(&next).copied();
            if to != Some(from) {
                frontiers[from].insert(next);
                if let Some(to) = to {
                    roots[to].insert(next);
                }
            }
        }
        if !input.region.reachable
            && instruction.flow_control() != FlowControl::Next
            && let Some(&to) = owner.get(&instruction.next_ip32())
        {
            roots[to].insert(instruction.next_ip32());
        }
    }
    let mut fragments = Vec::with_capacity(groups.len());
    for ((selected, roots), mut frontier) in groups.into_iter().zip(roots).zip(frontiers) {
        let start = *selected.first().unwrap();
        let end = selected
            .iter()
            .map(|pc| u64::from(*pc) + instructions[pc].len() as u64)
            .max()
            .unwrap();
        let mut region = Region::new(start, (end - u64::from(start)) as usize);
        region.reachable = true;
        region.complete_tail = input.region.complete_tail;
        region.compile_strategy = input.region.compile_strategy;
        // Overlapping instruction starts can fall inside an adjacent fragment's
        // byte interval. A frontier suppresses the other independent decode.
        frontier.extend(
            instructions
                .range(start..)
                .take_while(|(pc, _)| u64::from(**pc) < end)
                .filter_map(|(&pc, _)| (!selected.contains(&pc)).then_some(pc)),
        );
        frontier.retain(|pc| !selected.contains(pc));
        region.entries = roots.into_iter().collect();
        region.frontier = frontier.into_iter().collect();
        // Actual graph cost is checked by Worker on every enqueue. Do not decode
        // every piece twice on the owner while preparing a large authored input.
        fragments.push(input.fragment(region)?);
    }
    Ok(fragments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, Memory, READ, WRITE};
    fn input(bytes: &[u8]) -> Snapshot {
        let mut memory = Memory::new(0x10000, 0x10000).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, bytes).unwrap();
        let mut region = Region::new(0x10000, bytes.len());
        region.compile_strategy = CompileStrategy::Fast;
        Snapshot::capture(&region, &mut memory).unwrap()
    }
    #[test]
    fn memory_and_vector_costs_differ_and_speed_never_bypasses_the_limit() {
        assert_eq!(work(&input(&[0x90])), 1);
        assert_eq!(work(&input(&[0x8b, 0x00])), 8);
        assert_eq!(work(&input(&[0x0f, 0x10, 0x00])), 32);
        let mut original = input(&[0x8b, 0x00, 0x8b, 0x00]);
        original.region.compile_strategy = CompileStrategy::Speed;
        assert!(plan(original, 32).is_err());
    }
    #[test]
    fn callees_keep_a_hot_unit_whole_when_speed_does_not_fit() {
        let mut original = input(&[0x8b, 0x00, 0x8b, 0x00]);
        original.region.compile_strategy = CompileStrategy::Speed;
        original.region.callees = vec![0x10000];
        let fragments = plan(original.clone(), 32).unwrap();
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].region.compile_strategy, CompileStrategy::Fast);
        assert_eq!(fragments[0].region.callees, [0x10000]);
        // Splitting still bounds Fast work; fragments do not keep callees.
        original.region.compile_strategy = CompileStrategy::Fast;
        let fragments = plan(original, 8).unwrap();
        assert_eq!(fragments.len(), 2);
        assert!(fragments.iter().all(|f| f.region.callees.is_empty()));
    }
    #[test]
    fn authored_large_regions_are_sliced_on_full_instruction_boundaries() {
        let mut bytes = [0x8b, 0x05, 0, 0, 2, 0].repeat(32);
        bytes.push(0xc3);
        let fragments = plan(input(&bytes), RESPONSIVE_WORK).unwrap();
        assert_eq!(fragments.len(), 3);
        assert!(fragments.iter().all(|input| work(input) <= RESPONSIVE_WORK));
        assert_eq!(fragments[0].region.len, 96);
        assert!(fragments[0].region.frontier.contains(&(0x10000 + 96)));
    }

    #[test]
    fn small_loops_compile_whole_and_may_tier_up() {
        // mov eax,[eax] x4; dec ecx; jnz back; ret: work 49 over a limit of 16.
        let mut bytes = [0x8b, 0x00].repeat(4);
        bytes.extend([0x49, 0x75, 0xf7, 0xc3]);
        let original = input(&bytes);
        assert_eq!(work(&original), 49);
        assert_eq!(plan(original.clone(), 16).unwrap().len(), 1);
        assert_eq!(work_and_allowance(&original, 16), (49, 128));
        let mut speed = original;
        speed.region.compile_strategy = CompileStrategy::Speed;
        assert_eq!(plan(speed.clone(), 32).unwrap().len(), 1);
        assert!(plan(speed, 16).is_err());
        // Without a back edge the same work is still sliced.
        let mut straight = [0x8b, 0x00].repeat(6);
        straight.push(0xc3);
        assert!(plan(input(&straight), 16).unwrap().len() > 1);
    }

    fn memory_loop(accesses: usize) -> Snapshot {
        let mut bytes = [0x8b, 0x00].repeat(accesses);
        bytes.push(0x49); // dec ecx
        let displacement = -i32::try_from(bytes.len() + 6).unwrap();
        bytes.extend([0x0f, 0x85]); // jnz begin
        bytes.extend(displacement.to_le_bytes());
        bytes.push(0xc3);
        input(&bytes)
    }

    #[test]
    fn dedicated_speed_admits_whole_hot_loops_with_an_absolute_cap() {
        let mut hot = memory_loop(64);
        hot.region.compile_strategy = CompileStrategy::Speed;
        let shared = background_budget(CompileStrategy::Speed, 256, false);
        let dedicated = background_budget(CompileStrategy::Speed, 256, true);
        assert_eq!(work_and_allowance_with_budget(&hot, shared), (2116, 2048));
        assert!(plan_with_budget(hot.clone(), shared).is_err());
        assert_eq!(
            work_and_allowance_with_budget(&hot, dedicated),
            (2116, 8192)
        );
        let planned = plan_with_budget(hot, dedicated).unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].region.compile_strategy, CompileStrategy::Speed);

        let mut near_cap = memory_loop(253);
        near_cap.region.compile_strategy = CompileStrategy::Speed;
        assert_eq!(work(&near_cap), 8164);
        assert_eq!(plan_with_budget(near_cap, dedicated).unwrap().len(), 1);
        let mut over_cap = memory_loop(254);
        over_cap.region.compile_strategy = CompileStrategy::Speed;
        assert_eq!(
            work_and_allowance_with_budget(&over_cap, dedicated),
            (8196, 8192)
        );
        assert!(plan_with_budget(over_cap.clone(), dedicated).is_err());
        // A dedicated Speed cap must not become a larger Fast-job budget via
        // the shared service's legacy callee-preserving downgrade.
        over_cap.region.callees = vec![over_cap.region.start];
        assert!(plan_with_budget(over_cap, dedicated).is_err());
    }

    #[test]
    fn dedicated_fast_preserves_responsive_slicing_and_loop_allowance() {
        let original = memory_loop(64);
        let shared = background_budget(CompileStrategy::Fast, 16, false);
        let dedicated = background_budget(CompileStrategy::Fast, 16, true);
        assert_eq!(shared, dedicated);
        assert_eq!(
            work_and_allowance_with_budget(&original, dedicated),
            (529, 128)
        );
        let describe = |parts: Vec<Snapshot>| {
            parts
                .into_iter()
                .map(|part| {
                    (
                        part.region.start,
                        part.region.len,
                        part.region.entries.clone(),
                        part.region.frontier.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let shared_parts = plan_with_budget(original.clone(), shared).unwrap();
        let dedicated_parts = plan_with_budget(original, dedicated).unwrap();
        assert!(dedicated_parts.len() > 1);
        assert!(dedicated_parts.iter().all(|part| work(part) <= 16));
        assert_eq!(describe(shared_parts), describe(dedicated_parts));
    }

    #[test]
    fn crossing_branches_and_overlapping_decode_roots_keep_the_original_graph() {
        // Jump into the immediate of a linear MOV; then branch back across a
        // partition boundary. These are legitimate independent x86 decodes.
        let bytes = [
            0xb8, 0x90, 0x90, 0x90, 0x90, 0x8b, 0x00, 0x75, 0xf8, 0x8b, 0x00, 0xeb, 0xf8, 0xc3,
        ];
        let original = input(&bytes);
        let expected = decoded(&original);
        let fragments = plan_within(original, 16, 1, true).unwrap();
        let mut actual = BTreeMap::new();
        for fragment in fragments {
            assert!(work(&fragment) <= 16);
            actual.extend(decoded(&fragment));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_split_truncated_fetch_retains_the_inaccessible_tail_page_dependency() {
        let mut memory = Memory::new(0x10000, 8192).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        let mut bytes = [0x90; 16];
        bytes[15] = 0x0f;
        memory.write(0x10ff0, &bytes).unwrap();
        let mut region = Region::new(0x10ff0, 16);
        region.compile_strategy = CompileStrategy::Fast;
        region.complete_tail = true;
        let snapshot = Snapshot::capture(&region, &mut memory).unwrap();
        let fragments = plan(snapshot, 8).unwrap();
        let last = fragments.last().unwrap();
        let compiled = crate::compile::compile_snapshot(last).unwrap();
        assert!(compiled.pages.iter().any(|&(page, _)| page == 1));
        memory.protect(0x11000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x11000, &[0xa2]).unwrap();
        assert!(!last.valid(&memory));
        assert!(!compiled.valid(&memory));
    }
}
