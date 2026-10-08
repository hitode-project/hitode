//! Integer x86 semantics, with no dependency on the existing emitter caches.
use super::ir::*;
use iced_x86::{Code, ConditionCode, Instruction, Mnemonic as M, OpKind, Register as R};
use std::collections::BTreeMap;

pub fn classify(i: &Instruction) -> Result<(), Reject> {
    if i.is_invalid() {
        return Err(Reject::Invalid);
    }
    if i.has_lock_prefix() {
        return Err(Reject::Atomic);
    }
    if i.has_repne_prefix() || i.has_rep_prefix() && i.mnemonic() != M::Ret {
        return Err(Reject::Prefix);
    }
    if !matches!(i.segment_prefix(), R::None | R::DS | R::SS | R::ES) {
        return Err(Reject::Segment);
    }
    for r in [i.memory_base(), i.memory_index()] {
        if r != R::None && !r.is_gpr32() {
            return Err(Reject::AddressSize);
        }
    }
    if i.memory_displ_size() == 2 && (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory) {
        return Err(Reject::AddressSize);
    }
    for n in 0..i.op_count() {
        match i.op_kind(n) {
            OpKind::Register if register(i.op_register(n)).is_none() => {
                return Err(Reject::Register);
            }
            OpKind::Memory
                if i.mnemonic() != M::Lea && !matches!(i.memory_size().size(), 1 | 2 | 4) =>
            {
                return Err(Reject::Width);
            }
            OpKind::Register
            | OpKind::Memory
            | OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::NearBranch32 => {}
            _ => return Err(Reject::Form),
        }
    }
    if i.mnemonic() == M::Xchg && (0..i.op_count()).any(|n| i.op_kind(n) == OpKind::Memory) {
        return Err(Reject::Atomic);
    }
    match i.mnemonic() {
        M::Mov
        | M::Movzx
        | M::Movsx
        | M::Lea
        | M::Xchg
        | M::Add
        | M::Adc
        | M::Sub
        | M::Sbb
        | M::Cmp
        | M::Inc
        | M::Dec
        | M::Neg
        | M::And
        | M::Or
        | M::Xor
        | M::Not
        | M::Test
        | M::Shl
        | M::Sal
        | M::Shr
        | M::Sar
        | M::Rol
        | M::Ror
        | M::Cdq
        | M::Cwde
        | M::Cwd
        | M::Cbw => Ok(()),
        M::Imul if i.op_count() > 1 || operand_width(i, 0) == Width::B32 => Ok(()),
        M::Mul if operand_width(i, 0) == Width::B32 => Ok(()),
        M::Pushf | M::Pushfd => Ok(()),
        M::Push | M::Pop if i.stack_pointer_increment().unsigned_abs() <= 4 => Ok(()),
        M::Call if matches!(i.code(), Code::Call_rel32_32 | Code::Call_rm32) => Ok(()),
        M::Jmp
            if matches!(
                i.code(),
                Code::Jmp_rel8_32 | Code::Jmp_rel32_32 | Code::Jmp_rm32
            ) =>
        {
            Ok(())
        }
        M::Ret if matches!(i.code(), Code::Retnd | Code::Retnd_imm16) => Ok(()),
        M::Nop if i.op_count() == 0 => Ok(()),
        _ if i.is_jcc_short_or_near()
            || i.condition_code() != ConditionCode::None
                && matches!(i.op0_kind(), OpKind::Register | OpKind::Memory) =>
        {
            Ok(())
        }
        _ => Err(Reject::Instruction),
    }
}

fn width(bytes: usize) -> Width {
    match bytes {
        1 => Width::B8,
        2 => Width::B16,
        4 => Width::B32,
        _ => Width::B32,
    }
}
fn register(r: R) -> Option<(usize, Width, u8)> {
    let full = r.full_register32();
    if !(R::EAX..=R::EDI).contains(&full) || !matches!(r.size(), 1 | 2 | 4) {
        return None;
    }
    Some((
        full as usize - R::EAX as usize,
        width(r.size()),
        if (R::AH..=R::BH).contains(&r) { 8 } else { 0 },
    ))
}
fn operand_width(i: &Instruction, n: u32) -> Width {
    match i.op_kind(n) {
        OpKind::Register => width(i.op_register(n).size()),
        OpKind::Memory => width(i.memory_size().size()),
        _ => Width::B32,
    }
}

pub struct Lifted {
    pub region: Region,
    pub supported: usize,
    pub rejected: BTreeMap<Reject, usize>,
    pub inlined_calls: usize,
    pub call_boundaries: BTreeMap<super::cfg::Boundary, usize>,
}

/// Discover bounded call contexts, then build guest basic blocks with explicit
/// instruction commits. State is threaded directly within each basic block.
pub fn lift(
    instructions: &BTreeMap<u32, Instruction>,
    start: u32,
) -> Result<Lifted, super::verify::Error> {
    lift_with_limits(instructions, start, super::cfg::Limits::default())
}
pub fn lift_with_limits(
    instructions: &BTreeMap<u32, Instruction>,
    start: u32,
    limits: super::cfg::Limits,
) -> Result<Lifted, super::verify::Error> {
    let plan = super::cfg::discover(instructions, start, limits)?;
    let mut region = Region {
        entry: plan.entry,
        blocks: vec![],
        values: vec![],
        memory: vec![],
        commits: vec![],
        snapshots: vec![],
        recovery: vec![],
        dependencies: vec![],
        semantic_version: 1,
        inline_limit: limits.depth,
        inline_budget: limits.instructions.saturating_sub(instructions.len()),
    };
    for (n, planned) in plan.blocks.iter().enumerate() {
        let block = BlockId(n);
        let parameters: Vec<_> = (0..10)
            .map(|index| {
                let id = ValueId(region.values.len());
                region.values.push(Value {
                    width: if index == 9 { Width::Token } else { Width::B32 },
                    kind: ValueKind::Parameter { block, index },
                });
                id
            })
            .collect();
        let state = StateMap {
            gpr: std::array::from_fn(|n| parameters[n]),
            flags: parameters[8],
        };
        region.blocks.push(Block {
            entry_state: state.clone(),
            entry_token: parameters[9],
            inline_depth: planned.depth,
            parameters: parameters.clone(),
            operations: vec![],
            terminator: Terminator::Exit(Exit {
                state,
                pc: parameters[0],
                token: parameters[9],
                reason: ExitReason::Complete,
            }),
        });
    }
    for (n, planned) in plan.blocks.iter().enumerate() {
        let block = BlockId(n);
        let state = region.blocks[n].entry_state.clone();
        let token = region.blocks[n].entry_token;
        let mut b = Builder {
            region: &mut region,
            map: &planned.targets,
            block,
            state,
            token,
            point: CommitId(0),
            snapshot: SnapshotId(0),
            expected_return: planned.expected_return,
        };
        for &pc in &planned.instructions {
            let i = &instructions[&pc];
            if let Err(reason) = classify(i) {
                let value = b.constant(Width::B32, u64::from(pc));
                b.finish(Terminator::Exit(
                    b.exit(value, ExitReason::Unsupported(reason)),
                ));
                break;
            }
            b.point = CommitId(b.region.commits.len());
            b.snapshot = SnapshotId(b.region.snapshots.len());
            let dep = DependencyId(b.region.dependencies.len());
            b.region.dependencies.push(Dependency {
                start: pc,
                length: i.len() as u32,
                version: None,
            });
            b.region.commits.push(CommitPoint {
                instruction: b.point.0 as u32,
                pc,
                next_pc: i.next_ip32(),
                units: 1,
                subprogress: 0,
            });
            b.region.snapshots.push(Snapshot {
                state: b.state.clone(),
                commit: b.point,
                recipes: vec![],
                private_slots: vec![],
                dependencies: vec![dep],
                lease: None,
            });
            b.instruction(i);
        }
    }
    let mut supported = 0;
    let mut rejected = BTreeMap::new();
    for i in instructions.values() {
        match classify(i) {
            Ok(()) => supported += 1,
            Err(reason) => *rejected.entry(reason).or_default() += 1,
        }
    }
    super::state_ssa::compact(&mut region);
    region.recovery = region
        .snapshots
        .iter()
        .map(|s| super::recovery::Dag::build(&region, &s.state))
        .collect();
    super::verify::verify(&region)?;
    Ok(Lifted {
        region,
        supported,
        rejected,
        inlined_calls: plan.inlined_calls,
        call_boundaries: plan.call_boundaries,
    })
}

struct Builder<'a> {
    region: &'a mut Region,
    map: &'a BTreeMap<u32, BlockId>,
    block: BlockId,
    state: StateMap,
    token: ValueId,
    point: CommitId,
    snapshot: SnapshotId,
    expected_return: Option<u32>,
}
impl Builder<'_> {
    fn value(&mut self, width: Width, kind: ValueKind) -> ValueId {
        let id = ValueId(self.region.values.len());
        self.region.values.push(Value { width, kind });
        id
    }
    fn define(&mut self, width: Width, kind: ValueKind) -> ValueId {
        let id = self.value(width, kind);
        self.op(Operation::Define(id));
        id
    }
    fn op(&mut self, op: Operation) {
        self.region.blocks[self.block.0].operations.push(op);
    }
    fn constant(&mut self, width: Width, n: u64) -> ValueId {
        self.define(width, ValueKind::Constant(n & width.mask()))
    }
    fn pure(&mut self, width: Width, p: Pure) -> ValueId {
        self.define(width, ValueKind::Pure(p))
    }
    fn binary(&mut self, width: Width, kind: Binary, a: ValueId, b: ValueId) -> ValueId {
        self.pure(width, Pure::Binary(kind, a, b))
    }
    fn cast(&mut self, value: ValueId, width: Width, signed: bool) -> ValueId {
        let old = self.region.values[value.0].width;
        if old == width {
            value
        } else if old.bits() > width.bits() {
            self.pure(width, Pure::Extract { value, lsb: 0 })
        } else {
            self.pure(width, Pure::Extend { value, signed })
        }
    }
    fn reg(&mut self, r: R) -> ValueId {
        let (index, w, lsb) = register(r).unwrap();
        let value = self.state.gpr[index];
        if w == Width::B32 {
            value
        } else {
            self.pure(w, Pure::Extract { value, lsb })
        }
    }
    fn set_reg(&mut self, r: R, value: ValueId) {
        let (index, w, lsb) = register(r).unwrap();
        let value = self.cast(value, w, false);
        self.state.gpr[index] = if w == Width::B32 {
            value
        } else {
            self.pure(
                Width::B32,
                Pure::Insert {
                    base: self.state.gpr[index],
                    value,
                    lsb,
                },
            )
        };
    }
    fn address(&mut self, i: &Instruction) -> ValueId {
        let mut a = self.constant(Width::B32, u64::from(i.memory_displacement32()));
        if i.memory_base() != R::None {
            let base = self.reg(i.memory_base());
            a = self.binary(Width::B32, Binary::Add, a, base);
        }
        if i.memory_index() != R::None {
            let index = self.reg(i.memory_index());
            let scale = self.constant(Width::B32, u64::from(i.memory_index_scale()));
            let offset = self.binary(Width::B32, Binary::Mul, index, scale);
            a = self.binary(Width::B32, Binary::Add, a, offset);
        }
        a
    }
    fn memory(&mut self, address: ValueId, width: Width, access: Access) -> Option<ValueId> {
        let id = MemoryId(self.region.memory.len());
        let result = (access == Access::Read).then(|| self.value(width, ValueKind::Load(id)));
        let output = self.value(Width::Token, ValueKind::Effect(id));
        self.region.memory.push(MemoryOp {
            address,
            width,
            access,
            alias: AliasDomain::Guest,
            input: self.token,
            output,
            result,
            commit: self.point,
            snapshot: self.snapshot,
            proof: None,
        });
        self.op(Operation::Memory(id));
        self.token = output;
        result
    }
    fn read(&mut self, i: &Instruction, n: u32, w: Width) -> ValueId {
        match i.op_kind(n) {
            OpKind::Register => self.reg(i.op_register(n)),
            OpKind::Memory => {
                let address = self.address(i);
                self.memory(address, operand_width(i, n), Access::Read)
                    .unwrap()
            }
            OpKind::NearBranch32 => self.constant(Width::B32, i.near_branch_target()),
            _ => self.constant(w, i.immediate(n)),
        }
    }
    fn write(&mut self, i: &Instruction, n: u32, value: ValueId, enabled: Option<ValueId>) {
        if i.op_kind(n) == OpKind::Register {
            self.set_reg(i.op_register(n), value);
        } else {
            let address = self.address(i);
            self.memory(
                address,
                operand_width(i, n),
                Access::Write { value, enabled },
            );
        }
    }
    fn flags(
        &mut self,
        kind: FlagKind,
        defined: u32,
        width: Width,
        lhs: ValueId,
        rhs: ValueId,
        carry: ValueId,
        result: ValueId,
    ) {
        self.state.flags = self.define(
            Width::B32,
            ValueKind::Flags(Flags {
                kind,
                defined,
                width,
                lhs,
                rhs,
                carry,
                result,
                preserved: self.state.flags,
            }),
        );
    }
    fn condition(&mut self, i: &Instruction) -> ValueId {
        self.pure(
            Width::B1,
            Pure::Condition {
                flags: self.state.flags,
                code: i.condition_code(),
            },
        )
    }
    fn exit(&self, pc: ValueId, reason: ExitReason) -> Exit {
        Exit {
            state: self.state.clone(),
            pc,
            token: self.token,
            reason,
        }
    }
    fn target(&mut self, pc: u32) -> Target {
        if let Some(&block) = self.map.get(&pc) {
            Target::Block(Edge {
                block,
                arguments: self.state.values().chain([self.token]).collect(),
            })
        } else {
            let value = self.constant(Width::B32, u64::from(pc));
            Target::Exit(self.exit(value, ExitReason::Complete))
        }
    }
    fn finish(&mut self, term: Terminator) {
        self.region.blocks[self.block.0].terminator = term;
    }
    fn commit(&mut self, next_pc: ValueId) {
        self.op(Operation::Commit {
            point: self.point,
            state: self.state.clone(),
            next_pc,
        });
    }

    fn instruction(&mut self, i: &Instruction) {
        let w = operand_width(i, 0);
        let zero = self.constant(w, 0);
        let mut next = self.constant(Width::B32, u64::from(i.next_ip32()));
        let mut branch = None;
        let mut direct = None;
        let mut indirect = false;
        match i.mnemonic() {
            M::Nop => {}
            M::Mov | M::Movzx | M::Movsx => {
                let a = self.read(i, 1, w);
                let a = self.cast(a, w, i.mnemonic() == M::Movsx);
                self.write(i, 0, a, None);
            }
            M::Lea => {
                let a = self.address(i);
                self.write(i, 0, a, None);
            }
            M::Xchg => {
                let a = self.read(i, 0, w);
                let b = self.read(i, 1, w);
                self.write(i, 0, b, None);
                self.write(i, 1, a, None);
            }
            M::Add
            | M::Adc
            | M::Sub
            | M::Sbb
            | M::Cmp
            | M::Inc
            | M::Dec
            | M::Neg
            | M::And
            | M::Or
            | M::Xor
            | M::Test
            | M::Not => {
                let original = self.read(i, 0, w);
                let unary = i.op_count() == 1;
                let rhs = if !unary {
                    self.read(i, 1, w)
                } else if i.mnemonic() == M::Neg {
                    original
                } else {
                    self.constant(w, if i.mnemonic() == M::Not { w.mask() } else { 1 })
                };
                let lhs = if i.mnemonic() == M::Neg {
                    zero
                } else {
                    original
                };
                let carry = if matches!(i.mnemonic(), M::Adc | M::Sbb) {
                    let bit = self.pure(
                        Width::B1,
                        Pure::Extract {
                            value: self.state.flags,
                            lsb: 0,
                        },
                    );
                    self.cast(bit, w, false)
                } else {
                    zero
                };
                let (binary, kind, mut mask) = match i.mnemonic() {
                    M::Add | M::Adc | M::Inc => (Binary::Add, FlagKind::Add, ARITHMETIC),
                    M::Sub | M::Sbb | M::Cmp | M::Dec | M::Neg => {
                        (Binary::Sub, FlagKind::Sub, ARITHMETIC)
                    }
                    M::And | M::Test => (Binary::And, FlagKind::Logic, ARITHMETIC & !AF),
                    M::Or => (Binary::Or, FlagKind::Logic, ARITHMETIC & !AF),
                    _ => (Binary::Xor, FlagKind::Logic, ARITHMETIC & !AF),
                };
                let mut result = self.binary(w, binary, lhs, rhs);
                if matches!(i.mnemonic(), M::Adc | M::Sbb) {
                    result = self.binary(w, binary, result, carry);
                }
                if matches!(i.mnemonic(), M::Inc | M::Dec) {
                    mask &= !CF;
                }
                if i.mnemonic() != M::Not {
                    self.flags(kind, mask, w, lhs, rhs, carry, result);
                }
                if !matches!(i.mnemonic(), M::Cmp | M::Test) {
                    self.write(i, 0, result, None);
                }
            }
            M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror => {
                let a = self.read(i, 0, w);
                let count = self.read(i, 1, Width::B8);
                let mask = self.constant(Width::B8, 31);
                let count = self.binary(Width::B8, Binary::And, count, mask);
                let (binary, kind) = match i.mnemonic() {
                    M::Shl | M::Sal => (Binary::Shl, FlagKind::Shl),
                    M::Shr => (Binary::Shr, FlagKind::Shr),
                    M::Sar => (Binary::Sar, FlagKind::Sar),
                    M::Rol => (Binary::Rol, FlagKind::Rol),
                    _ => (Binary::Ror, FlagKind::Ror),
                };
                let result = self.binary(w, binary, a, count);
                self.flags(
                    kind,
                    if matches!(kind, FlagKind::Rol | FlagKind::Ror) {
                        CF | OF
                    } else {
                        ARITHMETIC & !AF
                    },
                    w,
                    a,
                    count,
                    zero,
                    result,
                );
                let enabled = self.pure(Width::B1, Pure::Nonzero(count));
                self.write(i, 0, result, Some(enabled));
            }
            M::Mul | M::Imul => {
                let (a, b) = if i.op_count() == 1 {
                    (self.reg(R::EAX), self.read(i, 0, w))
                } else if i.op_count() == 2 {
                    (self.read(i, 0, w), self.read(i, 1, w))
                } else {
                    (self.read(i, 1, w), self.read(i, 2, w))
                };
                let signed = i.mnemonic() == M::Imul;
                let aw = self.cast(a, Width::B64, signed);
                let bw = self.cast(b, Width::B64, signed);
                let product = self.binary(Width::B64, Binary::Mul, aw, bw);
                let low = self.cast(product, w, false);
                self.flags(
                    if signed {
                        FlagKind::SignedMultiply
                    } else {
                        FlagKind::UnsignedMultiply
                    },
                    CF | OF,
                    w,
                    a,
                    b,
                    zero,
                    low,
                );
                if i.op_count() == 1 {
                    let high = self.pure(
                        w,
                        Pure::Extract {
                            value: product,
                            lsb: w.bits() as u8,
                        },
                    );
                    self.set_reg(R::EAX, low);
                    self.set_reg(R::EDX, high);
                } else {
                    self.write(i, 0, low, None);
                }
            }
            M::Push | M::Pushf | M::Pushfd => {
                let sw = width(i.stack_pointer_increment().unsigned_abs() as usize);
                let value = if i.mnemonic() == M::Push {
                    self.read(i, 0, sw)
                } else {
                    let mask = self.constant(Width::B32, 0xfffc_ffff);
                    let flags = self.binary(Width::B32, Binary::And, self.state.flags, mask);
                    self.cast(flags, sw, false)
                };
                let size = self.constant(Width::B32, u64::from(sw.bits() / 8));
                let address = self.binary(Width::B32, Binary::Sub, self.state.gpr[4], size);
                self.memory(
                    address,
                    sw,
                    Access::Write {
                        value,
                        enabled: None,
                    },
                );
                self.state.gpr[4] = address;
            }
            M::Pop => {
                let sw = width(i.stack_pointer_increment().unsigned_abs() as usize);
                let value = self.memory(self.state.gpr[4], sw, Access::Read).unwrap();
                let size = self.constant(Width::B32, u64::from(sw.bits() / 8));
                self.state.gpr[4] = self.binary(Width::B32, Binary::Add, self.state.gpr[4], size);
                self.write(i, 0, value, None);
            }
            M::Call | M::Jmp => {
                next = self.read(i, 0, Width::B32);
                if i.mnemonic() == M::Call {
                    let size = self.constant(Width::B32, 4);
                    let address = self.binary(Width::B32, Binary::Sub, self.state.gpr[4], size);
                    let ret = self.constant(Width::B32, u64::from(i.next_ip32()));
                    self.memory(
                        address,
                        Width::B32,
                        Access::Write {
                            value: ret,
                            enabled: None,
                        },
                    );
                    self.state.gpr[4] = address;
                }
                if i.op0_kind() == OpKind::NearBranch32 {
                    direct = Some(i.near_branch32());
                } else {
                    indirect = true;
                }
            }
            M::Ret => {
                next = self
                    .memory(self.state.gpr[4], Width::B32, Access::Read)
                    .unwrap();
                let size = self.constant(
                    Width::B32,
                    4 + if i.op_count() == 1 {
                        u64::from(i.immediate16())
                    } else {
                        0
                    },
                );
                self.state.gpr[4] = self.binary(Width::B32, Binary::Add, self.state.gpr[4], size);
                indirect = true;
            }
            M::Cdq | M::Cwd | M::Cwde | M::Cbw => {
                let (source, dest) = match i.mnemonic() {
                    M::Cbw => (R::AL, R::AX),
                    M::Cwde => (R::AX, R::EAX),
                    M::Cwd => (R::AX, R::DX),
                    _ => (R::EAX, R::EDX),
                };
                let a = self.reg(source);
                let w = width(dest.size());
                let result = if matches!(i.mnemonic(), M::Cdq | M::Cwd) {
                    let count = self.constant(Width::B8, u64::from(w.bits() - 1));
                    self.binary(w, Binary::Sar, a, count)
                } else {
                    self.cast(a, w, true)
                };
                self.set_reg(dest, result);
            }
            _ => {
                let condition = self.condition(i);
                if i.is_jcc_short_or_near() {
                    let target = self.constant(Width::B32, u64::from(i.near_branch32()));
                    next = self.pure(
                        Width::B32,
                        Pure::Select {
                            condition,
                            yes: target,
                            no: next,
                        },
                    );
                    branch = Some(condition);
                } else if i.op_count() == 1 {
                    let value = self.cast(condition, Width::B8, false);
                    self.write(i, 0, value, None);
                } else {
                    let old = self.read(i, 0, w);
                    let value = self.read(i, 1, w);
                    let selected = self.pure(
                        w,
                        Pure::Select {
                            condition,
                            yes: value,
                            no: old,
                        },
                    );
                    self.write(i, 0, selected, None);
                }
            }
        }
        self.commit(next);
        let term = if let Some(condition) = branch {
            Terminator::Branch {
                condition,
                taken: self.target(i.near_branch32()),
                fallthrough: self.target(i.next_ip32()),
            }
        } else if i.mnemonic() == M::Ret && self.expected_return.is_some() {
            let expected = self.expected_return.unwrap();
            match self.target(expected) {
                Target::Block(matched) => Terminator::Return {
                    actual: next,
                    expected,
                    matched,
                    mismatch: self.exit(next, ExitReason::Indirect),
                },
                Target::Exit(_) => Terminator::Exit(self.exit(next, ExitReason::Indirect)),
            }
        } else if indirect {
            Terminator::Exit(self.exit(next, ExitReason::Indirect))
        } else if let Some(pc) = direct {
            let target = self.target(pc);
            if i.mnemonic() == M::Call {
                Terminator::Call {
                    callee: target,
                    return_pc: i.next_ip32(),
                }
            } else {
                Terminator::Jump(target)
            }
        } else {
            Terminator::Jump(self.target(i.next_ip32()))
        };
        self.finish(term);
    }
}
