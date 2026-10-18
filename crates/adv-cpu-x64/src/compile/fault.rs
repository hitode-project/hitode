//! First scalar fault site: a bounded pure-SSA recipe and same-frame epilogue.
use super::*;
use crate::host_fault::{
    Access, FaultSite,
    recipe::{Input, MAX_DEPTH, MAX_NODES, Node, Operation, Recipe, Source},
};
use cranelift_codegen::{
    LabelValueLoc,
    ir::{InstructionData, Opcode, ValueDef, ValueLabel},
};

pub(super) const LOAD_SOURCE: u32 = 1;
const LABEL_BASE: u32 = 100;

pub(super) fn candidate(start: u32, instructions: &BTreeMap<u32, Instruction>) -> Option<u32> {
    let mut pc = start;
    for _ in 0..32 {
        let instruction = instructions.get(&pc)?;
        if !supported(instruction) || instruction.flow_control() != FlowControl::Next {
            return None;
        }
        if matches!(instruction.mnemonic(), M::Mov | M::Movzx | M::Movsx)
            && instruction.op0_kind() == OpKind::Register
            && reg_index(instruction.op0_register()).is_some()
            && instruction.op1_kind() == OpKind::Memory
            && matches!(instruction.memory_size().size(), 1 | 2 | 4)
        {
            return Some(pc);
        }
        if instruction.mnemonic() != M::Lea
            && (0..instruction.op_count())
                .any(|operand| instruction.op_kind(operand) == OpKind::Memory)
        {
            return None;
        }
        if !matches!(
            instruction.mnemonic(),
            M::Mov
                | M::Lea
                | M::Add
                | M::Sub
                | M::Cmp
                | M::Test
                | M::And
                | M::Or
                | M::Xor
                | M::Inc
                | M::Dec
                | M::Neg
                | M::Not
                | M::Nop
        ) || instruction.op_count() > 0 && instruction.op0_register().size() != 4
        {
            return None;
        }
        pc = instruction.next_ip32();
    }
    None
}

pub(super) struct Draft {
    pub recipe: Recipe,
    values: Vec<Option<Value>>,
    pub pc: u32,
    pub width: usize,
    recovery: contract::RecoveryExit,
}

pub(super) struct Pending {
    seeds: Vec<(Value, Input)>,
    state: Value,
    outputs: [Value; 18],
    entry: u32,
    pc: u32,
    width: usize,
    recovery: contract::RecoveryExit,
}

impl Draft {
    pub(super) fn capture(
        seeds: &[(Value, Input)],
        state: Value,
        outputs: [Value; 18],
        entry: u32,
        pc: u32,
        width: usize,
        recovery: contract::RecoveryExit,
    ) -> Option<Pending> {
        Some(Pending {
            seeds: seeds.to_vec(),
            state,
            outputs,
            entry,
            pc,
            width,
            recovery,
        })
    }

    pub(super) fn finish(
        mut self,
        code: &cranelift_codegen::CompiledCode,
        base: usize,
    ) -> Option<(FaultSite, Recipe)> {
        let buffer = &code.buffer;
        let mut loads = buffer.traps().iter().filter(|trap| {
            trap.code == ir::TrapCode::user(1).unwrap()
                && buffer.get_srclocs_sorted().iter().any(|range| {
                    range.loc.bits() == LOAD_SOURCE
                        && range.start <= trap.offset
                        && trap.offset < range.end
                })
        });
        let offset = loads.next()?.offset;
        if loads.next().is_some() {
            return None;
        }
        // The unique trap is emitted on the typed scalar load itself. Its IR
        // width and access direction remain authoritative after instruction
        // selection; no machine-byte decoder participates in publication.
        let recovery = self.recovery.location(code)?;
        // Debug labels do not extend liveness. Use only a final range actually
        // containing the faulting instruction; otherwise retain the pure DAG.
        for (index, value) in self.values.iter().enumerate() {
            if value.is_none()
                || matches!(
                    self.recipe.nodes[index].source,
                    Source::Constant(_) | Source::Input(_)
                )
            {
                continue;
            }
            if let Some(ranges) = code.value_labels_ranges.get(&ValueLabel::from_u32(LABEL_BASE + index as u32))
                && let Some(range) = ranges.iter().find(|range| range.start <= offset && offset < range.end)
                && let LabelValueLoc::Reg(register) = range.loc
                && let Some(register) = register.to_real_reg()
                // regalloc2's integer class is discriminant 0. Do not interpret
                // float/vector register encodings as OS CONTEXT GPR numbers.
                && register.class() as u8 == 0 && register.hw_enc() < 16
            {
                self.recipe.nodes[index].source = Source::HostRegister(register.hw_enc());
            }
        }
        self.recipe.valid().then_some((
            FaultSite {
                host_pc: base.checked_add(offset as usize)?,
                recovery_pc: base.checked_add(recovery)?,
                guest_pc: self.pc,
                width: self.width,
                access: Access::Read,
                recovery_result: Some(super::Exit::ReadFault as u32),
            },
            self.recipe,
        ))
    }
}

impl Pending {
    pub(super) fn resolve(self, function: &mut ir::Function) -> Option<Draft> {
        let mut builder = Builder {
            function,
            seeds: &self.seeds,
            state: self.state,
            nodes: Vec::new(),
            values: Vec::new(),
            seen: BTreeMap::new(),
        };
        let mut indices = [0u8; 18];
        for (index, output) in self.outputs.into_iter().enumerate() {
            indices[index] = builder.value(output, 0)?;
        }
        let recipe = Recipe {
            entry_pc: self.entry,
            nodes: builder.nodes,
            outputs: indices,
        };
        let values = builder.values;
        if !recipe.valid() {
            return None;
        }
        for (index, value) in values.iter().enumerate() {
            if let Some(value) = value {
                let from = ir::RelSourceLoc::from_base_offset(
                    function.params.base_srcloc(),
                    ir::SourceLoc::new(LOAD_SOURCE),
                );
                let labels = function.dfg.values_labels.as_mut()?;
                let start = ir::ValueLabelStart {
                    from,
                    label: ValueLabel::from_u32(LABEL_BASE + index as u32),
                };
                match labels
                    .entry(*value)
                    .or_insert_with(|| ir::ValueLabelAssignments::Starts(Vec::new()))
                {
                    ir::ValueLabelAssignments::Starts(starts) => starts.push(start),
                    _ => return None,
                }
            }
        }
        Some(Draft {
            recipe,
            values,
            pc: self.pc,
            width: self.width,
            recovery: self.recovery,
        })
    }
}

struct Builder<'a> {
    function: &'a ir::Function,
    seeds: &'a [(Value, Input)],
    state: Value,
    nodes: Vec<Node>,
    values: Vec<Option<Value>>,
    seen: BTreeMap<Value, u8>,
}
impl Builder<'_> {
    fn constant(&self, value: Value) -> Option<u64> {
        let dfg = &self.function.dfg;
        let value = dfg.resolve_aliases(value);
        // The first checked entry explicitly seeds every lazy kind with zero.
        // It may still be a phi at this point because an indirect transfer can
        // revisit the block; the recovery contract already maps that entry
        // value to Input::Zero rather than a later traversal's value.
        if self.seeds.iter().any(|(seed, input)| {
            matches!(input, Input::Zero) && dfg.resolve_aliases(*seed) == value
        }) {
            return Some(0);
        }
        let ValueDef::Result(instruction, 0) = dfg.value_def(value) else {
            return None;
        };
        let InstructionData::UnaryImm {
            opcode: Opcode::Iconst,
            imm,
        } = dfg.insts[instruction]
        else {
            return None;
        };
        let bits = dfg.value_type(value).bits();
        matches!(bits, 8 | 16 | 32 | 64).then(|| imm.bits() as u64 & (u64::MAX >> (64 - bits)))
    }

    fn constant_condition(&self, value: Value) -> Option<bool> {
        if let Some(value) = self.constant(value) {
            return Some(value != 0);
        }
        let dfg = &self.function.dfg;
        let value = dfg.resolve_aliases(value);
        let ValueDef::Result(instruction, 0) = dfg.value_def(value) else {
            return None;
        };
        let InstructionData::IntCompareImm {
            opcode: Opcode::IcmpImm,
            cond,
            arg,
            imm,
        } = dfg.insts[instruction]
        else {
            return None;
        };
        let left = self.constant(arg)?;
        let bits = dfg.value_type(arg).bits();
        let right = imm.bits() as u64 & (u64::MAX >> (64 - bits));
        match cond {
            IntCC::Equal => Some(left == right),
            IntCC::NotEqual => Some(left != right),
            _ => None,
        }
    }

    fn push(&mut self, source: Source, bits: u8, value: Option<Value>) -> Option<u8> {
        if self.nodes.len() == MAX_NODES {
            return None;
        }
        let index = self.nodes.len() as u8;
        self.nodes.push(Node { source, bits });
        self.values.push(value);
        Some(index)
    }
    fn value(&mut self, value: Value, depth: usize) -> Option<u8> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let dfg = &self.function.dfg;
        let value = dfg.resolve_aliases(value);
        if let Some(&index) = self.seen.get(&value) {
            return Some(index);
        }
        let bits = u8::try_from(dfg.value_type(value).bits()).ok()?;
        let source = if let Some((_, input)) = self
            .seeds
            .iter()
            .find(|(seed, _)| dfg.resolve_aliases(*seed) == value)
        {
            Source::Input(*input)
        } else {
            let ValueDef::Result(instruction, 0) = dfg.value_def(value) else {
                return None;
            };
            match dfg.insts[instruction] {
                InstructionData::UnaryImm {
                    opcode: Opcode::Iconst,
                    imm,
                } => Source::Constant(imm.bits() as u64),
                InstructionData::Load {
                    opcode: Opcode::Load,
                    arg,
                    offset,
                    ..
                } if dfg.resolve_aliases(arg) == dfg.resolve_aliases(self.state)
                    && i32::from(offset) == offset_of!(CpuState, fs_base) as i32 =>
                {
                    Source::Input(Input::FsBase)
                }
                InstructionData::IntCompare {
                    opcode: Opcode::Icmp,
                    cond: IntCC::UnsignedLessThan,
                    args,
                } => {
                    let left = self.value(args[0], depth + 1)?;
                    let right = self.value(args[1], depth + 1)?;
                    Source::Calculate(Operation::UnsignedLessThan, left, right)
                }
                InstructionData::Ternary {
                    opcode: Opcode::Select,
                    args: [condition, if_true, if_false],
                } => {
                    // The entry's carry kind can still be an unsealed SSA
                    // parameter when INC/DEC emits a select. Resolve only a
                    // now-proven constant condition; dynamic selects remain
                    // outside the bounded recovery expression set.
                    let selected = if self.constant_condition(condition)? {
                        if_true
                    } else {
                        if_false
                    };
                    let index = self.value(selected, depth + 1)?;
                    self.seen.insert(value, index);
                    return Some(index);
                }
                InstructionData::Binary { opcode, args } => {
                    let operation = binary(opcode)?;
                    let left = self.value(args[0], depth + 1)?;
                    let right = self.value(args[1], depth + 1)?;
                    Source::Calculate(operation, left, right)
                }
                InstructionData::BinaryImm64 { opcode, arg, imm } => {
                    let operation = binary(opcode)?;
                    let left = self.value(arg, depth + 1)?;
                    let right = self.push(Source::Constant(imm.bits() as u64), bits, None)?;
                    Source::Calculate(operation, left, right)
                }
                InstructionData::Unary { opcode, arg } => {
                    let operation = match opcode {
                        Opcode::Bnot => Operation::Not,
                        Opcode::Ineg => Operation::Neg,
                        Opcode::Ireduce | Opcode::Uextend => Operation::Cast,
                        Opcode::Sextend => Operation::SignExtend(dfg.value_type(arg).bits() as u8),
                        _ => return None,
                    };
                    let left = self.value(arg, depth + 1)?;
                    Source::Calculate(operation, left, left)
                }
                _ => return None,
            }
        };
        let index = self.push(source, bits, Some(value))?;
        self.seen.insert(value, index);
        Some(index)
    }
}
fn binary(opcode: Opcode) -> Option<Operation> {
    Some(match opcode {
        Opcode::Iadd | Opcode::IaddImm => Operation::Add,
        Opcode::Isub => Operation::Sub,
        Opcode::Imul | Opcode::ImulImm => Operation::Mul,
        Opcode::Band | Opcode::BandImm => Operation::And,
        Opcode::Bor | Opcode::BorImm => Operation::Or,
        Opcode::Bxor | Opcode::BxorImm => Operation::Xor,
        Opcode::Ishl | Opcode::IshlImm => Operation::Shl,
        Opcode::Ushr | Opcode::UshrImm => Operation::Ushr,
        Opcode::Sshr | Opcode::SshrImm => Operation::Sshr,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: EXEC-X64-7
    #[test]
    fn only_proven_constant_selects_enter_the_recovery_recipe() {
        let mut function = ir::Function::new();
        function
            .signature
            .params
            .push(ir::AbiParam::new(types::I32));
        function
            .signature
            .params
            .push(ir::AbiParam::new(types::I32));
        let mut context = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut function, &mut context);
        let block = b.create_block();
        b.append_block_params_for_function_params(block);
        b.switch_to_block(block);
        let carry = b.block_params(block)[0];
        let entry_kind = b.block_params(block)[1];
        let zero = b.ins().iconst(types::I32, 0);
        let one = b.ins().iconst(types::I32, 1);
        // The inactive branch is deliberately outside the recipe vocabulary.
        let other = b.ins().rotl(carry, one);
        let yes = b.ins().icmp_imm(IntCC::Equal, zero, 0);
        let no = b.ins().icmp_imm(IntCC::NotEqual, zero, 0);
        let from_true = b.ins().select(yes, carry, other);
        let from_false = b.ins().select(no, other, carry);
        let unknown = b.ins().icmp_imm(IntCC::Equal, carry, 0);
        let dynamic = b.ins().select(unknown, carry, other);
        let negative = b.ins().iconst(types::I32, -1);
        let equal_negative = b.ins().icmp_imm(IntCC::Equal, negative, -1);
        let from_negative = b.ins().select(equal_negative, carry, other);
        let raw_entry_kind = b.ins().icmp_imm(IntCC::Equal, entry_kind, 0);
        let from_entry_kind = b.ins().select(raw_entry_kind, carry, other);
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize();

        let seeds = [(carry, Input::Carry), (entry_kind, Input::Zero)];
        let mut builder = Builder {
            function: &function,
            seeds: &seeds,
            state: carry,
            nodes: Vec::new(),
            values: Vec::new(),
            seen: BTreeMap::new(),
        };
        let index = builder.value(from_true, 0).unwrap();
        assert_eq!(builder.value(from_false, 0), Some(index));
        assert_eq!(builder.value(from_negative, 0), Some(index));
        assert_eq!(builder.value(from_entry_kind, 0), Some(index));
        assert!(builder.value(dynamic, 0).is_none());
        assert_eq!(builder.nodes.len(), 1);
        assert!(matches!(
            builder.nodes[0].source,
            Source::Input(Input::Carry)
        ));
    }
}
