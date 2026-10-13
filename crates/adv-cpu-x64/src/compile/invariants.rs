//! Permission proofs for fixed addresses in regions with an internal loop.
//!
//! This analysis does not prove data immutable, promote stack slots, or require
//! writes through other addresses to be disjoint. A group describes the guest
//! address as `u32(base + index * scale) + signed displacement`. The proof
//! rejects displacement wrapping, underflow, and nonordinary permissions
//! by selecting the original checked body, never by raising an early fault.
//! The caller separately excludes stateful helpers, parallel execution, fault
//! lanes, and existing counted-loop/nested kernels before emitting a clone.
use super::reg_index;
use iced_x86::{
    CodeSize, FlowControl, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind,
    Register,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SPAN: i64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Group {
    pub base: Option<usize>,
    pub index: Option<(usize, u32)>,
    /// Signed guest displacements; high is exclusive and includes operand width.
    pub low: i64,
    pub high: i64,
    pub read: bool,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Site {
    pub group: usize,
    pub displacement: i32,
    pub width: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub header: u32,
    pub latch: u32,
    /// Exclusive end of the selected, contiguous loop body.
    pub end: u32,
    /// Instruction boundaries which can enter a block inside the scope.
    pub entries: BTreeSet<u32>,
    pub groups: Vec<Group>,
    pub sites: BTreeMap<u32, Site>,
}

/// Select single explicit memory operands whose address registers remain
/// unchanged inside one loop, including partial/implicit writes. Setup, teardown,
/// callees and other loops outside that scope do not constrain its addresses.
/// The caller must prove the current addresses at the selected backedge before
/// entering the clone, and leave the clone when control leaves this scope.
pub(super) fn analyze(instructions: &BTreeMap<u32, Instruction>) -> Option<Plan> {
    let mut best = None;
    for latch in instructions.values() {
        if !matches!(
            latch.flow_control(),
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch
        ) || latch.op0_kind() != OpKind::NearBranch32
            || latch.near_branch32() > latch.ip32()
        {
            continue;
        }
        let Some((plan, savings)) = analyze_loop(instructions, latch) else {
            continue;
        };
        // Count removed read/write checks, not just instructions: an RMW site
        // has two checks. Prefer fewer proof groups when savings are equal.
        let score = (savings, std::cmp::Reverse(plan.groups.len()));
        if best.as_ref().is_none_or(|(_, previous)| score > *previous) {
            best = Some((plan, score));
        }
    }
    best.map(|(plan, _)| plan)
}

fn analyze_loop(
    instructions: &BTreeMap<u32, Instruction>,
    latch: &Instruction,
) -> Option<(Plan, usize)> {
    let header = latch.near_branch32();
    let end = latch.next_ip32();
    if end <= latch.ip32() {
        return None;
    }
    let body: Vec<_> = instructions.range(header..=latch.ip32()).collect();
    if body.first().map(|entry| *entry.0) != Some(header)
        || body
            .windows(2)
            .any(|pair| pair[0].1.next_ip32() != *pair[1].0)
        || body.iter().any(|entry| {
            let pc = *entry.0;
            let i = entry.1;
            i.ip32() != pc
                || i.is_invalid()
                // Start with one straight-line body and its direct backedge.
                // Unknown callees, indirect control and nested/side branches
                // cannot be covered by this bounded proof.
                || (pc != latch.ip32() && i.flow_control() != FlowControl::Next)
        })
    {
        return None;
    }

    let mut factory = InstructionInfoFactory::new();
    let mut clobbered = 0u8;
    for (_, i) in &body {
        for used in factory.info(i).used_registers() {
            if matches!(
                used.access(),
                OpAccess::Write
                    | OpAccess::CondWrite
                    | OpAccess::ReadWrite
                    | OpAccess::ReadCondWrite
            ) && let Some(index) = reg_index(used.register().full_register32())
            {
                clobbered |= 1 << index;
            }
        }
    }

    type Key = (Option<usize>, Option<(usize, u32)>);
    type Pending = (Group, Vec<(u32, i32, u32, usize)>);
    let mut pending: BTreeMap<Key, Pending> = BTreeMap::new();
    for (_, i) in &body {
        let pc = i.ip32();
        if matches!(i.mnemonic(), Mnemonic::Lea | Mnemonic::Nop)
            || i.has_lock_prefix()
            || i.has_rep_prefix()
            || i.has_repne_prefix()
            || !(0..i.op_count()).any(|op| i.op_kind(op) == OpKind::Memory)
        {
            continue;
        }
        let [used] = factory.info(i).used_memory() else {
            continue;
        };
        if used.address_size() != CodeSize::Code32
            || used.vsib_size() != 0
            || !matches!(
                used.segment(),
                Register::None | Register::DS | Register::SS | Register::ES
            )
        {
            continue;
        }
        let width = used.memory_size().size();
        if !(1..=16).contains(&width) {
            continue;
        }
        let (read, write) = match used.access() {
            OpAccess::Read | OpAccess::CondRead => (true, false),
            OpAccess::Write | OpAccess::CondWrite => (false, true),
            OpAccess::ReadWrite | OpAccess::ReadCondWrite => (true, true),
            _ => continue,
        };
        let component = |register: Register| -> Option<Option<usize>> {
            if register == Register::None {
                return Some(None);
            }
            let index = reg_index(register)?;
            (register.size() == 4 && clobbered & (1 << index) == 0).then_some(Some(index))
        };
        let Some(base) = component(used.base()) else {
            continue;
        };
        let Some(index) = component(used.index()) else {
            continue;
        };
        // Keep absolute addresses on their existing constant emission path.
        if base.is_none() && index.is_none() {
            continue;
        }
        let index = index.map(|index| (index, used.scale()));
        let displacement = used.displacement() as u32 as i32;
        let low = i64::from(displacement);
        let high = low + width as i64;
        let entry = pending.entry((base, index)).or_insert_with(|| {
            (
                Group {
                    base,
                    index,
                    low,
                    high,
                    read: false,
                    write: false,
                },
                Vec::new(),
            )
        });
        entry.0.low = entry.0.low.min(low);
        entry.0.high = entry.0.high.max(high);
        entry.0.read |= read;
        entry.0.write |= write;
        entry.1.push((
            pc,
            displacement,
            width as u32,
            usize::from(read) + usize::from(write),
        ));
    }

    let mut plan = Plan {
        header,
        latch: latch.ip32(),
        end,
        entries: BTreeSet::from([header]),
        groups: Vec::new(),
        sites: BTreeMap::new(),
    };
    // External side entries are safe: they execute the checked body until the
    // selected latch proves their current addresses. Preserve their boundaries
    // for a clone emitter using the original region's block structure.
    for i in instructions.values() {
        if i.op_count() != 0 && i.op0_kind() == OpKind::NearBranch32 {
            let target = i.near_branch32();
            if header <= target && target < end {
                if !instructions.contains_key(&target) {
                    return None;
                }
                plan.entries.insert(target);
            }
        }
        if i.flow_control() != FlowControl::Next {
            let next = i.next_ip32();
            if header <= next && next < end && instructions.contains_key(&next) {
                plan.entries.insert(next);
            }
        }
    }
    let mut savings = 0;
    for (group, sites) in pending.into_values() {
        if group.high - group.low > MAX_SPAN {
            continue;
        }
        let index = plan.groups.len();
        plan.groups.push(group);
        for (pc, displacement, width, checks) in sites {
            savings += checks;
            plan.sites.insert(
                pc,
                Site {
                    group: index,
                    displacement,
                    width,
                },
            );
        }
    }
    (plan.sites.len() >= 2).then_some((plan, savings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;
    use iced_x86::{Decoder, DecoderOptions};

    fn decode(build: impl FnOnce(&mut CodeAssembler)) -> BTreeMap<u32, Instruction> {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let bytes = a.assemble(0x1000).unwrap();
        Decoder::with_ip(32, &bytes, 0x1000, DecoderOptions::NONE)
            .iter()
            .map(|i| (i.ip32(), i))
            .collect()
    }

    #[test]
    fn readonly_base_merges_signed_ranges_and_keeps_site_widths() {
        let code = decode(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp - 8)).unwrap();
            a.mov(dx, word_ptr(ebp + 4)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.ret().unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(
            plan.groups,
            [Group {
                base: Some(5),
                index: None,
                low: -8,
                high: 6,
                read: true,
                write: false
            }]
        );
        assert_eq!(
            plan.sites
                .values()
                .map(|s| (s.displacement, s.width))
                .collect::<Vec<_>>(),
            [(-8, 4), (4, 2)]
        );
    }

    #[test]
    fn partial_and_implicit_clobbers_inside_the_scope_invalidate_addresses() {
        for clobber in [0, 1] {
            let code = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                if clobber == 0 {
                    a.mov(eax, dword_ptr(ebp)).unwrap();
                    a.mov(eax, dword_ptr(ebp + 4)).unwrap();
                    a.mov(bp, ax).unwrap();
                } else {
                    a.mov(ebx, dword_ptr(edx)).unwrap();
                    a.mov(ebx, dword_ptr(edx + 4)).unwrap();
                    a.mul(ecx).unwrap(); // Writes EDX and EAX implicitly.
                }
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
            });
            assert!(analyze(&code).is_none());
        }
    }

    // spec: EXEC-X64-9
    #[test]
    fn rmw_requires_read_and_write_but_unknown_aliasing_stores_are_allowed() {
        let code = decode(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp - 4)).unwrap();
            a.add(dword_ptr(ebp - 4), 1).unwrap();
            a.mov(dword_ptr(eax), ebx).unwrap(); // Could alias; no data is cached.
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(plan.sites.len(), 2);
        assert_eq!((plan.groups[0].low, plan.groups[0].high), (-4, 0));
        assert!(plan.groups[0].read && plan.groups[0].write);
    }

    #[test]
    fn invariant_index_is_supported_and_partial_index_write_is_rejected() {
        for mutate in [false, true] {
            let code = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(ebp + esi * 4 - 4)).unwrap();
                a.mov(edx, dword_ptr(ebp + esi * 4 + 8)).unwrap();
                if mutate {
                    a.inc(si).unwrap();
                }
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
            });
            if mutate {
                assert!(analyze(&code).is_none());
            } else {
                let plan = analyze(&code).unwrap();
                assert_eq!(plan.groups[0].index, Some((6, 4)));
                assert_eq!((plan.groups[0].low, plan.groups[0].high), (-4, 12));
            }
        }
    }

    #[test]
    fn segments_narrow_addresses_lock_and_absolute_addresses_are_not_proved() {
        for kind in 0..5 {
            let code = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                for _ in 0..2 {
                    match kind {
                        0 => a.mov(eax, dword_ptr(ebp).fs()).unwrap(),
                        1 => a.mov(eax, dword_ptr(ebp).gs()).unwrap(),
                        2 => a.mov(eax, dword_ptr(bp + si)).unwrap(),
                        3 => a.lock().add(dword_ptr(ebp), 1).unwrap(),
                        // An absolute moffs32 address is unsigned. Casting it
                        // through i32 makes the assembler see a 64-bit sign extension.
                        _ => a.mov(eax, dword_ptr(0x80000000u32)).unwrap(),
                    }
                }
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
            });
            assert!(analyze(&code).is_none());
        }
    }

    #[test]
    fn forward_only_and_single_loop_site_do_not_enable_a_clone() {
        let forward = decode(|a| {
            a.mov(eax, dword_ptr(ebp)).unwrap();
            a.mov(edx, dword_ptr(ebp + 4)).unwrap();
            a.ret().unwrap();
        });
        assert!(analyze(&forward).is_none());
        let single = decode(|a| {
            a.mov(edx, dword_ptr(ebp + 4)).unwrap(); // Outside the loop.
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        });
        assert!(analyze(&single).is_none());
    }

    #[test]
    fn group_spans_over_64k_are_rejected_instead_of_truncated() {
        for displacement in [65532, 65533] {
            let code = decode(|a| {
                let mut top = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(ebp)).unwrap();
                a.mov(edx, dword_ptr(ebp + displacement)).unwrap();
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
            });
            assert_eq!(analyze(&code).is_some(), displacement == 65532);
        }
    }

    #[test]
    fn prologue_and_teardown_can_write_the_loop_base_and_call_callees() {
        let code = decode(|a| {
            let mut top = a.create_label();
            let mut callee = a.create_label();
            a.push(ebp).unwrap();
            a.mov(ebp, esp).unwrap();
            a.sub(esp, 16).unwrap();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp - 4)).unwrap();
            a.mov(edx, dword_ptr(ebp - 8)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.mov(ebp, 0x30000).unwrap();
            a.call(callee).unwrap();
            a.add(esp, 16).unwrap();
            a.pop(ebp).unwrap();
            a.ret().unwrap();
            a.set_label(&mut callee).unwrap();
            a.ret().unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].base, Some(5));
        assert_eq!(code[&plan.header].memory_displacement32(), (-4i32) as u32);
        assert_eq!(
            code[&plan.latch].flow_control(),
            FlowControl::ConditionalBranch
        );
        assert_eq!(code[&plan.latch].near_branch32(), plan.header);
        assert_eq!(code[&plan.end].op0_register(), Register::EBP);
        assert_eq!(plan.sites.len(), 2);
        assert!(
            plan.sites
                .keys()
                .all(|pc| plan.header <= *pc && *pc < plan.end)
        );
    }

    #[test]
    fn reentry_with_a_different_base_and_partial_writes_outside_is_allowed() {
        let code = decode(|a| {
            let mut top = a.create_label();
            a.mov(ebp, 0x20000).unwrap();
            a.mov(esi, 3).unwrap();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp + esi * 4)).unwrap();
            a.mov(edx, dword_ptr(ebp + esi * 4 + 4)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
            a.mov(bp, ax).unwrap();
            a.mov(si, dx).unwrap();
            a.mov(ebp, 0x30000).unwrap();
            a.mov(ecx, 2).unwrap();
            a.jmp(top).unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(plan.groups[0].base, Some(5));
        assert_eq!(plan.groups[0].index, Some((6, 4)));
        assert_eq!(
            code[&plan.latch].flow_control(),
            FlowControl::ConditionalBranch
        );
        assert_eq!(code[&plan.end].op0_register(), Register::BP);
        // The later reentry backedge contains the first loop's conditional
        // branch and is not itself a straight-line candidate. Its new base is
        // read by the caller's proof each time the selected latch is taken.
        assert_eq!(plan.entries, BTreeSet::from([plan.header]));
        assert_eq!(plan.sites.len(), 2);
    }

    #[test]
    fn partial_base_and_index_writes_only_reject_when_inside_the_loop() {
        for inside in [false, true] {
            let code = decode(|a| {
                let mut top = a.create_label();
                if !inside {
                    a.mov(bp, ax).unwrap();
                    a.inc(si).unwrap();
                }
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(ebp + esi * 4)).unwrap();
                a.mov(edx, dword_ptr(ebp + esi * 4 + 4)).unwrap();
                if inside {
                    a.mov(bp, ax).unwrap();
                    a.inc(si).unwrap();
                }
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
                a.mov(bp, ax).unwrap();
                a.inc(si).unwrap();
            });
            assert_eq!(analyze(&code).is_some(), !inside);
        }
    }

    #[test]
    fn picks_one_loop_by_saved_checks_instead_of_unioning_scopes() {
        let code = decode(|a| {
            let mut first = a.create_label();
            let mut second = a.create_label();
            a.set_label(&mut first).unwrap();
            a.mov(eax, dword_ptr(ebp)).unwrap();
            a.mov(edx, dword_ptr(ebp + 4)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(first).unwrap();
            a.mov(ebp, 0x30000).unwrap();
            a.set_label(&mut second).unwrap();
            a.add(dword_ptr(ebp + 8), 1).unwrap();
            a.add(dword_ptr(ebp + 12), 1).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(second).unwrap();
            a.ret().unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(code[&plan.header].mnemonic(), Mnemonic::Add);
        assert_eq!(plan.sites.len(), 2);
        assert_eq!((plan.groups[0].low, plan.groups[0].high), (8, 16));
        assert!(plan.groups[0].read && plan.groups[0].write);
    }

    #[test]
    fn gaps_callees_indirect_control_and_internal_branches_are_rejected() {
        for kind in 0..4 {
            let mut code = decode(|a| {
                let mut top = a.create_label();
                let mut skip = a.create_label();
                a.set_label(&mut top).unwrap();
                a.mov(eax, dword_ptr(ebp)).unwrap();
                match kind {
                    0 => a.nop().unwrap(),
                    1 => a.call(eax).unwrap(),
                    2 => a.jmp(eax).unwrap(),
                    _ => a.jz(skip).unwrap(),
                }
                a.set_label(&mut skip).unwrap();
                a.mov(edx, dword_ptr(ebp + 4)).unwrap();
                a.dec(ecx).unwrap();
                a.jnz(top).unwrap();
            });
            if kind == 0 {
                let missing = code
                    .values()
                    .find(|i| i.mnemonic() == Mnemonic::Nop)
                    .unwrap()
                    .ip32();
                code.remove(&missing);
            }
            assert!(analyze(&code).is_none(), "unsupported control kind {kind}");
        }
    }

    #[test]
    fn external_side_entries_preserve_original_block_boundaries() {
        let code = decode(|a| {
            let mut top = a.create_label();
            let mut side = a.create_label();
            a.jz(side).unwrap();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp)).unwrap();
            a.set_label(&mut side).unwrap();
            a.mov(edx, dword_ptr(ebp + 4)).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        });
        let plan = analyze(&code).unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert!(plan.entries.contains(&plan.header));
        assert!(
            plan.entries
                .contains(&code.values().next().unwrap().near_branch32())
        );
    }
}
