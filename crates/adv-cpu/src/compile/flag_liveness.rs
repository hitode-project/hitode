//! Guest-level liveness of the six arithmetic flags.
//!
//! An exit resumes the guest at an exact PC. When every path from there writes
//! CF, PF, AF, ZF, SF and OF before reading any of them, the exit need not
//! materialize the lazy flags: no guest instruction can observe them. Exits
//! that do not need them leave the lazy values dead, which removes their
//! computation and their register pressure from the hot path.
use super::*;
use iced_x86::RflagsBits;

const ARITHMETIC: u32 = RflagsBits::OF
    | RflagsBits::SF
    | RflagsBits::ZF
    | RflagsBits::AF
    | RflagsBits::CF
    | RflagsBits::PF;

/// Arithmetic flags this instruction always replaces, counting flags it leaves
/// architecturally undefined: no correct program reads those. Shifts and
/// rotates leave every flag unchanged for a zero count, so only a nonzero
/// immediate count writes.
fn writes(i: &Instruction) -> u32 {
    let shift = matches!(
        i.mnemonic(),
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd
    );
    if shift {
        let count = (0..i.op_count())
            .rev()
            .find(|&n| i.op_kind(n) != OpKind::Register || i.op_register(n) == R::CL);
        let fixed = count.is_some_and(|n| {
            matches!(i.op_kind(n), OpKind::Immediate8 | OpKind::Immediate8_2nd)
                && (if i.op_kind(n) == OpKind::Immediate8 {
                    i.immediate8()
                } else {
                    i.immediate8_2nd()
                }) & 31
                    != 0
        });
        if !fixed {
            return 0;
        }
    }
    (i.rflags_written() | i.rflags_cleared() | i.rflags_set() | i.rflags_undefined()) & ARITHMETIC
}

/// PCs on entry to which no arithmetic flag is live within `instructions`.
/// Transfers leaving the map, returns, calls and indirect branches count as
/// reading every flag.
pub(super) fn dead_on_entry(instructions: &BTreeMap<u32, Instruction>) -> BTreeSet<u32> {
    let successors = |i: &Instruction| -> Option<Vec<u32>> {
        let next = i.next_ip32();
        let target = i.near_branch_target() as u32;
        match i.flow_control() {
            FlowControl::Next => Some(vec![next]),
            FlowControl::ConditionalBranch => Some(vec![next, target]),
            FlowControl::UnconditionalBranch => Some(vec![target]),
            _ => None,
        }
    };
    // The least fixed point: start with nothing live and grow.
    let mut live: BTreeMap<u32, u32> = instructions.keys().map(|&pc| (pc, 0)).collect();
    loop {
        let mut changed = false;
        for (&pc, i) in instructions.iter().rev() {
            let after = match successors(i) {
                Some(targets) => targets
                    .iter()
                    .map(|target| live.get(target).copied().unwrap_or(ARITHMETIC))
                    .fold(0, |a, b| a | b),
                None => ARITHMETIC,
            };
            let before = (i.rflags_read() & ARITHMETIC) | (after & !writes(i));
            let slot = live.get_mut(&pc).unwrap();
            if *slot != before {
                *slot = before;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    live.into_iter()
        .filter(|&(_, flags)| flags == 0)
        .map(|(pc, _)| pc)
        .collect()
}

/// EFLAGS bits that an exit at `pc` leaves unspecified, for code decoded
/// linearly from `start` and compiled as one region.
#[cfg(test)]
pub(super) fn unspecified_at(bytes: &[u8], start: u32, pc: u32) -> u32 {
    let mut decoder = Decoder::with_ip(32, bytes, u64::from(start), DecoderOptions::NONE);
    let instructions = decoder.iter().map(|i| (i.ip32(), i)).collect();
    if dead_on_entry(&instructions).contains(&pc) {
        0x8d5
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    fn map(build: impl FnOnce(&mut CodeAssembler)) -> BTreeMap<u32, Instruction> {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let bytes = a.assemble(0x1000).unwrap();
        let mut decoder = Decoder::with_ip(32, &bytes, 0x1000, DecoderOptions::NONE);
        decoder.iter().map(|i| (i.ip32(), i)).collect()
    }

    fn pcs(instructions: &BTreeMap<u32, Instruction>) -> Vec<u32> {
        instructions.keys().copied().collect()
    }

    #[test]
    fn flags_are_dead_until_the_next_full_writer() {
        let instructions = map(|a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(eax, 1).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.jne(0x1000u64).unwrap();
            a.ret().unwrap();
        });
        let pc = pcs(&instructions);
        let dead = dead_on_entry(&instructions);
        assert!(dead.contains(&pc[0]) && dead.contains(&pc[1]));
        // JNE reads ZF; the store before it sees the ADD's flags.
        assert!(!dead.contains(&pc[2]) && !dead.contains(&pc[3]));
        assert!(!dead.contains(&pc[4]));
    }

    #[test]
    fn partial_writers_and_zero_count_shifts_keep_flags_live() {
        for partial in [
            (|a: &mut CodeAssembler| a.inc(eax).unwrap()) as fn(&mut CodeAssembler),
            |a| a.shl(eax, cl).unwrap(),
            |a| a.shl(eax, 32).unwrap(),
            |a| a.bt(eax, 1).unwrap(),
        ] {
            let instructions = map(|a| {
                partial(a);
                // LAHF reads every arithmetic flag but OF.
                a.lahf().unwrap();
                a.add(eax, eax).unwrap();
                a.ret().unwrap();
            });
            let pc = pcs(&instructions);
            let dead = dead_on_entry(&instructions);
            assert!(!dead.contains(&pc[0]) && !dead.contains(&pc[1]));
            assert!(dead.contains(&pc[2]));
        }
    }

    #[test]
    fn loops_and_outside_targets_are_conservative() {
        let instructions = map(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.cmp(eax, ebx).unwrap();
            a.jl(top).unwrap();
            a.mov(eax, 1).unwrap();
            a.jmp(0x9000u64).unwrap();
        });
        let pc = pcs(&instructions);
        let dead = dead_on_entry(&instructions);
        assert!(dead.contains(&pc[0]) && dead.contains(&pc[1]));
        assert!(!dead.contains(&pc[2]) && !dead.contains(&pc[3]) && !dead.contains(&pc[4]));
    }
}
