//! Loop versioning for memory checks. An innermost counted loop whose
//! pointers advance by constant steps touches a range that is known when an
//! iteration ends. Checking that range once lets a clone of the loop access
//! those operands without per-access guards.
//!
//! A clone also forwards loop-invariant stack slots, such as `[ebp-8]`, through
//! SSA values. Stores still write memory, so every exit sees current memory;
//! loads read the value instead. That needs every write that could reach a
//! slot to update its value: the loop may contain no unproven write, and the
//! proof also checks that the other proven writes miss every slot.
//!
//! The permission table is read-only for one native invocation: every change
//! advances the memory epoch and a trap that observes a new epoch leaves native
//! code. A versioned loop contains no guest calls, and the memory helpers its
//! unproven accesses may call never change permissions, so a range proven at
//! its latch stays proven for every remaining iteration.
use super::*;
use iced_x86::{InstructionInfoFactory, OpAccess};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Plan {
    pub header: u32,
    pub latch: u32,
    pub counter: usize,
    /// Width in bytes of the counter decremented before the latch.
    pub counter_size: u32,
    /// Guest block starts inside the loop, each cloned once.
    pub starts: BTreeSet<u32>,
    pub groups: Vec<Group>,
    /// Instructions whose single memory operand is covered by a group, with
    /// the operand width in bytes.
    pub proven: BTreeMap<u32, u32>,
    /// Forwarded doubleword slots: invariant base register and displacement.
    pub slots: Vec<(usize, i64)>,
    /// Instructions whose memory operand is exactly a slot, and its index.
    pub promoted: BTreeMap<u32, usize>,
    /// Proven writes outside the slots, which the proof must keep off them.
    pub writes: Vec<Access>,
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

pub(super) fn plans(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    eligible: impl Fn(&Instruction) -> bool,
) -> Vec<Plan> {
    let mut factory = InstructionInfoFactory::new();
    let mut plans = Vec::new();
    for latch in instructions.values() {
        if latch.flow_control() != FlowControl::ConditionalBranch
            || latch.mnemonic() != M::Jne
            || latch.near_branch32() > latch.ip32()
            || !entries.contains(&latch.near_branch32())
        {
            continue;
        }
        if let Some(plan) = plan(instructions, entries, latch, &eligible, &mut factory)
            && !plans
                .iter()
                .any(|other: &Plan| other.contains(plan.header) || other.contains(plan.latch))
        {
            plans.push(plan);
        }
    }
    plans
}

fn plan(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    latch: &Instruction,
    eligible: &impl Fn(&Instruction) -> bool,
    factory: &mut InstructionInfoFactory,
) -> Option<Plan> {
    let header = latch.near_branch32();
    let body: Vec<&Instruction> = instructions
        .range(header..=latch.ip32())
        .map(|(_, i)| i)
        .collect();
    // Contiguous straight-line code with forward branches only.
    for pair in body.windows(2) {
        if pair[0].next_ip32() != pair[1].ip32() {
            return None;
        }
    }
    let end = latch.next_ip32();
    let mut skipped = Vec::new();
    for i in &body[..body.len() - 1] {
        if !eligible(i)
            || i.has_lock_prefix()
            || i.has_rep_prefix()
            || i.has_repne_prefix()
            || !matches!(i.segment_prefix(), R::None | R::DS | R::SS | R::ES)
        {
            return None;
        }
        match i.flow_control() {
            FlowControl::Next => {}
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch
                if i.op0_kind() == OpKind::NearBranch32 =>
            {
                let target = i.near_branch32();
                if target <= i.ip32() {
                    return None;
                }
                // Instructions after an exit branch run only when it is not
                // taken; an unconditional internal jump skips its gap.
                if target < end {
                    skipped.push((i.ip32(), target));
                } else if i.flow_control() == FlowControl::UnconditionalBranch {
                    skipped.push((i.ip32(), end));
                }
            }
            _ => return None,
        }
    }
    let always = |pc: u32| skipped.iter().all(|&(from, to)| !(from < pc && pc < to));

    // Classify every written register: an induction register changes only by
    // unconditional constant steps; anything else varies.
    let mut steps: [Vec<(u32, i64)>; 8] = Default::default();
    let mut varies = [false; 8];
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
            let delta = (used.register() == full && always(i.ip32()))
                .then(|| step(i, full))
                .flatten();
            match delta {
                Some(delta) => steps[index].push((i.ip32(), delta)),
                None => varies[index] = true,
            }
        }
    }

    // The counter: DEC/SUB 1 of a 32- or 16-bit register right before JNE.
    let before = body.get(body.len().checked_sub(2)?)?;
    let counter_register = before.op0_register();
    let counted = before.op_count() >= 1
        && before.op0_kind() == OpKind::Register
        && (before.mnemonic() == M::Dec
            || before.mnemonic() == M::Sub
                && before.op1_kind() != OpKind::Register
                && before.immediate(1) == 1);
    if !counted || !matches!(counter_register.size(), 2 | 4) {
        return None;
    }
    let counter = reg_index(counter_register.full_register32())?;
    let counter_size = counter_register.size() as u32;
    if counter_size == 2 {
        // Only the counting DEC/SUB may write the register; its narrow write
        // makes it unusable as an address component.
        if steps[counter].len() + usize::from(varies[counter]) != 1 {
            return None;
        }
        varies[counter] = true;
        steps[counter].clear();
    } else if varies[counter] || steps[counter].len() != 1 {
        return None;
    }

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
        if step.abs() > MAX_STEP || offset.abs() > MAX_STEP << 8 {
            continue;
        }
        let index = index.map(|index| (index, scale));
        proven.insert(pc, size as u32);
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
        return None;
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
            if overlapping.clone().all(|(_, other, write, forwards)| {
                other.low == access.low && other.high == access.high && (!write || *forwards)
            }) {
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
    Some(Plan {
        header,
        latch: latch.ip32(),
        counter,
        counter_size,
        starts,
        groups,
        proven,
        slots,
        promoted,
        writes,
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
        assert_eq!(plan.slots, [(5, 8), (5, -4)]);
        assert_eq!(plan.promoted.len(), 2);
        assert_eq!(plan.writes.len(), 1, "the [edi] store");
        // A narrow store overlapping [ebp+8] keeps that slot in memory.
        let (instructions, entries) =
            decode(|a| body(a, |a| a.mov(byte_ptr(ebp + 10), al).unwrap()));
        let plan = &super::plans(&instructions, &entries, |_| true)[0];
        assert_eq!(plan.slots, [(5, -4)]);
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
