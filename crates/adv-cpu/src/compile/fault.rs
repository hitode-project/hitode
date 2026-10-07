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
pub(super) const RETURN_SOURCE: u32 = 2;
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
}

pub(super) struct Pending {
    seeds: Vec<(Value, Input)>,
    state: Value,
    outputs: [Value; 18],
    entry: u32,
    pc: u32,
    width: usize,
}

impl Draft {
    pub(super) fn capture(
        b: &mut FunctionBuilder<'_>,
        seeds: &[(Value, Input)],
        state: Value,
        outputs: [Value; 18],
        entry: u32,
        pc: u32,
        width: usize,
    ) -> Option<Pending> {
        let _ = b;
        Some(Pending {
            seeds: seeds.to_vec(),
            state,
            outputs,
            entry,
            pc,
            width,
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
        let instruction = Decoder::with_ip(
            64,
            &buffer.data()[offset as usize..],
            u64::from(offset),
            DecoderOptions::NONE,
        )
        .decode();
        if !matches!(
            instruction.mnemonic(),
            M::Mov | M::Movzx | M::Movsx | M::Movsxd
        ) || instruction.op1_kind() != OpKind::Memory
            || instruction.memory_size().size() != self.width
        {
            return None;
        }
        let mut recovery = buffer
            .get_srclocs_sorted()
            .iter()
            .filter(|range| range.loc.bits() == RETURN_SOURCE)
            .map(|range| range.start)
            .min()?;
        if !epilogue(buffer.data(), recovery as usize) {
            // Constant CSE can leave the return's mov eax,reason assigned to
            // its earlier SSA source location. Include only one immediately
            // preceding, decoded instruction and revalidate the entire return.
            let mut decoder = Decoder::new(64, buffer.data(), DecoderOptions::NONE);
            let mut previous = None;
            while decoder.can_decode() && decoder.ip() < u64::from(recovery) {
                let instruction = decoder.decode();
                if instruction.next_ip() == u64::from(recovery) {
                    previous = Some(instruction.ip() as u32);
                }
            }
            recovery = previous?;
        }
        if !epilogue(buffer.data(), recovery as usize) {
            return None;
        }
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
                recovery_pc: base.checked_add(recovery as usize)?,
                guest_pc: self.pc,
                width: self.width,
                access: Access::Read,
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

fn epilogue(code: &[u8], mut offset: usize) -> bool {
    let mut reason = false;
    for _ in 0..32 {
        if offset >= code.len() {
            return false;
        }
        let instruction =
            Decoder::with_ip(64, &code[offset..], offset as u64, DecoderOptions::NONE).decode();
        if instruction.is_invalid() {
            return false;
        }
        match instruction.mnemonic() {
            M::Mov
                if matches!(instruction.op0_register(), R::EAX | R::RAX)
                    && matches!(
                        instruction.op1_kind(),
                        OpKind::Immediate32 | OpKind::Immediate32to64 | OpKind::Immediate64
                    ) =>
            {
                if instruction.immediate(1) != Exit::ReadFault as u64 {
                    return false;
                }
                reason = true;
            }
            M::Mov
                if instruction.op0_register() == R::RSP && instruction.op1_register() == R::RBP => {
            }
            M::Mov
            | M::Movups
            | M::Movaps
            | M::Movdqu
            | M::Movdqa
            | M::Vmovups
            | M::Vmovaps
            | M::Vmovdqu
            | M::Vmovdqa
                if instruction.op0_kind() == OpKind::Register
                    && instruction.op1_kind() == OpKind::Memory
                    && instruction.memory_base() == R::RSP
                    && instruction.memory_index() == R::None
                    && (matches!(
                        instruction.op0_register(),
                        R::RBX | R::RBP | R::RSI | R::RDI | R::R12 | R::R13 | R::R14 | R::R15
                    ) || (R::XMM6 as u32..=R::XMM15 as u32)
                        .contains(&(instruction.op0_register() as u32))) => {}
            M::Add
                if instruction.op0_register() == R::RSP
                    && matches!(
                        instruction.op1_kind(),
                        OpKind::Immediate8to64 | OpKind::Immediate32to64
                    ) => {}
            M::Pop if instruction.op0_register() == R::RBP => {}
            M::Jmp if instruction.op0_kind() == OpKind::NearBranch64 => {
                offset = instruction.near_branch64() as usize;
                continue;
            }
            M::Ret => return reason,
            _ => return false,
        }
        offset = instruction.next_ip() as usize;
    }
    false
}
