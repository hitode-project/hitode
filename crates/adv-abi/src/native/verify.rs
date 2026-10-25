//! Independent admission of positive-count, forward byte recurrences.
//!
//! The checker re-decodes code through the shared integer semantics. Each step
//! has typed values, ordered memory versions and complete continuation/final
//! exits. Entry predicates remain obligations for the publication bridge.

use adv_cpu::integer::{self, Operand, Operation};
use iced_x86::{
    ConditionCode, Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register as X86,
};

use super::expression::Kind;
use super::*;

pub const RULE_VERSION: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    pub before: MemoryVersion,
    pub after: MemoryVersion,
    pub address: Expression,
    pub value: Expression,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitState {
    pub registers: [Expression; 8],
    pub flags: Expression,
    pub memory: MemoryVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    Continue,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionExit {
    pub kind: ExitKind,
    pub target: u32,
    pub condition: Expression,
    pub state: ExitState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub store: Store,
    pub exits: Vec<RegionExit>,
    pub instructions: u32,
}

/// Only the independent checker can construct this capability.
///
/// ~~~compile_fail
/// use adv_abi::native::{CandidateBinding, VerifiedBinding};
/// fn bypass(candidate: CandidateBinding) -> VerifiedBinding { candidate.into() }
/// ~~~
#[derive(Clone, Debug)]
pub struct VerifiedBinding {
    candidate: CandidateBinding,
    dependencies: Vec<Dependency>,
    transition: Transition,
    conditions: Vec<Predicate>,
}

impl VerifiedBinding {
    /// Compile-time bridge inputs for the checked recurrence and scalar tail.
    /// # Safety
    /// `kernel` must implement this binding's reference contract, accepting
    /// source/destination/count under the returned aliasing rule.
    pub unsafe fn lower(
        &self,
        kernel: adv_cpu::native_region::Kernel,
    ) -> Option<adv_cpu::native_region::NativeRegion> {
        use adv_cpu::native_region::{Affine, Aliasing, NativeRegion};
        let source_role = match self.candidate.contract.computation {
            Computation::PngUp => Input::PreviousRow,
            Computation::PngSub => Input::ReconstructedPrefix,
            Computation::Lz77 => Input::History,
            _ => return None,
        };
        let view = &self.candidate.view;
        let mut inputs = [Affine {
            registers: [0; 8],
            constant: 0,
        }; 3];
        for (target, expression) in inputs.iter_mut().zip([
            view.input(source_role)?,
            &view.output.address,
            view.input(Input::Length)?,
        ]) {
            let (terms, constant) = expression.affine()?;
            for (target, term) in target.registers.iter_mut().zip(terms) {
                *target = match term {
                    0 => 0,
                    1 => 1,
                    u32::MAX => -1,
                    _ => return None,
                };
            }
            target.constant = constant;
        }
        let mut advances = [0; 8];
        for (r, value) in self.transition.exits[0].state.registers.iter().enumerate() {
            if let Some((terms, constant)) = value.affine() {
                let mut expected = [0; 8];
                expected[r] = 1;
                if terms != expected {
                    return None;
                }
                advances[r] = match constant {
                    0 => 0,
                    1 => 1,
                    u32::MAX => -1,
                    _ => return None,
                };
            }
        }
        if advances[4] != 0 || advances[5] != 0 {
            return None;
        }
        let aliasing = if self.candidate.contract.computation == Computation::PngUp {
            Aliasing::Disjoint
        } else {
            Aliasing::Backward
        };
        // SAFETY: admission checked the recurrence, complete effects, additive
        // cursor updates and scalar-tail restoration; the caller owns kernel identity.
        Some(unsafe {
            NativeRegion::new(
                inputs,
                advances,
                self.transition.instructions,
                aliasing,
                kernel,
            )
        })
    }

    pub fn candidate(&self) -> &CandidateBinding {
        &self.candidate
    }
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
    pub fn transition(&self) -> &Transition {
        &self.transition
    }
    pub fn conditions(&self) -> &[Predicate] {
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
    let r = integer::Reg::decode(r)?;
    (r.shift == 0 && matches!(r.width, 1 | 4)).then_some(Register(r.index))
}

pub(super) fn plain_memory(i: &Instruction) -> bool {
    integer::Address::decode(i, false).is_some_and(|a| {
        !a.word
            && !a.fs
            && a.base.is_none_or(|r| r.width == 4)
            && a.index.is_none_or(|r| r.width == 4)
            && a.scale == 1
            && matches!(i.segment_prefix(), X86::None | X86::DS)
    })
}

fn add(value: Expression, amount: u32) -> Expression {
    Expression::binary(
        Binary::Add,
        value,
        Expression::constant(Width::Dword, u64::from(amount)),
    )
    .expect("32-bit cursor")
}
fn low(value: Expression) -> Expression {
    Expression::extract(value, 0, Width::Byte).expect("byte or full register")
}
pub(super) fn address(i: &Instruction, values: &[Expression; 8]) -> Option<Expression> {
    if !plain_memory(i) {
        return None;
    }
    let a = integer::Address::decode(i, false)?;
    let mut value = Expression::constant(Width::Dword, u64::from(a.displacement));
    for r in [a.base, a.index].into_iter().flatten() {
        value = Expression::binary(Binary::Add, value, values[r.index as usize].clone())?;
    }
    Some(value)
}

struct Checker {
    values: [Expression; 8],
    flags: Expression,
    reads: Vec<Expression>,
    store: Option<Store>,
    defined: [u32; 8],
    initial_reads: [u32; 8],
}

impl Checker {
    fn new() -> Self {
        Self {
            values: std::array::from_fn(|r| Expression::register(Register(r as u8))),
            flags: Expression::flags(),
            reads: Vec::new(),
            store: None,
            defined: [0; 8],
            initial_reads: [0; 8],
        }
    }
    fn read_register(&mut self, r: integer::Reg) {
        let bits = integer::mask(r.width) << r.shift;
        self.initial_reads[r.index as usize] |= bits & !self.defined[r.index as usize];
    }
    fn write_register(&mut self, r: integer::Reg) {
        self.defined[r.index as usize] |= integer::mask(r.width) << r.shift;
    }
    fn address_registers(&mut self, i: &Instruction) {
        if let Some(a) = integer::Address::decode(i, false) {
            for r in [a.base, a.index].into_iter().flatten() {
                self.read_register(r);
            }
        }
    }
    fn read(&mut self, i: &Instruction) -> Result<Expression, Rejection> {
        let at = i.ip32();
        if self.store.is_some() {
            return Err(unsupported(at, "memory read after a write"));
        }
        let a = address(i, &self.values).ok_or_else(|| unsupported(at, "memory address"))?;
        self.address_registers(i);
        self.reads.push(a.clone());
        Ok(Expression::load(MemoryVersion(0), a, Width::Byte).expect("32-bit address"))
    }
    fn store(&mut self, i: &Instruction, value: Expression) -> Result<(), Rejection> {
        let at = i.ip32();
        let a = address(i, &self.values).ok_or_else(|| unsupported(at, "store address"))?;
        self.address_registers(i);
        if self
            .store
            .replace(Store {
                before: MemoryVersion(0),
                after: MemoryVersion(1),
                address: a,
                value: low(value),
            })
            .is_some()
        {
            return Err(unsupported(at, "more than one memory write per step"));
        }
        Ok(())
    }
    fn flags(&mut self, kind: Mnemonic, left: Expression, right: Expression) {
        self.flags = Expression::integer_flags(kind, self.flags.clone(), left, right)
            .expect("decoded integer operands");
    }
    fn instruction(&mut self, i: &Instruction) -> Result<(), Rejection> {
        let at = i.ip32();
        let bad = || unsupported(at, "instruction form");
        if i.has_lock_prefix() || i.has_rep_prefix() || i.has_repne_prefix() {
            return Err(bad());
        }
        match integer::decode(i).ok_or_else(bad)? {
            Operation::Nop if i.op_count() == 0 => {}
            Operation::Mov(Operand::Register(r), Operand::Memory(_, 1), false) if r.width == 4 => {
                let value = self.read(i)?;
                self.write_register(r);
                self.values[r.index as usize] =
                    Expression::extend(value, Width::Dword, false).expect("byte extension");
            }
            Operation::Mov(Operand::Register(r), Operand::Memory(_, 1), false)
                if r.width == 1 && r.shift == 0 =>
            {
                let value = self.read(i)?;
                self.write_register(r);
                self.values[r.index as usize] =
                    Expression::insert(self.values[r.index as usize].clone(), value, 0)
                        .expect("low-byte register write");
            }
            Operation::Mov(Operand::Memory(_, 1), Operand::Register(r), false)
                if r.width == 1 && r.shift == 0 =>
            {
                self.read_register(r);
                self.store(i, self.values[r.index as usize].clone())?;
            }
            Operation::Binary(Mnemonic::Add, Operand::Memory(_, 1), Operand::Register(r))
                if r.width == 1 && r.shift == 0 =>
            {
                self.read_register(r);
                let left = self.read(i)?;
                let right = low(self.values[r.index as usize].clone());
                self.flags(Mnemonic::Add, left.clone(), right.clone());
                self.store(
                    i,
                    Expression::binary(Binary::Add, left, right).expect("byte sum"),
                )?;
            }
            Operation::Unary(kind @ (Mnemonic::Inc | Mnemonic::Dec), Operand::Register(r))
                if r.width == 4 =>
            {
                self.read_register(r);
                self.write_register(r);
                let value = self.values[r.index as usize].clone();
                self.flags(kind, value.clone(), Expression::constant(Width::Dword, 1));
                self.values[r.index as usize] =
                    add(value, if kind == Mnemonic::Inc { 1 } else { u32::MAX });
            }
            Operation::Binary(Mnemonic::Cmp, Operand::Register(a), Operand::Register(b))
                if a.width == 4 && b.width == 4 =>
            {
                self.read_register(a);
                self.read_register(b);
                self.flags(
                    Mnemonic::Cmp,
                    self.values[a.index as usize].clone(),
                    self.values[b.index as usize].clone(),
                );
            }
            _ => return Err(bad()),
        }
        Ok(())
    }
}

/// Check actual code and establish the forward-loop induction rule.
/// Average/Paeth require additional proof rules and return Unsupported.
pub fn verify(
    image: &ImageSnapshot,
    candidate: CandidateBinding,
    budget: &mut Budget,
) -> Result<VerifiedBinding, Rejection> {
    let at = candidate.entry;
    if image.identity != candidate.image {
        return Err(counterexample(at, "image identity"));
    }
    let computation = candidate.contract.computation;
    if candidate.contract != ReferenceContract::get(computation) {
        return Err(counterexample(at, "reference contract identity"));
    }
    if !matches!(
        computation,
        Computation::Lz77 | Computation::PngSub | Computation::PngUp
    ) {
        return Err(unsupported(at, "reference recurrence"));
    }
    if candidate.cutpoints != [at] || candidate.exits != [candidate.end] {
        return Err(counterexample(at, "cutpoints or exits"));
    }
    let view = &candidate.view;
    let source_role = match computation {
        Computation::Lz77 => Input::History,
        Computation::PngSub => Input::ReconstructedPrefix,
        _ => Input::PreviousRow,
    };
    let source = view
        .input(source_role)
        .ok_or_else(|| counterexample(at, "source mapping"))?
        .clone();
    let destination = view.output.address.clone();
    let remaining = view
        .input(Input::Length)
        .ok_or_else(|| counterexample(at, "count mapping"))?
        .clone();
    if [source.clone(), destination.clone(), remaining.clone()]
        .iter()
        .any(|e| e.affine().is_none())
    {
        return Err(unsupported(at, "non-affine loop address or count"));
    }
    if byte_view(
        computation,
        source.clone(),
        destination.clone(),
        remaining.clone(),
    )
    .as_ref()
        != Some(view)
    {
        return Err(counterexample(at, "input mapping"));
    }
    if candidate.proof
        != (ProofHint::ForwardBytes {
            remaining: remaining.clone(),
        })
    {
        return Err(counterexample(at, "loop proof hint"));
    }
    let len = candidate
        .end
        .checked_sub(at)
        .filter(|&n| n != 0)
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
    let next_remaining = add(remaining.clone(), u32::MAX);
    let test = match checker.flags.kind() {
        Kind::IntegerFlags(Mnemonic::Dec, _, left, _) => *left == remaining,
        Kind::IntegerFlags(Mnemonic::Cmp, _, left, right) => {
            let difference = Expression::binary(Binary::Subtract, left.clone(), right.clone());
            let negative = Expression::binary(
                Binary::Subtract,
                Expression::constant(Width::Dword, 0),
                next_remaining.clone(),
            );
            difference.as_ref() == Some(&next_remaining) || difference == negative
        }
        _ => false,
    };
    if !test {
        return Err(counterexample(
            at,
            "loop test is not the decreasing counter",
        ));
    }
    for (value, delta) in [(&source, 1), (&destination, 1), (&remaining, u32::MAX)] {
        if value.after(&checker.values) != Some(add(value.clone(), delta)) {
            return Err(counterexample(at, "loop state recurrence"));
        }
    }
    let copied =
        Expression::load(MemoryVersion(0), source.clone(), Width::Byte).expect("byte source");
    let expected = if computation == Computation::Lz77 {
        copied.clone()
    } else {
        Expression::binary(
            Binary::Add,
            Expression::load(MemoryVersion(0), destination.clone(), Width::Byte)
                .expect("byte destination"),
            copied.clone(),
        )
        .expect("byte sum")
    };
    let expected_store = Store {
        before: MemoryVersion(0),
        after: MemoryVersion(1),
        address: destination.clone(),
        value: expected,
    };
    if checker.store.as_ref() != Some(&expected_store) {
        return Err(counterexample(at, "byte recurrence"));
    }
    if checker
        .reads
        .iter()
        .any(|a| *a != source && *a != destination)
    {
        return Err(counterexample(at, "undeclared memory input"));
    }
    for r in 0..8 {
        let r = Register(r);
        let initial = Expression::register(r);
        let extended =
            Expression::extend(copied.clone(), Width::Dword, false).expect("byte extension");
        let partial = Expression::insert(initial.clone(), copied.clone(), 0).expect("low byte");
        let actual = &checker.values[r.index() as usize];
        if let Some((terms, _)) = actual.affine()
            && terms == initial.affine().unwrap().0
        {
            continue;
        }
        if *actual != initial && *actual != extended && *actual != partial {
            return Err(unsupported(at, "unproven loop-carried register"));
        }
        let changed = if *actual == extended { u32::MAX } else { 0xff };
        if *actual != initial && checker.initial_reads[r.index() as usize] & changed != 0 {
            return Err(unsupported(
                at,
                "scalar tail reads a loop-carried temporary before defining it",
            ));
        }
    }
    let state = ExitState {
        registers: checker.values,
        flags: checker.flags,
        memory: MemoryVersion(1),
    };
    let exits = [
        (ExitKind::Continue, candidate.entry, ConditionCode::ne),
        (ExitKind::Complete, candidate.end, ConditionCode::e),
    ]
    .map(|(kind, target, cc)| RegionExit {
        kind,
        target,
        condition: Expression::condition(cc, state.flags.clone()).expect("flags condition"),
        state: state.clone(),
    })
    .into();
    let input = MemorySpan {
        address: source.clone(),
        length: remaining.clone(),
    };
    let output = candidate.view.output.clone();
    let mut conditions = vec![
        Predicate::Positive(remaining),
        Predicate::NoWrap(input.clone()),
        Predicate::NoWrap(output.clone()),
        Predicate::Ordinary {
            span: input.clone(),
            access: Access::Read,
        },
        Predicate::Ordinary {
            span: output.clone(),
            access: Access::ReadWrite,
        },
        Predicate::WritesOutsideDependencies(output.clone()),
        Predicate::NoInstructionObservers {
            entry: at,
            end: candidate.end,
        },
    ];
    if computation == Computation::PngUp {
        conditions.push(Predicate::Disjoint(input, output));
    } else {
        conditions.push(Predicate::BackwardDistance {
            distance: Expression::binary(Binary::Subtract, destination.clone(), source.clone())
                .expect("32-bit distance"),
            source,
            destination,
        });
    }
    Ok(VerifiedBinding {
        candidate,
        dependencies: vec![Dependency {
            offset: at,
            bytes: bytes.into(),
            kind: DependencyKind::Code,
        }],
        transition: Transition {
            store: expected_store,
            exits,
            instructions: count,
        },
        conditions,
    })
}
