//! Loop versioning for memory checks. An innermost counted loop whose
//! pointers advance by constant steps touches a range that is known when an
//! iteration ends. Checking that range once lets a clone of the loop access
//! those operands without per-access guards.
//!
//! A loop counts when its latch tests flags that compare an induction
//! register, the counter, with an invariant bound: a `dec ecx; jne`, an
//! `inc esi; cmp esi, edx; jb` or an `add edi, 4; cmp edi, ebx; jne`. The
//! counter's distance to the bound gives the iterations left. A clone stops
//! on its own once the counter has stepped as often as it proved and the
//! fuel paid for, so only that stop keeps it inside its proof; batches,
//! which skip the latch within a batch, also rely on the count being exact.
//! The latch conditions accepted are those that keep it exact: the counter
//! moves toward its bound and cannot wrap before reaching it.
//!
//! A clone also forwards stack slots that the loop writes, such as an
//! accumulator at `[ebp-8]`, through SSA values. Stores still write memory, so
//! every exit sees current memory; loads read the value instead. That needs
//! every write that could reach a slot to update its value: the loop may
//! contain no unproven write, and the proof also checks that the other proven
//! writes miss every slot. Slots the loop only reads stay in memory, as in the
//! original code: forwarding them would keep one host register (or spill slot)
//! per slot live across the loop for loads that cost no more.
//!
//! The permission table is read-only for one native invocation: every change
//! advances the memory epoch and a trap that observes a new epoch leaves native
//! code. A versioned loop contains no guest calls, and the memory helpers its
//! unproven accesses may call never change permissions, so a range proven at
//! its latch stays proven for every remaining iteration.
use super::*;
use iced_x86::{Code, CodeSize, CpuidFeature, InstructionInfoFactory, OpAccess};

/// Largest per-iteration pointer step considered; keeps range arithmetic in i64.
const MAX_STEP: i64 = 1 << 16;

/// Operands sharing registers, scale, step and access kind; one range check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub base: Option<usize>,
    pub index: Option<(usize, u32)>,
    pub step: i64,
    /// Lowest and highest operand byte offsets from `base + index * scale` in
    /// iteration zero, including the deltas applied earlier in the iteration.
    pub low: i64,
    pub high: i64,
    pub read: bool,
    pub write: bool,
}

/// The value a loop's counter moves toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bound {
    Constant(u32),
    /// An invariant general register.
    Register(usize),
    /// A doubleword whose immutable address is checked before loading the
    /// bound; the loop proof also keeps every write away from these bytes.
    Memory(Operand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub header: u32,
    pub latch: u32,
    pub counter: usize,
    /// Width in bytes of the counter: 2 only for a 16-bit DEC/SUB 1 whose
    /// flags a JNE latch tests.
    pub counter_size: u32,
    /// Address of the counter's one step, `steps[counter]` in size.
    pub counter_step: u32,
    /// The latch branches back while the counter after its step has not
    /// reached `bound`; when `inclusive`, also while it equals the bound.
    pub bound: Bound,
    pub inclusive: bool,
    /// Guest block starts inside the loop, each cloned once.
    pub starts: BTreeSet<u32>,
    pub groups: Vec<Group>,
    /// Instructions whose single memory operand is covered by a group.
    pub proven: BTreeMap<u32, Operand>,
    /// Forwarded doubleword slots: invariant base register and displacement.
    pub slots: Vec<(usize, i64)>,
    /// Instructions whose memory operand is exactly a slot, and its index.
    pub promoted: BTreeMap<u32, usize>,
    /// Proven writes outside the slots, which the proof must keep off them.
    pub writes: Vec<Access>,
    /// Unproven operands, as (pc, write), that a kernel checks against a
    /// validated window of its own instead of the page table, before the
    /// instruction's effects. Each instruction here starts a checked block,
    /// where the kernel leaves when its access is not ordinary memory.
    pub windows: Vec<(u32, bool)>,
    /// General registers the loop never writes, as a mask by index.
    pub invariant: u8,
    /// Registers changed only by the constant steps in `updates`, the
    /// counter included, and each one's total step per iteration.
    pub inductions: u8,
    pub steps: [i64; 8],
    pub updates: BTreeSet<u32>,
    /// Every proven operand as (pc, range, read, write).
    pub accesses: Vec<(u32, Access, bool, bool)>,
}

/// The form of a proven memory operand: `base + index * scale + displacement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Operand {
    /// Width in bytes.
    pub width: u32,
    pub base: Option<usize>,
    pub index: Option<(usize, u32)>,
    pub displacement: i32,
}

/// One proven memory operand: like a [`Group`] of a single instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Access {
    pub base: Option<usize>,
    pub index: Option<(usize, u32)>,
    pub step: i64,
    pub low: i64,
    pub high: i64,
}

impl Plan {
    pub fn contains(&self, pc: u32) -> bool {
        (self.header..=self.latch).contains(&pc)
    }
}

#[cfg(test)]
pub(super) fn plans(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    eligible: impl Fn(&Instruction) -> bool,
) -> Vec<Plan> {
    survey(instructions, entries, eligible).0
}

/// A backward branch that did not become a plan, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rejected {
    pub header: u32,
    pub latch: u32,
    pub reason: String,
}

/// The plans of `instructions`, and every other backward branch inside them
/// with the first reason its loop is not versioned.
pub(super) fn survey(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    eligible: impl Fn(&Instruction) -> bool,
) -> (Vec<Plan>, Vec<Rejected>) {
    let mut factory = InstructionInfoFactory::new();
    let mut plans = Vec::new();
    let mut rejected = Vec::new();
    for latch in instructions.values() {
        if !matches!(
            latch.flow_control(),
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch
        ) || latch.op0_kind() != OpKind::NearBranch32
            || latch.near_branch32() > latch.ip32()
            || !instructions.contains_key(&latch.near_branch32())
        {
            continue;
        }
        let header = latch.near_branch32();
        let result = if latch.flow_control() != FlowControl::ConditionalBranch {
            Err(format!("latch {}", name(latch)))
        } else if !entries.contains(&header) {
            Err("header".into())
        } else {
            plan(instructions, entries, latch, &eligible, &mut factory)
        };
        match result {
            Ok(plan)
                if plans.iter().any(|other: &Plan| {
                    other.contains(plan.header) || other.contains(plan.latch)
                }) =>
            {
                rejected.push(Rejected {
                    header,
                    latch: latch.ip32(),
                    reason: "overlap".into(),
                })
            }
            Ok(plan) => plans.push(plan),
            Err(reason) => rejected.push(Rejected {
                header,
                latch: latch.ip32(),
                reason,
            }),
        }
    }
    (plans, rejected)
}

/// An instruction's mnemonic in lower case, for diagnostics.
pub(super) fn name(i: &Instruction) -> String {
    format!("{:?}", i.mnemonic()).to_lowercase()
}

fn plan(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    latch: &Instruction,
    eligible: &impl Fn(&Instruction) -> bool,
    factory: &mut InstructionInfoFactory,
) -> Result<Plan, String> {
    let header = latch.near_branch32();
    let body: Vec<&Instruction> = instructions
        .range(header..=latch.ip32())
        .map(|(_, i)| i)
        .collect();
    // Contiguous straight-line code with forward branches only.
    for pair in body.windows(2) {
        if pair[0].next_ip32() != pair[1].ip32() {
            return Err("gap".into());
        }
    }
    let end = latch.next_ip32();
    let mut skipped = Vec::new();
    for i in &body[..body.len() - 1] {
        if !eligible(i) {
            return Err(format!("instruction {}", name(i)));
        }
        if i.has_lock_prefix() || i.has_rep_prefix() || i.has_repne_prefix() {
            return Err("prefix".into());
        }
        if !matches!(i.segment_prefix(), R::None | R::DS | R::SS | R::ES) {
            return Err("segment".into());
        }
        match i.flow_control() {
            FlowControl::Next => {}
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch
                if i.op0_kind() == OpKind::NearBranch32 =>
            {
                let target = i.near_branch32();
                if target <= i.ip32() {
                    return Err("inner loop".into());
                }
                // Instructions after an exit branch run only when it is not
                // taken; an unconditional internal jump skips its gap.
                if target < end {
                    skipped.push((i.ip32(), target));
                } else if i.flow_control() == FlowControl::UnconditionalBranch {
                    skipped.push((i.ip32(), end));
                }
            }
            FlowControl::Call | FlowControl::IndirectCall => return Err("call".into()),
            _ => return Err(format!("flow {}", name(i))),
        }
    }
    let always = |pc: u32| skipped.iter().all(|&(from, to)| !(from < pc && pc < to));

    // Classify every written register: an induction register changes only by
    // unconditional constant steps; anything else varies.
    let mut steps: [Vec<(u32, i64)>; 8] = Default::default();
    let mut varies = [false; 8];
    let mut write_sites: [BTreeSet<u32>; 8] = Default::default();
    for i in &body {
        for used in factory.info(i).used_registers() {
            if !matches!(
                used.access(),
                OpAccess::Write
                    | OpAccess::CondWrite
                    | OpAccess::ReadWrite
                    | OpAccess::ReadCondWrite
            ) {
                continue;
            }
            let full = used.register().full_register32();
            let Some(index) = reg_index(full) else {
                continue;
            };
            write_sites[index].insert(i.ip32());
            let delta = (used.register() == full && always(i.ip32()))
                .then(|| step(i, full))
                .flatten();
            match delta {
                Some(delta) => steps[index].push((i.ip32(), delta)),
                None => varies[index] = true,
            }
        }
    }

    // The counter: the register the last flag write before the latch
    // compares, either its own step or a CMP/TEST. LOOP is its own step of
    // ECX and compares the result with zero without flags.
    let looping = latch.code() == Code::Loop_rel8_32_ECX;
    let producer = if looping {
        latch
    } else {
        *body[..body.len() - 1]
            .iter()
            .rev()
            .find(|i| i.rflags_modified() != 0)
            .ok_or("counter")?
    };
    if !looping && (producer.op_count() == 0 || producer.op0_kind() != OpKind::Register) {
        let kind = if producer.op_count() != 0 && producer.op0_kind() == OpKind::Memory {
            " memory"
        } else {
            ""
        };
        return Err(format!("counter {}{kind}", name(producer)));
    }
    if !always(producer.ip32()) {
        return Err("counter path".into());
    }
    let counter_register = if looping {
        R::ECX
    } else {
        producer.op0_register()
    };
    let counter = reg_index(counter_register.full_register32())
        .ok_or_else(|| format!("counter {}", name(producer)))?;
    // How the flags relate the counter to the bound: the result of its own
    // step, CMP's difference, or TEST's value, which is CMP with zero.
    #[derive(Clone, Copy, PartialEq)]
    enum Compared {
        Step,
        Difference,
        Value,
    }
    let invariant_register = |register: R| {
        let index = reg_index(register).filter(|_| register.size() == 4)?;
        (index != counter && !varies[index] && steps[index].is_empty()).then_some(index)
    };
    let stepped = step(producer, counter_register).is_some();
    if counter_register.size() != 4 && !stepped {
        return Err("counter width".into());
    }
    let (compared, bound, bound_pc) = if stepped {
        (Compared::Step, Bound::Constant(0), None)
    } else {
        match (producer.mnemonic(), producer.op1_kind()) {
            (M::Cmp, OpKind::Immediate8to32 | OpKind::Immediate32) => (
                Compared::Difference,
                Bound::Constant(producer.immediate(1) as u32),
                None,
            ),
            (M::Cmp, OpKind::Register) => {
                let register = producer.op1_register();
                if let Some(index) = invariant_register(register) {
                    (Compared::Difference, Bound::Register(index), None)
                } else {
                    let index = reg_index(register)
                        .filter(|&index| register.size() == 4 && index != counter)
                        .ok_or("counter bound")?;
                    // The last write dominates the comparison. Earlier narrow
                    // or temporary values are irrelevant after this full load.
                    let load = body
                        .iter()
                        .rev()
                        .find(|i| {
                            i.ip32() < producer.ip32() && write_sites[index].contains(&i.ip32())
                        })
                        .copied()
                        .ok_or("counter bound")?;
                    if load.mnemonic() != M::Mov
                        || load.op0_kind() != OpKind::Register
                        || load.op0_register() != register
                        || load.op1_kind() != OpKind::Memory
                        || !always(load.ip32())
                    {
                        return Err("counter bound".into());
                    }
                    let operand = memory_bound(load, &varies, &steps, factory)?;
                    (
                        Compared::Difference,
                        Bound::Memory(operand),
                        Some(load.ip32()),
                    )
                }
            }
            (M::Cmp, OpKind::Memory) => {
                let operand = memory_bound(producer, &varies, &steps, factory)?;
                (
                    Compared::Difference,
                    Bound::Memory(operand),
                    Some(producer.ip32()),
                )
            }
            (M::Test, OpKind::Register) if producer.op1_register() == counter_register => {
                (Compared::Value, Bound::Constant(0), None)
            }
            (_, OpKind::Memory) => return Err(format!("counter {} memory", name(producer))),
            _ => return Err(format!("counter {}", name(producer))),
        }
    };
    let (counter_size, counter_step, up) = match counter_register.size() {
        2 => {
            // Only a decrement by one, whose own flags the latch tests, may
            // write the register; its narrow write makes it unusable as an
            // address component.
            let decrement = producer.mnemonic() == M::Dec
                || producer.mnemonic() == M::Sub && producer.immediate(1) == 1;
            if compared != Compared::Step || !decrement || latch.mnemonic() != M::Jne {
                return Err("counter width".into());
            }
            if write_sites[counter].len() != 1 || !write_sites[counter].contains(&producer.ip32()) {
                return Err("counter varies".into());
            }
            varies[counter] = true;
            steps[counter].clear();
            (2, producer.ip32(), false)
        }
        4 => {
            let [(pc, delta)] = steps[counter][..] else {
                return Err("counter varies".into());
            };
            if delta == 0 {
                return Err("counter step".into());
            }
            if varies[counter] {
                return Err("counter varies".into());
            }
            // The flags must see the stepped counter.
            if pc > producer.ip32() {
                return Err("counter order".into());
            }
            (4, pc, delta > 0)
        }
        _ => return Err("counter width".into()),
    };
    // Conditions under which the counter reaches the bound without wrapping
    // first, given that the latch just branched back. A step's own flags
    // compare its result with zero; their overflow flag is clear while the
    // counter moves toward zero from the side the condition requires.
    let inclusive = match (compared, latch.mnemonic(), up) {
        (_, M::Jne, _) | (Compared::Step, M::Loop, false) => false,
        (Compared::Step, M::Js | M::Jl, true) | (Compared::Step, M::Jg, false) => false,
        (Compared::Step, M::Jle, true) | (Compared::Step, M::Jns | M::Jge, false) => true,
        (Compared::Difference | Compared::Value, M::Jb | M::Jl, true)
        | (Compared::Difference | Compared::Value, M::Ja | M::Jg, false) => false,
        (Compared::Difference | Compared::Value, M::Jbe | M::Jle, true)
        | (Compared::Difference | Compared::Value, M::Jae | M::Jge, false) => true,
        (Compared::Value, M::Js, true) => false,
        (Compared::Value, M::Jns, false) => true,
        _ => return Err(format!("latch {}", name(latch))),
    };
    let total = |index: usize| steps[index].iter().map(|&(_, d)| d).sum::<i64>();
    let before_pc = |index: usize, pc: u32| {
        steps[index]
            .iter()
            .filter(|&&(at, _)| at < pc)
            .map(|&(_, d)| d)
            .sum::<i64>()
    };
    let mut groups: Vec<Group> = Vec::new();
    let mut proven = BTreeMap::new();
    let mut listed = Vec::new();
    // (pc, access, writes, writer forwards its store) for each proven operand.
    let mut accesses = Vec::new();
    for i in &body {
        let info = factory.info(i);
        let memory: Vec<_> = info.used_memory().to_vec();
        if memory.is_empty() || i.mnemonic() == M::Lea || i.mnemonic() == M::Nop {
            continue;
        }
        if memory.len() != 1 || (0..i.op_count()).all(|n| i.op_kind(n) != OpKind::Memory) {
            continue;
        }
        let used = &memory[0];
        let (read, write) = match used.access() {
            OpAccess::Read | OpAccess::CondRead => (true, false),
            OpAccess::Write | OpAccess::CondWrite => (false, true),
            OpAccess::ReadWrite | OpAccess::ReadCondWrite => (true, true),
            _ => continue,
        };
        let size = i.memory_size().size() as i64;
        if size == 0 || size > 16 {
            continue;
        }
        let component = |register: R| -> Option<Option<usize>> {
            if register == R::None {
                return Some(None);
            }
            let index = reg_index(register)?;
            (!varies[index]).then_some(Some(index))
        };
        let Some(base) = component(i.memory_base()) else {
            continue;
        };
        let Some(index) = component(i.memory_index()) else {
            continue;
        };
        let scale = i.memory_index_scale();
        let pc = i.ip32();
        let mut step = 0;
        let mut offset = i.memory_displacement32() as i32 as i64;
        if let Some(base) = base {
            step += total(base);
            offset += before_pc(base, pc);
        }
        if let Some(index) = index {
            step += total(index) * i64::from(scale);
            offset += before_pc(index, pc) * i64::from(scale);
        }
        let prechecked_bound = bound_pc == Some(pc);
        let large_offset = offset.abs() > MAX_STEP << 8;
        if step.abs() > MAX_STEP || large_offset && !prechecked_bound {
            continue;
        }
        let index = index.map(|index| (index, scale));
        listed.push((
            pc,
            Access {
                base,
                index,
                step,
                low: offset,
                high: offset + size,
            },
            read,
            write,
        ));
        proven.insert(
            pc,
            Operand {
                width: size as u32,
                base,
                index,
                displacement: i.memory_displacement32() as i32,
            },
        );
        accesses.push((
            pc,
            Access {
                base,
                index,
                step,
                low: offset,
                high: offset + size,
            },
            write,
            forwards_store(i),
        ));
        // A bound's exact wrapped address has its own check before the
        // speculative load. Large absolute displacements need no affine
        // group, whose unwrapped signed offset would otherwise reject them.
        if prechecked_bound && large_offset {
            continue;
        }
        if let Some(group) = groups.iter_mut().find(|g| {
            g.base == base
                && g.index == index
                && g.step == step
                && g.read == read
                && g.write == write
        }) {
            group.low = group.low.min(offset);
            group.high = group.high.max(offset + size);
        } else {
            groups.push(Group {
                base,
                index,
                step,
                low: offset,
                high: offset + size,
                read,
                write,
            });
        }
    }
    if proven.is_empty() {
        return Err("nothing proven".into());
    }
    let starts = entries.range(header..=latch.ip32()).copied().collect();
    // Any write the clone does not prove could reach a slot unseen.
    let unproven_write = body.iter().any(|i| {
        !proven.contains_key(&i.ip32())
            && factory.info(i).used_memory().iter().any(|m| {
                matches!(
                    m.access(),
                    OpAccess::Write
                        | OpAccess::CondWrite
                        | OpAccess::ReadWrite
                        | OpAccess::ReadCondWrite
                )
            })
    });
    if let Bound::Memory(operand) = bound {
        if unproven_write {
            return Err("counter bound write".into());
        }
        if bound_pc.and_then(|pc| proven.get(&pc)) != Some(&operand) {
            return Err("counter bound proof".into());
        }
    }
    let mut slots = Vec::new();
    let mut promoted = BTreeMap::new();
    if !unproven_write {
        for (_, access, _, _) in &accesses {
            let Some(base) = access.base else { continue };
            let slot = (base, access.low);
            if access.index.is_some()
                || !steps[base].is_empty()
                || access.high - access.low != 4
                || slots.contains(&slot)
            {
                continue;
            }
            // Every operand overlapping the slot through the same base must be
            // exactly it, and every write to it must forward its value.
            let overlapping = accesses.iter().filter(|(_, other, _, _)| {
                other.base == Some(base)
                    && other.index.is_none()
                    && other.low < access.high
                    && access.low < other.high
            });
            if overlapping.clone().any(|(_, _, write, _)| *write)
                && overlapping.clone().all(|(_, other, write, forwards)| {
                    other.low == access.low && other.high == access.high && (!write || *forwards)
                })
            {
                for (pc, ..) in overlapping {
                    promoted.insert(*pc, slots.len());
                }
                slots.push(slot);
            }
        }
    }
    let writes = accesses
        .into_iter()
        .filter(|(pc, _, write, _)| *write && !promoted.contains_key(pc))
        .map(|(_, access, ..)| access)
        .collect();
    let mut windows = Vec::new();
    for i in &body {
        if proven.contains_key(&i.ip32()) || matches!(i.mnemonic(), M::Lea | M::Nop) {
            continue;
        }
        // The check precedes the instruction: its operand must be the
        // entry registers' address, and its bytes at most two pages.
        if matches!(
            i.mnemonic(),
            M::Pop | M::Push | M::Bt | M::Bts | M::Btr | M::Btc
        ) || i.flow_control() != FlowControl::Next
            || !(1..=64).contains(&i.memory_size().size())
            || i.cpuid_features().contains(&CpuidFeature::FPU)
        {
            continue;
        }
        let info = factory.info(i);
        let [used] = info.used_memory() else {
            continue;
        };
        if (0..i.op_count()).all(|n| i.op_kind(n) != OpKind::Memory) {
            continue;
        }
        let (read, write) = match used.access() {
            OpAccess::Read | OpAccess::CondRead => (true, false),
            OpAccess::Write | OpAccess::CondWrite => (false, true),
            OpAccess::ReadWrite | OpAccess::ReadCondWrite => (true, true),
            _ => continue,
        };
        if read {
            windows.push((i.ip32(), false));
        }
        if write {
            windows.push((i.ip32(), true));
        }
    }
    let invariant = (0..8)
        .filter(|&index| !varies[index] && steps[index].is_empty())
        .fold(0, |mask, index| mask | 1 << index);
    // A 16-bit counter varies as an address component, yet still steps by
    // one through its decrement.
    let inductions = (0..8)
        .filter(|&index| !varies[index] && !steps[index].is_empty())
        .fold(1 << counter, |mask, index| mask | 1 << index);
    let mut totals = [0; 8];
    // The comparison closes the iteration with the steps: a batch runs it
    // once, with its last lane's counter.
    let mut updates = BTreeSet::from([producer.ip32(), counter_step]);
    for index in 0..8 {
        if inductions & 1 << index != 0 {
            totals[index] = total(index);
            updates.extend(steps[index].iter().map(|&(pc, _)| pc));
        }
    }
    if counter_size == 2 {
        totals[counter] = -1;
    }
    Ok(Plan {
        header,
        latch: latch.ip32(),
        counter,
        counter_size,
        counter_step,
        bound,
        inclusive,
        starts,
        groups,
        proven,
        slots,
        promoted,
        writes,
        windows,
        invariant,
        inductions,
        steps: totals,
        updates,
        accesses: listed,
    })
}

/// A bound load through a full-width, immutable address. Its bytes are proved
/// ordinary and disjoint from every loop write before the kernel uses it.
fn memory_bound(
    i: &Instruction,
    varies: &[bool; 8],
    steps: &[Vec<(u32, i64)>; 8],
    factory: &mut InstructionInfoFactory,
) -> Result<Operand, String> {
    let [used] = factory.info(i).used_memory() else {
        return Err("counter bound memory".into());
    };
    if i.memory_size().size() != 4 || used.address_size() != CodeSize::Code32 {
        return Err("counter bound address".into());
    }
    let component = |register: R| -> Result<Option<usize>, String> {
        if register == R::None {
            return Ok(None);
        }
        let index = reg_index(register)
            .filter(|&index| register.size() == 4 && !varies[index] && steps[index].is_empty())
            .ok_or("counter bound")?;
        Ok(Some(index))
    };
    Ok(Operand {
        width: 4,
        base: component(i.memory_base())?,
        index: component(i.memory_index())?.map(|index| (index, i.memory_index_scale())),
        displacement: i.memory_displacement32() as i32,
    })
}

/// Integer forms whose doubleword memory destination the emitter stores
/// through `store_memory` or `direct_store`, where a clone forwards it.
fn forwards_store(i: &Instruction) -> bool {
    i.op0_kind() == OpKind::Memory
        && i.memory_size().size() == 4
        && matches!(
            i.mnemonic(),
            M::Mov
                | M::Add
                | M::Sub
                | M::And
                | M::Or
                | M::Xor
                | M::Inc
                | M::Dec
                | M::Not
                | M::Neg
                | M::Adc
                | M::Sbb
        )
}

/// A full-register constant step, or None for any other write.
fn step(i: &Instruction, register: R) -> Option<i64> {
    let immediate = || match i.op1_kind() {
        OpKind::Immediate8to32 | OpKind::Immediate32 | OpKind::Immediate8 => {
            Some(i.immediate(1) as u32 as i32 as i64)
        }
        _ => None,
    };
    if i.code() == Code::Loop_rel8_32_ECX {
        return (register == R::ECX).then_some(-1);
    }
    if i.op_count() == 0 || i.op0_kind() != OpKind::Register || i.op0_register() != register {
        return None;
    }
    match i.mnemonic() {
        M::Inc => Some(1),
        M::Dec => Some(-1),
        M::Add => immediate(),
        M::Sub => immediate().map(|d| -d),
        M::Lea
            if i.memory_base() == register
                && i.memory_index() == R::None
                && i.segment_prefix() == R::None =>
        {
            Some(i.memory_displacement32() as i32 as i64)
        }
        _ => None,
    }
}

/// Whether `[start, end)` of window offsets is ordinary memory permitting the
/// requested access: mapped, not shared, and for writes not executable.
pub(crate) extern "C" fn range_ok(
    perms: *const u8,
    start: i64,
    end: i64,
    len: i64,
    required: u32,
    forbidden: u32,
) -> u32 {
    if start < 0 || end > len || start >= end {
        return 0;
    }
    let first = start as usize / crate::memory::PAGE_SIZE;
    let last = (end - 1) as usize / crate::memory::PAGE_SIZE;
    for page in first..=last {
        // SAFETY: `end <= len` keeps every page inside the table, which lives
        // for the native invocation that calls this.
        let p = u32::from(unsafe { *perms.add(page) });
        if p & required != required || p & forbidden != 0 {
            return 0;
        }
    }
    1
}

/// Pages a window may extend over on each side of the access that fills it.
pub(super) const WINDOW_PAGES: i64 = 256;

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    fn decode(
        build: impl FnOnce(&mut CodeAssembler),
    ) -> (BTreeMap<u32, Instruction>, BTreeSet<u32>) {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let bytes = a.assemble(0x1000).unwrap();
        let mut decoder = Decoder::with_ip(32, &bytes, 0x1000, DecoderOptions::NONE);
        let instructions: BTreeMap<_, _> = decoder.iter().map(|i| (i.ip32(), i)).collect();
        let mut entries = BTreeSet::from([0x1000]);
        for i in instructions.values() {
            if i.flow_control() != FlowControl::Next {
                entries.insert(i.next_ip32());
            }
            if i.op0_kind() == OpKind::NearBranch32 {
                entries.insert(i.near_branch32());
            }
        }
        entries.retain(|pc| instructions.contains_key(pc));
        (instructions, entries)
    }

    #[test]
    fn carry_controls_do_not_supply_a_counter_comparison() {
        for carry in [
            (|a: &mut CodeAssembler| a.clc().unwrap()) as fn(&mut CodeAssembler),
            |a| a.stc().unwrap(),
            |a| a.cmc().unwrap(),
        ] {
            for after_counter in [false, true] {
                let (instructions, entries) = decode(|a| {
                    let mut top = a.create_label();
                    a.set_label(&mut top).unwrap();
                    a.mov(eax, dword_ptr(esi)).unwrap();
                    a.add(esi, 4).unwrap();
                    if !after_counter {
                        carry(a);
                    }
                    a.dec(ecx).unwrap();
                    if after_counter {
                        carry(a);
                    }
                    a.jne(top).unwrap();
                    a.ret().unwrap();
                });
                // A later partial flag writer conservatively rejects the
                // proof, even though this particular JNE does not read CF.
                let plans = super::plans(&instructions, &entries, super::super::supported);
                assert_eq!(plans.len(), usize::from(!after_counter));
            }
        }
    }

    #[test]
    fn loop_latch_counts_ecx_down_to_zero() {
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.add(esi, 8).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();
            a.add(edi, 8).unwrap();
            a.loop_(top).unwrap();
            a.ret().unwrap();
        });
        let plans = super::plans(&instructions, &entries, |_| true);
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!((plan.counter, plan.counter_size), (1, 4));
        assert_eq!(plan.counter_step, plan.latch);
        assert_eq!(plan.bound, Bound::Constant(0));
        assert!(!plan.inclusive);
        assert_eq!(plan.steps[1], -1);
        assert_eq!(plan.proven.len(), 2);
        // Another write of ECX leaves the count unknown.
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(esi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.loop_(top).unwrap();
            a.ret().unwrap();
        });
        assert!(super::plans(&instructions, &entries, |_| true).is_empty());
    }

    #[test]
    fn copy_loop_groups_source_and_destination() {
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(dword_ptr(edi + 8), mm0).unwrap();
            a.add(edi, 4).unwrap();
            a.add(esi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
            a.ret().unwrap();
        });
        let plans = super::plans(&instructions, &entries, |_| true);
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        assert_eq!((plan.counter, plan.counter_size), (1, 4));
        assert_eq!(plan.proven.len(), 2);
        assert!(plan.groups.contains(&Group {
            base: Some(6),
            index: None,
            step: 4,
            low: 0,
            high: 4,
            read: true,
            write: false,
        }));
        assert!(plan.groups.contains(&Group {
            base: Some(7),
            index: None,
            step: 4,
            low: 8,
            high: 12,
            read: false,
            write: true,
        }));
    }

    #[test]
    fn conditional_steps_varying_pointers_and_inner_loops_are_not_proven() {
        // A pointer stepped only on one path varies.
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            let mut skip = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.je(skip).unwrap();
            a.add(esi, 4).unwrap();
            a.set_label(&mut skip).unwrap();
            a.mov(dword_ptr(ebx + 4), eax).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
            a.ret().unwrap();
        });
        let plans = super::plans(&instructions, &entries, |_| true);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].proven.len(), 1, "only the invariant [ebx+4]");
        // Backward branches inside the body reject the outer loop; the inner one stays.
        let (instructions, entries) = decode(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(edx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(edx).unwrap();
            a.jne(inner).unwrap();
            a.dec(ecx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        let plans = super::plans(&instructions, &entries, |_| true);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].counter, 2);
    }

    #[test]
    fn narrow_counter_proof_rejects_other_writes_and_a_skipped_decrement() {
        for overwrite in [
            (|a: &mut CodeAssembler| a.mov(cx, 4).unwrap()) as fn(&mut CodeAssembler),
            |a| a.mov(cl, 4).unwrap(),
            |a| a.mov(ch, 4).unwrap(),
            |a| a.mov(ecx, 4).unwrap(),
            |a| a.dec(cx).unwrap(),
        ] {
            let (instructions, entries) = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.add(esi, 4).unwrap();
                overwrite(a);
                a.dec(cx).unwrap();
                a.jne(top).unwrap();
                a.ret().unwrap();
            });
            assert!(super::plans(&instructions, &entries, |_| true).is_empty());
        }
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            let mut latch = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(esi, 4).unwrap();
            a.cmp(ebx, 0).unwrap();
            a.jae(latch).unwrap();
            a.dec(cx).unwrap();
            a.set_label(&mut latch).unwrap();
            a.jne(top).unwrap();
            a.ret().unwrap();
        });
        assert!(super::plans(&instructions, &entries, |_| true).is_empty());
    }

    #[test]
    fn survey_names_the_first_reason_each_loop_is_not_versioned() {
        let (instructions, entries) = decode(|a| {
            let mut compare = a.create_label();
            let mut memory = a.create_label();
            let mut divide = a.create_label();
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut compare).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.cmp(edi, esi).unwrap();
            a.ja(compare).unwrap();
            a.set_label(&mut memory).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(dword_ptr(ebp - 4)).unwrap();
            a.jne(memory).unwrap();
            a.set_label(&mut divide).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.div(ebx).unwrap();
            a.dec(ecx).unwrap();
            a.jne(divide).unwrap();
            a.set_label(&mut outer).unwrap();
            a.mov(edx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(edx).unwrap();
            a.jne(inner).unwrap();
            a.dec(ecx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        let (plans, rejected) = survey(&instructions, &entries, |i| i.mnemonic() != M::Div);
        assert_eq!(plans.len(), 1, "the inner loop");
        let reasons: Vec<&str> = rejected.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "latch ja",
                "counter dec memory",
                "instruction div",
                "inner loop"
            ]
        );
    }

    #[test]
    fn counters_compare_with_invariant_bounds_toward_which_they_step() {
        type Close = fn(&mut CodeAssembler, CodeLabel);
        // Each loop copies through [esi]; the closing code steps and tests.
        let survey_one = |close: Close| {
            let (instructions, entries) = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(esi + ebx * 4)).unwrap();
                a.mov(dword_ptr(edi + ebx * 4), eax).unwrap();
                close(a, top);
                a.ret().unwrap();
            });
            let (plans, rejected) = survey(&instructions, &entries, |_| true);
            match (plans.as_slice(), rejected.as_slice()) {
                ([plan], []) => Ok((
                    plan.counter,
                    plan.steps[plan.counter],
                    plan.bound,
                    plan.inclusive,
                )),
                ([], [loop_]) => Err(loop_.reason.clone()),
                other => panic!("{other:?}"),
            }
        };
        let cases: [(Close, Result<(usize, i64, Bound, bool), &str>); 15] = [
            (
                |a, top| {
                    a.inc(ebx).unwrap();
                    a.cmp(ebx, edx).unwrap();
                    a.jb(top).unwrap();
                },
                Ok((3, 1, Bound::Register(2), false)),
            ),
            (
                |a, top| {
                    a.add(ebx, 2).unwrap();
                    a.cmp(ebx, 1000).unwrap();
                    a.jle(top).unwrap();
                },
                Ok((3, 2, Bound::Constant(1000), true)),
            ),
            (
                |a, top| {
                    a.sub(ebx, 1).unwrap();
                    a.jns(top).unwrap();
                },
                Ok((3, -1, Bound::Constant(0), true)),
            ),
            (
                |a, top| {
                    a.dec(ebx).unwrap();
                    a.lea(esi, dword_ptr(esi + 4)).unwrap();
                    a.test(ebx, ebx).unwrap();
                    a.jg(top).unwrap();
                },
                Ok((3, -1, Bound::Constant(0), false)),
            ),
            (
                |a, top| {
                    a.dec(ecx).unwrap();
                    a.mov(dword_ptr(edi), eax).unwrap();
                    a.jne(top).unwrap();
                },
                Ok((1, -1, Bound::Constant(0), false)),
            ),
            (
                |a, top| {
                    a.dec(ebx).unwrap();
                    a.cmp(ebx, ebp).unwrap();
                    a.jae(top).unwrap();
                },
                Ok((3, -1, Bound::Register(5), true)),
            ),
            // Moving away from the bound wraps before reaching it.
            (
                |a, top| {
                    a.inc(ebx).unwrap();
                    a.cmp(ebx, edx).unwrap();
                    a.ja(top).unwrap();
                },
                Err("latch ja"),
            ),
            // A step's carry does not compare its result.
            (
                |a, top| {
                    a.add(ebx, 1).unwrap();
                    a.jb(top).unwrap();
                },
                Err("latch jb"),
            ),
            (
                |a, top| {
                    a.cmp(ebx, edx).unwrap();
                    a.lea(ebx, dword_ptr(ebx + 1)).unwrap();
                    a.jb(top).unwrap();
                },
                Err("counter order"),
            ),
            (
                |a, top| {
                    a.inc(ebx).unwrap();
                    a.add(esi, 4).unwrap();
                    a.cmp(ebx, esi).unwrap();
                    a.jb(top).unwrap();
                },
                Err("counter bound"),
            ),
            (
                |a, top| {
                    a.inc(ebx).unwrap();
                    a.cmp(ebx, dword_ptr(ebp - 4)).unwrap();
                    a.jb(top).unwrap();
                },
                Ok((
                    3,
                    1,
                    Bound::Memory(Operand {
                        width: 4,
                        base: Some(5),
                        index: None,
                        displacement: -4,
                    }),
                    false,
                )),
            ),
            (
                |a, top| {
                    a.inc(bx).unwrap();
                    a.cmp(bx, 100).unwrap();
                    a.jb(top).unwrap();
                },
                Err("counter width"),
            ),
            // A fixed counter cannot fund any iterations or range check.
            (
                |a, top| {
                    a.add(ebx, 0).unwrap();
                    a.jne(top).unwrap();
                },
                Err("counter step"),
            ),
            (
                |a, top| {
                    a.lea(ebx, dword_ptr(ebx)).unwrap();
                    a.cmp(ebx, 100).unwrap();
                    a.jb(top).unwrap();
                },
                Err("counter step"),
            ),
            // A later partial flag write makes finding the earlier counter
            // conservative; it must not borrow flags from the wrong result.
            (
                |a, top| {
                    a.dec(ebx).unwrap();
                    a.ror(edx, 1).unwrap();
                    a.jne(top).unwrap();
                },
                Err("counter ror"),
            ),
        ];
        for (n, (close, expected)) in cases.into_iter().enumerate() {
            let expected = expected.map_err(str::to_owned);
            assert_eq!(survey_one(close), expected, "case {n}");
        }
    }

    #[test]
    fn memory_bounds_require_a_dominating_load_and_immutable_address() {
        let build = |extra: fn(&mut CodeAssembler), address: AsmMemoryOperand| {
            decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                extra(a);
                a.mov(edx, address).unwrap();
                a.inc(ebx).unwrap();
                a.cmp(ebx, edx).unwrap();
                a.jl(top).unwrap();
                a.ret().unwrap();
            })
        };
        // An earlier partial write cannot affect the full invariant load
        // that supplies the comparison, even though EDX varies in the body.
        let (instructions, entries) = build(
            |a| a.mov(dx, word_ptr(edi)).unwrap(),
            dword_ptr(ebp + ecx * 4 - 8),
        );
        let plan = &super::plans(&instructions, &entries, |_| true)[0];
        assert_eq!(
            plan.bound,
            Bound::Memory(Operand {
                width: 4,
                base: Some(5),
                index: Some((1, 4)),
                displacement: -8,
            })
        );
        for (extra, address, reason) in [
            (
                (|a: &mut CodeAssembler| a.add(ebp, 4).unwrap()) as fn(&mut CodeAssembler),
                dword_ptr(ebp),
                "counter bound",
            ),
            (
                |a| a.inc(ecx).unwrap(),
                dword_ptr(ebp + ecx * 4),
                "counter bound",
            ),
            (
                |a| a.mov(dword_ptr(edi + eax * 4), 9).unwrap(),
                dword_ptr(ebp),
                "counter bound write",
            ),
            (|_| {}, dword_ptr(bx + si), "counter bound address"),
        ] {
            let (instructions, entries) = build(extra, address);
            let (plans, rejected) = survey(&instructions, &entries, |_| true);
            assert!(plans.is_empty());
            assert_eq!(rejected[0].reason, reason);
        }
        // The exact bound check covers an absolute address outside the
        // affine groups' displacement limit, including the high half.
        for address in [0x1234_0000u32, 0xf234_0000] {
            let (instructions, entries) = build(|_| {}, dword_ptr(address as i32));
            let plan = &super::plans(&instructions, &entries, |_| true)[0];
            assert_eq!(
                plan.bound,
                Bound::Memory(Operand {
                    width: 4,
                    base: None,
                    index: None,
                    displacement: address as i32,
                })
            );
            assert_eq!(plan.proven.len(), 2);
            assert_eq!(plan.groups.len(), 1, "the source read only");
        }
        let (instructions, entries) = decode(|a| {
            let mut top = a.create_label();
            let mut skip = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.je(skip).unwrap();
            a.mov(edx, dword_ptr(ebp)).unwrap();
            a.set_label(&mut skip).unwrap();
            a.inc(ebx).unwrap();
            a.cmp(ebx, edx).unwrap();
            a.jl(top).unwrap();
            a.ret().unwrap();
        });
        assert!(super::plans(&instructions, &entries, |_| true).is_empty());
    }

    #[test]
    fn invariant_doubleword_slots_are_forwarded_unless_written_unseen() {
        let body = |a: &mut CodeAssembler, extra: fn(&mut CodeAssembler)| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp + 8)).unwrap();
            a.add(dword_ptr(ebp - 4), eax).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            extra(a);
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
            a.ret().unwrap();
        };
        let (instructions, entries) = decode(|a| body(a, |_| {}));
        let plan = &super::plans(&instructions, &entries, |_| true)[0];
        // [ebp+8] is only read and stays in memory.
        assert_eq!(plan.slots, [(5, -4)]);
        assert_eq!(plan.promoted.len(), 1);
        assert_eq!(plan.writes.len(), 1, "the [edi] store");
        assert_eq!(
            plan.proven.values().next(),
            Some(&Operand {
                width: 4,
                base: Some(5),
                index: None,
                displacement: 8,
            })
        );
        // A narrow store overlapping [ebp-4] keeps that slot in memory.
        let (instructions, entries) =
            decode(|a| body(a, |a| a.mov(byte_ptr(ebp - 2), al).unwrap()));
        let plan = &super::plans(&instructions, &entries, |_| true)[0];
        assert!(plan.slots.is_empty());
        // An unproven indexed store could reach any slot.
        let (instructions, entries) =
            decode(|a| body(a, |a| a.mov(dword_ptr(esi + eax * 4), eax).unwrap()));
        let plan = &super::plans(&instructions, &entries, |_| true)[0];
        assert!(plan.slots.is_empty());
    }

    #[test]
    fn range_check_requires_ordinary_permissions_in_the_window() {
        let perms = [READ | WRITE, READ | WRITE | EXEC, READ, 0];
        let len = perms.len() as i64 * 4096;
        let (read, write) = (u32::from(READ), u32::from(WRITE));
        let forbid_write = u32::from(crate::memory::SHARED | EXEC);
        let shared = u32::from(crate::memory::SHARED);
        assert_eq!(
            range_ok(perms.as_ptr(), 0, 4096, len, write, forbid_write),
            1
        );
        assert_eq!(
            range_ok(perms.as_ptr(), 4000, 4100, len, write, forbid_write),
            0
        );
        assert_eq!(range_ok(perms.as_ptr(), 0, 3 * 4096, len, read, shared), 1);
        assert_eq!(
            range_ok(perms.as_ptr(), 0, 3 * 4096 + 1, len, read, shared),
            0
        );
        assert_eq!(range_ok(perms.as_ptr(), -4, 4, len, read, shared), 0);
        assert_eq!(range_ok(perms.as_ptr(), 0, len + 1, len, read, shared), 0);
    }
}
