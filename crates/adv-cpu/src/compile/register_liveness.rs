//! Guest-level liveness of the eight general registers.
//!
//! An exit that resumes the guest at an exact PC need not carry a register
//! that every path from there fully overwrites before reading it. Such exits
//! keep the value already in `CpuState`, so a temporary that a loop recomputes
//! each iteration (or the high half of a `mul`) is no longer live across the
//! back edge, which removes its computation and its register pressure.
//!
//! Only exits that resume execution use this. Exits that raise or replay a
//! guest exception store every register, because an exception context shows
//! all of them. As with dead flags, a fault after a resuming exit and before
//! the overwrite would show the older value; no correct program reads it.
use super::*;
use iced_x86::{InstructionInfoFactory, OpAccess};

/// Every register; ESP is always live because the stack is.
pub(super) const ALL: u8 = 0xff;
const ESP: u8 = 1 << 4;

/// Registers read and fully written by one instruction. A narrow or
/// conditional write merges with the old value, so it reads instead.
fn effects(factory: &mut InstructionInfoFactory, i: &Instruction) -> (u8, u8) {
    let (mut read, mut written) = (0, 0);
    let info = factory.info(i);
    for used in info.used_registers() {
        let register = used.register();
        let Some(index) = reg_index(register.full_register32()) else {
            continue;
        };
        let bit = 1 << index;
        match used.access() {
            OpAccess::Write if register.size() == 4 => written |= bit,
            OpAccess::Write
            | OpAccess::CondWrite
            | OpAccess::Read
            | OpAccess::CondRead
            | OpAccess::ReadWrite
            | OpAccess::ReadCondWrite => read |= bit,
            _ => {}
        }
    }
    for memory in info.used_memory() {
        for register in [memory.base(), memory.index()] {
            if let Some(index) = reg_index(register) {
                read |= 1 << index;
            }
        }
    }
    (read, written & !read)
}

/// The registers live on entry to each PC within `instructions`. Transfers
/// leaving the map, calls, returns, indirect branches and interrupts count as
/// reading every register.
pub(super) fn live_on_entry(instructions: &BTreeMap<u32, Instruction>) -> BTreeMap<u32, u8> {
    let mut factory = InstructionInfoFactory::new();
    let effects: BTreeMap<u32, (u8, u8)> = instructions
        .iter()
        .map(|(&pc, i)| (pc, effects(&mut factory, i)))
        .collect();
    let successors = |i: &Instruction| -> Option<[Option<u32>; 2]> {
        let target = i.near_branch_target() as u32;
        match i.flow_control() {
            FlowControl::Next => Some([Some(i.next_ip32()), None]),
            FlowControl::ConditionalBranch => Some([Some(i.next_ip32()), Some(target)]),
            FlowControl::UnconditionalBranch => Some([Some(target), None]),
            _ => None,
        }
    };
    // The least fixed point: start with nothing live and grow.
    let mut live: BTreeMap<u32, u8> = instructions.keys().map(|&pc| (pc, ESP)).collect();
    loop {
        let mut changed = false;
        for (&pc, i) in instructions.iter().rev() {
            let after = match successors(i) {
                Some(targets) => targets
                    .into_iter()
                    .flatten()
                    .map(|target| live.get(&target).copied().unwrap_or(ALL))
                    .fold(0, |a, b| a | b),
                None => ALL,
            };
            let (read, written) = effects[&pc];
            let before = read | (after & !written) | ESP;
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
    live
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

    fn at(instructions: &BTreeMap<u32, Instruction>, n: usize) -> u8 {
        let live = live_on_entry(instructions);
        live[instructions.keys().nth(n).unwrap()]
    }

    const EAX: u8 = 1;
    const ECX: u8 = 2;
    const EDX: u8 = 4;
    const EBX: u8 = 8;
    const EBP: u8 = 32;
    const ESI: u8 = 64;
    const EDI: u8 = 128;

    #[test]
    fn loop_temporaries_and_mul_high_halves_are_dead_at_the_header() {
        let instructions = map(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.mov(eax, dword_ptr(ebp + 8)).unwrap();
            a.mov(ebx, dword_ptr(ebp - 4)).unwrap();
            a.mul(ebx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
            a.mov(eax, edx).unwrap();
            a.jmp(0x9000u64).unwrap();
        });
        // EDX is live only after the multiply that writes it.
        assert_eq!(at(&instructions, 0), ECX | ESP | EBP | EDI | ESI);
        let after_mul = at(&instructions, 3);
        assert_eq!(after_mul & (EAX | EDX), EAX | EDX);
    }

    #[test]
    fn narrow_and_conditional_writes_keep_the_register_live() {
        for partial in [
            (|a: &mut CodeAssembler| a.mov(al, 1).unwrap()) as fn(&mut CodeAssembler),
            |a| a.mov(ah, 1).unwrap(),
            |a| a.mov(ax, 1).unwrap(),
            |a| a.cmove(eax, ebx).unwrap(),
        ] {
            let instructions = map(|a| {
                partial(a);
                a.mov(dword_ptr(edi), eax).unwrap();
                a.mov(eax, 0).unwrap();
                a.ret().unwrap();
            });
            assert_ne!(at(&instructions, 0) & EAX, 0);
        }
    }

    #[test]
    fn calls_returns_and_outside_targets_read_every_register() {
        let instructions = map(|a| {
            a.mov(eax, 1).unwrap();
            a.call(0x9000u64).unwrap();
            a.mov(eax, 1).unwrap();
            a.jmp(0x9000u64).unwrap();
        });
        assert_eq!(at(&instructions, 1), ALL);
        assert_eq!(at(&instructions, 3), ALL);
        assert_eq!(at(&instructions, 0), ALL & !EAX);
        assert_eq!(at(&instructions, 2), ALL & !EAX);
    }

    #[test]
    fn address_registers_are_read() {
        let instructions = map(|a| {
            a.mov(eax, dword_ptr(esi + edx * 4 + 8)).unwrap();
            a.xor(esi, esi).unwrap();
            a.mov(edx, 0).unwrap();
            a.ret().unwrap();
        });
        let live = at(&instructions, 0);
        assert_eq!(live & (ESI | EDX | EAX), ESI | EDX);
    }
}
