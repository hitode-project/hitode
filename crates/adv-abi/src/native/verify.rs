//! Small admission checker for single-byte, forward recurrence loops.
//!
//! The checker decodes the snapshot itself. A successful check establishes one
//! complete iteration, its unique backedge, the decreasing counter and every
//! register/flag/memory effect. Repeating that transition is an induction over
//! a positive counter, not bounded execution or comparison on sample inputs.

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register as X86};

use super::*;

pub const RULE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub base: Register,
    pub offset: u32,
}

/// Exact expressions at the end of a single recurrence step. A low-byte write
/// preserves its enclosing register's high bits explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expression {
    Initial(Register),
    Add(Box<Expression>, u32),
    /// Memory immediately before this iteration's store, including any history
    /// written by preceding iterations. It is not a post-store reread.
    ReadByte(Address),
    SumByte(Box<Expression>, Box<Expression>),
    LowByte(Box<Expression>),
    ReplaceLowByte(Box<Expression>, Box<Expression>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arithmetic {
    Increment32,
    Decrement32,
    Add8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagEffect {
    pub arithmetic: Arithmetic,
    pub left: Expression,
    pub right: Option<Expression>,
}

/// The complete checked step. Effects remain ordered; INC and DEC preserve CF.
/// No instruction outside this transition is silently treated as a no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub registers: [Expression; 8],
    pub flags: Vec<FlagEffect>,
    pub destination: Address,
    pub value: Expression,
    pub instructions: u32,
}

/// A capability issued only after checking actual code against a reference
/// recurrence. It has no public constructor and cannot be deserialized.
///
/// ```compile_fail
/// use adv_abi::native::{CandidateBinding, VerifiedBinding};
/// fn bypass(candidate: CandidateBinding) -> VerifiedBinding {
///     candidate.into()
/// }
/// ```
#[derive(Clone, Debug)]
pub struct VerifiedBinding {
    candidate: CandidateBinding,
    dependencies: Vec<Dependency>,
    transition: Transition,
    conditions: EntryConditions,
}

impl VerifiedBinding {
    pub fn candidate(&self) -> &CandidateBinding {
        &self.candidate
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub fn transition(&self) -> &Transition {
        &self.transition
    }

    pub fn conditions(&self) -> &EntryConditions {
        &self.conditions
    }

    pub const fn rule_version(&self) -> u32 {
        RULE_VERSION
    }
}

fn counterexample(offset: u32, reason: &'static str) -> Rejection {
    Rejection::Counterexample { offset, reason }
}

fn unsupported(offset: u32, reason: &'static str) -> Rejection {
    Rejection::Unsupported { offset, reason }
}

pub(super) fn register(r: X86) -> Option<Register> {
    match r {
        X86::EAX | X86::AL => Register::new(0),
        X86::ECX | X86::CL => Register::new(1),
        X86::EDX | X86::DL => Register::new(2),
        X86::EBX | X86::BL => Register::new(3),
        X86::ESP => Register::new(4),
        X86::EBP => Register::new(5),
        X86::ESI => Register::new(6),
        X86::EDI => Register::new(7),
        _ => None,
    }
}

pub(super) fn plain_memory(i: &Instruction) -> bool {
    i.memory_index() == X86::None
        && i.memory_base().size() == 4
        && matches!(i.segment_prefix(), X86::None | X86::DS)
}

fn add(value: Expression, amount: u32) -> Expression {
    match (value, amount) {
        (value, 0) => value,
        (Expression::Add(value, old), amount) => add(*value, old.wrapping_add(amount)),
        (value, amount) => Expression::Add(Box::new(value), amount),
    }
}

fn low(value: Expression) -> Expression {
    match value {
        value @ (Expression::ReadByte(_) | Expression::SumByte(..)) => value,
        Expression::ReplaceLowByte(_, value) => *value,
        value => Expression::LowByte(Box::new(value)),
    }
}

fn address(i: &Instruction, values: &[Expression; 8]) -> Option<Address> {
    if !plain_memory(i) {
        return None;
    }
    let r = register(i.memory_base())?;
    let (base, offset) = match &values[r.index() as usize] {
        Expression::Initial(base) => (*base, 0),
        Expression::Add(base, offset) => {
            let Expression::Initial(base) = **base else {
                return None;
            };
            (base, *offset)
        }
        _ => return None,
    };
    Some(Address {
        base,
        offset: offset.wrapping_add(i.memory_displacement32()),
    })
}

struct Checker {
    values: [Expression; 8],
    flags: Vec<FlagEffect>,
    reads: Vec<Address>,
    store: Option<(Address, Expression)>,
}

impl Checker {
    fn new() -> Self {
        Self {
            values: std::array::from_fn(|r| Expression::Initial(Register(r as u8))),
            flags: Vec::new(),
            reads: Vec::new(),
            store: None,
        }
    }

    fn read(&mut self, i: &Instruction) -> Result<Expression, Rejection> {
        let at = i.ip32();
        // A read after a store needs an alias-aware store chain. It is outside
        // this rule, including when two different pointer registers may alias.
        if self.store.is_some() {
            return Err(unsupported(at, "memory read after a write"));
        }
        let a = address(i, &self.values).ok_or_else(|| unsupported(at, "memory address"))?;
        self.reads.push(a.clone());
        Ok(Expression::ReadByte(a))
    }

    fn store(&mut self, i: &Instruction, value: Expression) -> Result<(), Rejection> {
        let at = i.ip32();
        let a = address(i, &self.values).ok_or_else(|| unsupported(at, "store address"))?;
        if self.store.replace((a, low(value))).is_some() {
            return Err(unsupported(at, "more than one memory write per step"));
        }
        Ok(())
    }

    fn instruction(&mut self, i: &Instruction) -> Result<(), Rejection> {
        let at = i.ip32();
        let bad = || unsupported(at, "instruction form");
        if i.has_lock_prefix() || i.has_rep_prefix() || i.has_repne_prefix() {
            return Err(bad());
        }
        match i.mnemonic() {
            Mnemonic::Nop if i.op_count() == 0 => {}
            Mnemonic::Movzx
                if i.op0_kind() == OpKind::Register
                    && i.op0_register().size() == 4
                    && i.op1_kind() == OpKind::Memory
                    && i.memory_size().size() == 1 =>
            {
                let r = register(i.op0_register()).ok_or_else(bad)?;
                let v = self.read(i)?;
                self.values[r.index() as usize] = v;
            }
            Mnemonic::Mov
                if i.op0_kind() == OpKind::Register
                    && i.op0_register().size() == 1
                    && i.op1_kind() == OpKind::Memory
                    && i.memory_size().size() == 1 =>
            {
                let r = register(i.op0_register()).ok_or_else(bad)?;
                let v = self.read(i)?;
                let old = self.values[r.index() as usize].clone();
                self.values[r.index() as usize] =
                    Expression::ReplaceLowByte(Box::new(old), Box::new(v));
            }
            Mnemonic::Mov
                if i.op0_kind() == OpKind::Memory
                    && i.op1_kind() == OpKind::Register
                    && i.op1_register().size() == 1
                    && i.memory_size().size() == 1 =>
            {
                let r = register(i.op1_register()).ok_or_else(bad)?;
                self.store(i, self.values[r.index() as usize].clone())?;
            }
            Mnemonic::Add
                if i.op0_kind() == OpKind::Memory
                    && i.op1_kind() == OpKind::Register
                    && i.op1_register().size() == 1
                    && i.memory_size().size() == 1 =>
            {
                let r = register(i.op1_register()).ok_or_else(bad)?;
                let left = self.read(i)?;
                let right = low(self.values[r.index() as usize].clone());
                self.flags.push(FlagEffect {
                    arithmetic: Arithmetic::Add8,
                    left: left.clone(),
                    right: Some(right.clone()),
                });
                self.store(i, Expression::SumByte(Box::new(left), Box::new(right)))?;
            }
            Mnemonic::Inc | Mnemonic::Dec
                if i.op0_kind() == OpKind::Register && i.op0_register().size() == 4 =>
            {
                let r = register(i.op0_register()).ok_or_else(bad)?;
                let value = self.values[r.index() as usize].clone();
                self.flags.push(FlagEffect {
                    arithmetic: if i.mnemonic() == Mnemonic::Inc {
                        Arithmetic::Increment32
                    } else {
                        Arithmetic::Decrement32
                    },
                    left: value.clone(),
                    right: None,
                });
                self.values[r.index() as usize] = add(
                    value,
                    if i.mnemonic() == Mnemonic::Inc {
                        1
                    } else {
                        u32::MAX
                    },
                );
            }
            _ => return Err(bad()),
        }
        Ok(())
    }
}

/// Re-decode a candidate and establish the forward-loop induction rule.
/// Current rules cover one-byte LZ77 and PNG Sub/Up recurrences; other
/// computations return Unsupported, rather than obtaining weaker admission.
pub fn verify(
    image: &ImageSnapshot,
    candidate: CandidateBinding,
    budget: &mut Budget,
) -> Result<VerifiedBinding, Rejection> {
    let at = candidate.entry;
    if image.identity != candidate.image {
        return Err(counterexample(at, "image identity"));
    }
    if candidate.contract != ReferenceContract::get(candidate.contract.computation) {
        return Err(counterexample(at, "reference contract identity"));
    }
    if !matches!(
        candidate.contract.computation,
        Computation::Lz77 | Computation::PngSub | Computation::PngUp
    ) {
        return Err(unsupported(at, "reference recurrence"));
    }
    let len = candidate
        .end
        .checked_sub(at)
        .filter(|&len| len != 0)
        .ok_or_else(|| counterexample(at, "region extent"))?;
    let bytes = image
        .bytes(at, len)
        .ok_or_else(|| counterexample(at, "incomplete code dependency"))?;
    if !image.dependencies.iter().any(|d| {
        d.kind == DependencyKind::Code
            && at
                .checked_sub(d.offset)
                .and_then(|start| start.checked_add(len))
                .is_some_and(|end| end as usize <= d.bytes.len())
            && d.bytes
                .get((at - d.offset) as usize..(at - d.offset + len) as usize)
                == Some(bytes)
    }) {
        return Err(counterexample(at, "region is not code"));
    }
    let m = &candidate.mapping;
    if m.source == m.destination || m.source == m.remaining || m.destination == m.remaining {
        return Err(counterexample(at, "loop roles overlap"));
    }
    let source = Address {
        base: m.source,
        offset: m.source_offset,
    };
    let destination = Address {
        base: m.destination,
        offset: m.destination_offset,
    };
    let mut decoder = Decoder::with_ip(32, bytes, u64::from(at), DecoderOptions::NONE);
    let mut checker = Checker::new();
    let mut count = 0;
    let mut backedge = false;
    while decoder.can_decode() {
        if budget.instructions == 0 {
            return Err(Rejection::Timeout);
        }
        budget.instructions -= 1;
        let i = decoder.decode();
        count += 1;
        if i.is_invalid() {
            return Err(unsupported(i.ip32(), "invalid instruction"));
        }
        if i.mnemonic() == Mnemonic::Jne {
            if i.op0_kind() != OpKind::NearBranch32 {
                return Err(unsupported(i.ip32(), "nonrelocatable branch width"));
            }
            if decoder.can_decode() || i.near_branch_target() != u64::from(at) {
                return Err(counterexample(i.ip32(), "unaccounted control-flow edge"));
            }
            backedge = true;
            break;
        }
        checker.instruction(&i)?;
    }
    if !backedge {
        return Err(counterexample(at, "missing loop backedge"));
    }
    let expected_flag = FlagEffect {
        arithmetic: Arithmetic::Decrement32,
        left: Expression::Initial(m.remaining),
        right: None,
    };
    if checker.flags.last() != Some(&expected_flag) {
        return Err(counterexample(
            at,
            "loop test is not the decreasing counter",
        ));
    }
    for (r, delta) in [(m.source, 1), (m.destination, 1), (m.remaining, u32::MAX)] {
        if checker.values[r.index() as usize] != add(Expression::Initial(r), delta) {
            return Err(counterexample(at, "loop state recurrence"));
        }
    }
    let copied = Expression::ReadByte(source.clone());
    let expected = if candidate.contract.computation == Computation::Lz77 {
        copied
    } else {
        Expression::SumByte(
            Box::new(Expression::ReadByte(destination.clone())),
            Box::new(copied),
        )
    };
    if checker.store != Some((destination.clone(), expected.clone())) {
        return Err(counterexample(at, "byte recurrence"));
    }
    if checker
        .reads
        .iter()
        .any(|a| *a != source && *a != destination)
    {
        return Err(counterexample(at, "undeclared memory input"));
    }
    // Every non-loop register is either invariant or receives one byte, with
    // its high bits invariant. No hidden loop-carried temporary is admitted.
    for r in 0..8 {
        let r = Register(r);
        if [m.source, m.destination, m.remaining].contains(&r) {
            continue;
        }
        let initial = Expression::Initial(r);
        let read = Expression::ReadByte(source.clone());
        let low_write =
            Expression::ReplaceLowByte(Box::new(initial.clone()), Box::new(read.clone()));
        if checker.values[r.index() as usize] != initial
            && checker.values[r.index() as usize] != read
            && checker.values[r.index() as usize] != low_write
        {
            return Err(unsupported(at, "unproven loop-carried register"));
        }
    }
    let transition = Transition {
        registers: checker.values,
        flags: checker.flags,
        destination,
        value: expected,
        instructions: count,
    };
    let conditions = EntryConditions {
        positive_count: true,
        nonwrapping_ranges: true,
        ordinary_readable_source: true,
        ordinary_readable_writable_destination: true,
        destination_disjoint_dependencies: true,
        no_instruction_observers: true,
        aliasing: if candidate.contract.computation == Computation::PngUp {
            Aliasing::Disjoint
        } else {
            Aliasing::ForwardHistory
        },
    };
    Ok(VerifiedBinding {
        candidate,
        dependencies: vec![Dependency {
            offset: at,
            bytes: bytes.into(),
            kind: DependencyKind::Code,
        }],
        transition,
        conditions,
    })
}
