//! Conservative row independence over an existing innermost loop plan.
//!
//! A row begins at the inner header. Its induction registers may carry only
//! `self + invariant` into the next row; reset registers must return to the
//! same invariant expression. All remaining architectural outputs are tracked
//! and must be fully written before a row reads them, on every path. This is
//! an independence proof, never permission to discard an observable value.
//!
//! Memory values in the glue are symbolic invariant inputs. The runtime must
//! prove their bytes read-only with respect to every admitted row's writes,
//! together with the ordinary-memory and row-overlap checks. Observing one
//! serial row supplies values for the proven affine steps, not their proof.
use super::*;
use iced_x86::{InstructionInfoFactory, OpAccess, RflagsBits};

/// A read outside the inner loop. Its address components never change anywhere
/// in the entire nest, so its operand is also valid at the inner header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GlueRead {
    pub pc: u32,
    pub operand: loops::Operand,
}

#[derive(Debug, Clone)]
pub(super) struct Nest {
    pub inner: usize,
    pub header: u32,
    pub latch: u32,
    /// The boundary immediately after the inner counted branch.
    pub exit: u32,
    pub counter: usize,
    /// General registers with a proved constant row-to-row step.
    pub inductions: u8,
    /// General registers with the same value at every inner header.
    pub invariant: u8,
    /// Written architectural registers except the row inductions. Bits 0..8
    /// are GPRs, 8..16 MMX, and 16..24 XMM. Writable header invariants (including
    /// the inner counter) remain tracked: their values at the inner EXIT matter.
    pub tracked: u32,
    pub glue_cost: u64,
    pub inner_cost: u64,
    pub glue_reads: Vec<GlueRead>,
}

const ARITHMETIC: u32 = RflagsBits::CF
    | RflagsBits::PF
    | RflagsBits::AF
    | RflagsBits::ZF
    | RflagsBits::SF
    | RflagsBits::OF;

fn reads(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Read | OpAccess::CondRead | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}

fn writes(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}

fn register_bit(register: R) -> Option<u32> {
    reg_index(register.full_register32())
        .map(|index| 1 << index)
        .or_else(|| mmx::register(register).map(|index| 1 << (8 + index)))
        .or_else(|| simd::register(register).map(|index| 1 << (16 + index)))
}

/// Every architectural register an instruction may write, including narrow
/// and conditional writes. The emitter uses the same packing as `tracked`.
pub(super) fn written(i: &Instruction) -> u32 {
    let mut factory = InstructionInfoFactory::new();
    factory
        .info(i)
        .used_registers()
        .iter()
        .filter(|used| writes(used.access()))
        .filter_map(|used| register_bit(used.register()))
        .fold(0, |mask, bit| mask | bit)
}

/// Registers read and definitely fully written. A partial/conditional write
/// reads the old architectural value even if the decoder lists only a write.
fn effects(factory: &mut InstructionInfoFactory, i: &Instruction) -> Option<(u32, u32)> {
    let info = factory.info(i);
    let (mut read, mut full) = (0, 0);
    for used in info.used_registers() {
        let register = used.register();
        let Some(bit) = register_bit(register) else {
            if register.is_segment_register() && !writes(used.access()) {
                continue;
            }
            return None;
        };
        let narrow = reg_index(register.full_register32()).is_some() && register.size() != 4;
        if reads(used.access())
            || narrow && writes(used.access())
            || matches!(used.access(), OpAccess::CondWrite | OpAccess::ReadCondWrite)
        {
            read |= bit;
        }
        if !narrow && matches!(used.access(), OpAccess::Write | OpAccess::ReadWrite) {
            full |= bit;
        }
    }
    for memory in info.used_memory() {
        for register in [memory.base(), memory.index()] {
            if register != R::None {
                read |= register_bit(register)?;
            }
        }
    }
    // Whole-register zero idioms do not depend on the previous value.
    if matches!(i.mnemonic(), M::Xor | M::Sub | M::Pxor)
        && i.op_count() == 2
        && i.op0_kind() == OpKind::Register
        && i.op1_kind() == OpKind::Register
        && i.op0_register() == i.op1_register()
        && !i.op0_register().is_gpr8()
        && !i.op0_register().is_gpr16()
    {
        read &= !register_bit(i.op0_register())?;
    }
    Some((read, full))
}

fn ordinary(i: &Instruction) -> bool {
    !i.is_invalid()
        && !i.has_lock_prefix()
        && !i.has_rep_prefix()
        && !i.has_repne_prefix()
        && i.segment_prefix() == R::None
        && supported(i)
}

/// Existing integer lowering only. In particular no x87, MXCSR-changing SSE,
/// alignment fault, division, implicit stack access, atomics or system state.
fn inner_supported(i: &Instruction) -> bool {
    ordinary(i)
        && (matches!(
            i.mnemonic(),
            M::Mov
                | M::Movzx
                | M::Movsx
                | M::Lea
                | M::Add
                | M::Sub
                | M::And
                | M::Or
                | M::Xor
                | M::Cmp
                | M::Test
                | M::Inc
                | M::Dec
                | M::Not
                | M::Neg
                | M::Adc
                | M::Sbb
                | M::Mul
                | M::Imul
                | M::Cdq
                | M::Shl
                | M::Shr
                | M::Sar
                | M::Rol
                | M::Ror
                | M::Nop
                | M::Je
                | M::Jne
                | M::Ja
                | M::Jae
                | M::Jb
                | M::Jbe
                | M::Jg
                | M::Jge
                | M::Jl
                | M::Jle
                | M::Jo
                | M::Jno
                | M::Js
                | M::Jns
                | M::Jp
                | M::Jnp
                | M::Jmp
                | M::Sete
                | M::Setne
                | M::Setb
                | M::Setae
                | M::Setbe
                | M::Seta
                | M::Setl
                | M::Setge
                | M::Setle
                | M::Setg
                | M::Seto
                | M::Setno
                | M::Sets
                | M::Setns
                | M::Setp
                | M::Setnp
        ) || mmx::supported(i) && i.mnemonic() != M::Emms
            || simd::supported(i)
                && i.mnemonic() != M::Movdqa
                && (i.mnemonic() == M::Movdqu
                    || (0..i.op_count()).all(|n| i.op_kind(n) != OpKind::Memory)))
}

/// Undefined flags are not definitions: MUL lowering, for example, preserves
/// them. A variable or zero shift count may also preserve every flag.
fn defined_flags(i: &Instruction) -> u32 {
    let mut defined = i.rflags_written() | i.rflags_cleared() | i.rflags_set();
    if matches!(i.mnemonic(), M::Shl | M::Shr | M::Sar | M::Rol | M::Ror) {
        if i.op1_kind() != OpKind::Immediate8 {
            return 0;
        }
        let count = u32::from(i.immediate8() & 31);
        if count == 0 {
            return 0;
        }
        if count != 1 {
            defined &= !RflagsBits::OF;
        }
        if matches!(i.mnemonic(), M::Shl | M::Shr | M::Sar)
            && count >= i.op0_register().size() as u32 * 8
        {
            defined &= !RflagsBits::CF;
        }
    }
    defined & ARITHMETIC
}

/// Modulo-2^32 affine expression in the first header's GPRs and invariant
/// memory reads. An unknown is deliberately contagious, even when code later
/// subtracts two unknown values. Only full-register zero idioms escape it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Affine {
    registers: [u32; 8],
    memory: BTreeMap<u32, u32>,
    constant: u32,
}

impl Affine {
    fn constant(value: u32) -> Self {
        Self {
            registers: [0; 8],
            memory: BTreeMap::new(),
            constant: value,
        }
    }

    fn register(index: usize) -> Self {
        let mut value = Self::constant(0);
        value.registers[index] = 1;
        value
    }

    fn memory(pc: u32) -> Self {
        let mut value = Self::constant(0);
        value.memory.insert(pc, 1);
        value
    }

    fn scaled(mut self, scale: u32) -> Self {
        for coefficient in &mut self.registers {
            *coefficient = coefficient.wrapping_mul(scale);
        }
        self.memory.retain(|_, coefficient| {
            *coefficient = coefficient.wrapping_mul(scale);
            *coefficient != 0
        });
        self.constant = self.constant.wrapping_mul(scale);
        self
    }

    fn plus(mut self, other: &Self) -> Self {
        for (coefficient, other) in self.registers.iter_mut().zip(other.registers) {
            *coefficient = coefficient.wrapping_add(other);
        }
        for (&pc, &coefficient) in &other.memory {
            let sum = self
                .memory
                .get(&pc)
                .copied()
                .unwrap_or(0)
                .wrapping_add(coefficient);
            if sum == 0 {
                self.memory.remove(&pc);
            } else {
                self.memory.insert(pc, sum);
            }
        }
        self.constant = self.constant.wrapping_add(other.constant);
        self
    }

    fn invariant(&self, mask: u8) -> bool {
        self.registers
            .iter()
            .enumerate()
            .all(|(index, &coefficient)| coefficient == 0 || mask & 1 << index != 0)
    }
}

fn immediate(i: &Instruction, n: u32) -> Option<u32> {
    matches!(
        i.op_kind(n),
        OpKind::Immediate32 | OpKind::Immediate8to32 | OpKind::Immediate8
    )
    .then(|| i.immediate(n) as u32)
}

fn operand(i: &Instruction) -> Option<loops::Operand> {
    let component = |r| {
        if r == R::None {
            Some(None)
        } else {
            reg_index(r).map(Some)
        }
    };
    Some(loops::Operand {
        width: i.memory_size().size() as u32,
        base: component(i.memory_base())?,
        index: component(i.memory_index())?.map(|index| (index, i.memory_index_scale())),
        displacement: i.memory_displacement32() as i32,
    })
}

/// Glue is straight-line, store-free integer code with explicit invariant
/// reads. Its changing values use only affine operations we can prove.
fn glue_supported(i: &Instruction) -> bool {
    if !ordinary(i) || i.flow_control() != FlowControl::Next {
        return false;
    }
    match i.mnemonic() {
        M::Nop => true,
        M::Cmp | M::Test => true,
        M::Mov | M::Add | M::Sub | M::Inc | M::Dec | M::Lea | M::Xor | M::Imul | M::Shl => {
            i.op0_kind() == OpKind::Register && reg_index(i.op0_register()).is_some()
        }
        _ => false,
    }
}

fn apply_glue(
    i: &Instruction,
    values: &mut [Option<Affine>; 8],
    reads: &BTreeSet<u32>,
) -> Option<()> {
    if matches!(i.mnemonic(), M::Nop | M::Cmp | M::Test) {
        return Some(());
    }
    let destination = reg_index(i.op0_register())?;
    let source = |n| match i.op_kind(n) {
        OpKind::Register => values[reg_index(i.op_register(n))?].clone(),
        OpKind::Memory if reads.contains(&i.ip32()) => Some(Affine::memory(i.ip32())),
        _ => immediate(i, n).map(Affine::constant),
    };
    let old = values[destination].clone();
    let value = match i.mnemonic() {
        M::Mov => source(1),
        M::Add => old.zip(source(1)).map(|(a, b)| a.plus(&b)),
        M::Sub if i.op1_kind() == OpKind::Register && i.op1_register() == i.op0_register() => {
            Some(Affine::constant(0))
        }
        M::Sub => old.zip(source(1)).map(|(a, b)| a.plus(&b.scaled(u32::MAX))),
        M::Inc => old.map(|a| a.plus(&Affine::constant(1))),
        M::Dec => old.map(|a| a.plus(&Affine::constant(u32::MAX))),
        M::Xor if i.op1_kind() == OpKind::Register && i.op1_register() == i.op0_register() => {
            Some(Affine::constant(0))
        }
        M::Lea => {
            let mut value = Some(Affine::constant(i.memory_displacement32()));
            for (register, scale) in [
                (i.memory_base(), 1),
                (i.memory_index(), i.memory_index_scale()),
            ] {
                if register != R::None {
                    value = value
                        .zip(values[reg_index(register)?].clone())
                        .map(|(a, b)| a.plus(&b.scaled(scale)));
                }
            }
            value
        }
        M::Imul if i.op_count() == 3 => {
            let scale = immediate(i, 2)?;
            source(1).map(|a| a.scaled(scale))
        }
        M::Shl if i.op1_kind() == OpKind::Immediate8 => {
            old.map(|a| a.scaled(1u32 << (i.immediate8() & 31)))
        }
        _ => return None,
    };
    values[destination] = value;
    Some(())
}

#[derive(Clone, Copy)]
struct State {
    written: u32,
    flags: u32,
}

fn meet(a: State, b: State) -> State {
    State {
        written: a.written & b.written,
        flags: a.flags & b.flags,
    }
}

/// A forward-only inner CFG, with its counted back edge treated as the row
/// exit. Reads cannot depend on an uninitialized worker row's architectural
/// values. Conditional writers are retained even if no later code reads them.
fn independent(
    body: &[&Instruction],
    plan: &loops::Plan,
    needs_write: u32,
    glue_flags: u32,
    factory: &mut InstructionInfoFactory,
) -> Option<()> {
    let mut incoming = BTreeMap::<u32, State>::new();
    let mut current = Some(State {
        written: 0,
        flags: 0,
    });
    let mut changed_flags = glue_flags;
    for i in body {
        let pc = i.ip32();
        let state = match (current, incoming.remove(&pc)) {
            (Some(a), Some(b)) => meet(a, b),
            (a, b) => a.or(b)?,
        };
        let (read, full) = effects(factory, i)?;
        if read & needs_write & !state.written != 0 || i.rflags_read() & !state.flags != 0 {
            return None;
        }
        let state = State {
            written: state.written | full,
            flags: state.flags | defined_flags(i),
        };
        changed_flags |= i.rflags_modified();
        if pc == plan.latch {
            // At the inner exit, even an interrupt can expose preserved flags.
            // Any row-varying flag must already be this row's own result.
            return (changed_flags & ARITHMETIC & !state.flags == 0).then_some(());
        }
        match i.flow_control() {
            FlowControl::Next => current = Some(state),
            FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch => {
                let target = i.near_branch32();
                let merged = incoming.get(&target).map_or(state, |&s| meet(s, state));
                incoming.insert(target, merged);
                current = (i.flow_control() == FlowControl::ConditionalBranch).then_some(state);
            }
            _ => return None,
        }
    }
    None
}

pub(super) fn nests(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    plans: &[loops::Plan],
) -> Vec<Nest> {
    instructions
        .values()
        .filter(|i| {
            i.mnemonic() == M::Jne
                && i.flow_control() == FlowControl::ConditionalBranch
                && i.op0_kind() == OpKind::NearBranch32
                && i.near_branch32() < i.ip32()
                && entries.contains(&i.near_branch32())
        })
        .filter_map(|latch| nest(instructions, entries, plans, latch))
        .collect()
}

fn nest(
    instructions: &BTreeMap<u32, Instruction>,
    entries: &BTreeSet<u32>,
    plans: &[loops::Plan],
    latch: &Instruction,
) -> Option<Nest> {
    let header = latch.near_branch32();
    let body: Vec<_> = instructions
        .range(header..=latch.ip32())
        .map(|(_, i)| i)
        .collect();
    if body.first()?.ip32() != header
        || body
            .windows(2)
            .any(|pair| pair[0].next_ip32() != pair[1].ip32())
    {
        return None;
    }
    let contained: Vec<_> = plans
        .iter()
        .enumerate()
        .filter(|(_, p)| header < p.header && p.latch < latch.ip32())
        .collect();
    let [(inner, plan)] = contained.as_slice() else {
        return None;
    };
    if plans.iter().any(|p| {
        p.header <= latch.ip32()
            && header <= p.latch
            && (p.header != plan.header || p.latch != plan.latch)
    }) || plan.counter_size != 4
        // Row summaries currently require an exact countdown to zero. General
        // scalar kernels also accept bounds and strides; those are not yet
        // covered by the row runtime's width/fuel and final-state contract.
        || !matches!(plan.bound, loops::Bound::Constant(0))
        || plan.inclusive
        || plan.steps[plan.counter] != -1
        || instructions.get(&plan.counter_step)?.next_ip32() != plan.latch
        || !plan.windows.is_empty()
        || !plan.slots.is_empty()
    {
        return None;
    }
    let before = body.get(body.len().checked_sub(2)?)?;
    let counter = reg_index(before.op0_register())?;
    if before.op0_kind() != OpKind::Register
        || !(before.mnemonic() == M::Dec
            || before.mnemonic() == M::Sub && immediate(before, 1) == Some(1))
        || counter == plan.counter
        || !ordinary(latch)
    {
        return None;
    }
    let inner_latch = instructions.get(&plan.latch)?;
    let exit = inner_latch.next_ip32();
    let inner_body: Vec<_> = body
        .iter()
        .copied()
        .filter(|i| plan.contains(i.ip32()))
        .collect();
    let prefix: Vec<_> = body
        .iter()
        .copied()
        .filter(|i| i.ip32() < plan.header)
        .collect();
    let suffix: Vec<_> = body
        .iter()
        .copied()
        .filter(|i| exit <= i.ip32() && i.ip32() < latch.ip32())
        .collect();
    let glue: Vec<_> = prefix.iter().chain(&suffix).copied().collect();
    // Every inner transfer is internal and forward, except its own counted
    // latch. Glue has no branches, calls, stores or hidden instruction effects.
    for i in &inner_body {
        if !inner_supported(i) {
            return None;
        }
        if i.ip32() == plan.latch {
            if i.mnemonic() != M::Jne || i.near_branch32() != plan.header {
                return None;
            }
        } else {
            match i.flow_control() {
                FlowControl::Next => {}
                FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch
                    if i.op0_kind() == OpKind::NearBranch32
                        && i.ip32() < i.near_branch32()
                        && i.near_branch32() <= plan.latch
                        && instructions.contains_key(&i.near_branch32()) => {}
                _ => return None,
            }
        }
    }
    if glue.iter().any(|i| !glue_supported(i)) {
        return None;
    }
    // Incoming edges to the row interior would bypass its proven resets.
    if instructions.values().any(|i| {
        !(header..=latch.ip32()).contains(&i.ip32())
            && i.op0_kind() == OpKind::NearBranch32
            && header < i.near_branch32()
            && i.near_branch32() <= latch.ip32()
    }) {
        return None;
    }
    let mut factory = InstructionInfoFactory::new();
    let mut inner_written = 0;
    let mut glue_written = 0;
    let mut glue_flags = 0;
    for i in &inner_body {
        let (read, _) = effects(&mut factory, i)?;
        let write = written(i);
        // The outer count changes every row, so it is not an inner parameter.
        if (read | write) & 1 << counter != 0 {
            return None;
        }
        inner_written |= write;
        let memory = factory.info(i).used_memory().to_vec();
        if i.mnemonic() != M::Lea && i.mnemonic() != M::Nop && !memory.is_empty() {
            if memory.len() != 1 || !plan.proven.contains_key(&i.ip32()) {
                return None;
            }
        }
    }
    for i in &glue {
        effects(&mut factory, i)?;
        let write = written(i);
        if write & !0xff != 0 || write & 1 << counter != 0 && i.ip32() != before.ip32() {
            return None;
        }
        glue_written |= write;
        glue_flags |= i.rflags_modified();
    }
    let immutable = !(inner_written | glue_written) as u8;
    let mut glue_reads = Vec::new();
    for i in &glue {
        let memory = factory.info(i).used_memory().to_vec();
        if i.mnemonic() == M::Lea || i.mnemonic() == M::Nop || memory.is_empty() {
            continue;
        }
        if memory.len() != 1
            || writes(memory[0].access())
            || !reads(memory[0].access())
            || !(0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory)
        {
            return None;
        }
        let operand = operand(i)?;
        if operand.width != 4
            || [operand.base, operand.index.map(|(index, _)| index)]
                .into_iter()
                .flatten()
                .any(|index| immutable & 1 << index == 0)
        {
            return None;
        }
        glue_reads.push(GlueRead {
            pc: i.ip32(),
            operand,
        });
    }
    let read_pcs = glue_reads.iter().map(|read| read.pc).collect();
    let mut reset: [Option<Affine>; 8] =
        std::array::from_fn(|index| (immutable & 1 << index != 0).then(|| Affine::register(index)));
    for i in &prefix {
        apply_glue(i, &mut reset, &read_pcs)?;
    }
    // A row count must be reestablished independently of the previous row.
    let trip = reset[plan.counter].clone()?;
    if !trip.invariant(immutable) {
        return None;
    }
    let seed: [Option<Affine>; 8] = std::array::from_fn(|index| {
        Some(
            reset[index]
                .clone()
                .unwrap_or_else(|| Affine::register(index)),
        )
    });
    let mut next = seed.clone();
    for index in 0..8 {
        if plan.inductions & 1 << index != 0 {
            next[index] = next[index]
                .take()
                .map(|a| a.plus(&trip.clone().scaled(plan.steps[index] as u32)));
        } else if inner_written & 1 << index != 0 {
            next[index] = None;
        }
    }
    for i in suffix.iter().chain(&prefix) {
        apply_glue(i, &mut next, &read_pcs)?;
    }
    let invariant = (0..8)
        .filter(|&index| next[index].is_some() && next[index] == seed[index])
        .fold(0u8, |mask, index| mask | 1 << index);
    if invariant & 1 << plan.counter == 0 || invariant & immutable != immutable {
        return None;
    }
    let mut inductions = 0u8;
    for index in 0..8 {
        if invariant & 1 << index != 0 {
            continue;
        }
        let Some(value) = &next[index] else { continue };
        let Some(seed) = &seed[index] else { continue };
        let delta = value.clone().plus(&seed.clone().scaled(u32::MAX));
        if delta.invariant(invariant) && value.registers[index] == 1 {
            inductions |= 1 << index;
        }
    }
    if inductions & 1 << counter == 0 {
        return None;
    }
    let tracked = inner_written & !u32::from(inductions);
    let varying = inner_written & !u32::from(invariant | inductions);
    // Glue may reset an invariant or advance an induction, but cannot use a
    // varying inner result to compute the next row's input or output state.
    if glue_written & !u32::from(invariant | inductions) != 0 {
        return None;
    }
    independent(&inner_body, plan, varying, glue_flags, &mut factory)?;
    let glue_cost = entries
        .range(header..=latch.ip32())
        .filter(|&&pc| !plan.contains(pc))
        .count() as u64;
    Some(Nest {
        inner: *inner,
        header,
        latch: latch.ip32(),
        exit,
        counter,
        inductions,
        invariant,
        tracked,
        glue_cost,
        inner_cost: plan.starts.len() as u64,
        glue_reads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    fn decode(
        build: impl FnOnce(&mut CodeAssembler),
    ) -> (BTreeMap<u32, Instruction>, BTreeSet<u32>, Vec<loops::Plan>) {
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
        let plans = loops::plans(&instructions, &entries, supported);
        (instructions, entries, plans)
    }

    fn analyze(build: impl FnOnce(&mut CodeAssembler)) -> Vec<Nest> {
        let (instructions, entries, plans) = decode(build);
        nests(&instructions, &entries, &plans)
    }

    #[test]
    fn generalized_inner_counters_stay_on_scalar_or_vector_kernels() {
        for shape in 0..3 {
            let (instructions, entries, plans) = decode(|a| {
                let mut outer = a.create_label();
                let mut inner = a.create_label();
                a.set_label(&mut outer).unwrap();
                a.mov(ecx, 20).unwrap();
                a.set_label(&mut inner).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.add(esi, 4).unwrap();
                a.add(edi, 4).unwrap();
                match shape {
                    0 => {
                        a.sub(ecx, 2).unwrap();
                        a.mov(dword_ptr(edi), eax).unwrap();
                        a.jne(inner).unwrap();
                    }
                    1 => {
                        a.mov(dword_ptr(edi), eax).unwrap();
                        a.dec(ecx).unwrap();
                        a.cmp(ecx, 4).unwrap();
                        a.ja(inner).unwrap();
                    }
                    _ => {
                        a.dec(ecx).unwrap();
                        a.mov(dword_ptr(edi), eax).unwrap();
                        a.jne(inner).unwrap();
                    }
                }
                a.dec(edx).unwrap();
                a.jne(outer).unwrap();
                a.ret().unwrap();
            });
            assert_eq!(plans.len(), 1, "shape {shape}");
            assert!(
                nests(&instructions, &entries, &plans).is_empty(),
                "shape {shape}"
            );
        }
    }

    #[test]
    fn proves_constant_row_steps_and_invariant_memory_resets() {
        let nests = analyze(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(ecx, dword_ptr(ebp + 8)).unwrap();
            a.set_label(&mut inner).unwrap();
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.add(esi, dword_ptr(ebp + 12)).unwrap();
            a.add(edi, dword_ptr(ebp + 16)).unwrap();
            a.dec(edx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        assert_eq!(nests.len(), 1);
        let nest = &nests[0];
        assert_eq!(nest.counter, 2);
        assert_eq!(nest.inductions, (1 << 2) | (1 << 6) | (1 << 7));
        assert_ne!(nest.invariant & (1 << 1), 0, "the counter resets each row");
        assert_eq!(nest.tracked, (1 << 1) | (1 << 8));
        assert_eq!(nest.glue_reads.len(), 3);
        assert!(nest.glue_reads.iter().all(|r| r.operand.base == Some(5)));
        assert_eq!((nest.glue_cost, nest.inner_cost), (2, 1));
    }

    #[test]
    fn preserves_a_conditional_mmx_writer_without_requiring_it_on_every_path() {
        let nests = analyze(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            let mut skip = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(ecx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.je(skip).unwrap();
            a.movd(mm0, eax).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.set_label(&mut skip).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.dec(edx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        assert_eq!(nests.len(), 1);
        assert_ne!(nests[0].tracked & (1 << 8), 0);
        assert_ne!(nests[0].tracked & 1, 0);
        assert_eq!(nests[0].inner_cost, 3);
    }

    #[test]
    fn rejects_row_steps_that_depend_on_the_changing_outer_counter() {
        let nests = analyze(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(ecx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.add(esi, edx).unwrap();
            a.dec(edx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        assert!(
            nests.is_empty(),
            "two observed deltas would not prove the next one"
        );
    }

    #[test]
    fn rejects_carried_temporaries_and_partial_first_writes() {
        for first in [
            (|a: &mut CodeAssembler| a.add(eax, dword_ptr(esi)).unwrap()) as fn(&mut CodeAssembler),
            |a| a.mov(al, byte_ptr(esi)).unwrap(),
        ] {
            let nests = analyze(|a| {
                let mut outer = a.create_label();
                let mut inner = a.create_label();
                a.set_label(&mut outer).unwrap();
                a.mov(ecx, 4).unwrap();
                a.set_label(&mut inner).unwrap();
                first(a);
                a.mov(dword_ptr(edi), eax).unwrap();
                a.add(esi, 4).unwrap();
                a.add(edi, 4).unwrap();
                a.dec(ecx).unwrap();
                a.jne(inner).unwrap();
                a.dec(edx).unwrap();
                a.jne(outer).unwrap();
                a.ret().unwrap();
            });
            assert!(nests.is_empty());
        }
    }

    #[test]
    fn rejects_side_exits_and_incoming_flag_dependencies() {
        for early in [
            (|a: &mut CodeAssembler, out| {
                a.test(eax, eax).unwrap();
                a.je(out).unwrap();
            }) as fn(&mut CodeAssembler, CodeLabel),
            |a, _| a.adc(eax, 1).unwrap(),
        ] {
            let nests = analyze(|a| {
                let mut outer = a.create_label();
                let mut inner = a.create_label();
                let mut out = a.create_label();
                a.set_label(&mut outer).unwrap();
                a.mov(ecx, 4).unwrap();
                a.set_label(&mut inner).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                early(a, out);
                a.mov(dword_ptr(edi), eax).unwrap();
                a.add(esi, 4).unwrap();
                a.add(edi, 4).unwrap();
                a.dec(ecx).unwrap();
                a.jne(inner).unwrap();
                a.dec(edx).unwrap();
                a.jne(outer).unwrap();
                a.set_label(&mut out).unwrap();
                a.ret().unwrap();
            });
            assert!(nests.is_empty());
        }
    }

    #[test]
    fn rejects_glue_flags_preserved_at_the_inner_exit() {
        let nests = analyze(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(ecx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(edx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        assert!(
            nests.is_empty(),
            "DEC does not replace the previous row's CF"
        );
    }

    #[test]
    fn tracks_all_mul_outputs_and_all_register_classes() {
        let nests = analyze(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(ecx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mul(ebp).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.movd(mm0, eax).unwrap();
            a.movdqu(xmm0, xmm1).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.dec(ebx).unwrap();
            a.jne(outer).unwrap();
            a.ret().unwrap();
        });
        assert_eq!(nests.len(), 1);
        assert_eq!(
            nests[0].tracked,
            1 | (1 << 1) | (1 << 2) | (1 << 8) | (1 << 16)
        );
    }
}
