use super::*;
use opcode::Category;
use std::collections::HashSet;

pub fn verify(module: &Module) -> Result<(), Error> {
    match (module.stage, module.model) {
        (
            Stage::Hull | Stage::Domain,
            Model::Sm40 | Model::Sm41 | Model::Level91 | Model::Level93,
        )
        | (Stage::Geometry | Stage::Compute, Model::Level91 | Model::Level93) => {
            return Err(Error::Invalid("stage for shader model"));
        }
        (Stage::Vertex | Stage::Pixel, Model::Level91 | Model::Level93)
        | (_, Model::Sm40 | Model::Sm41 | Model::Sm50) => {}
    }
    for element in module
        .signatures
        .input
        .iter()
        .chain(&module.signatures.output)
        .chain(&module.signatures.patch_constant)
    {
        if element.mask & !15 != 0 || element.used_mask & !element.mask != 0 {
            return Err(Error::Invalid("signature mask"));
        }
    }
    let mut state = State {
        stage: module.stage,
        workgroup: false,
        immediate: false,
        bindings: HashSet::new(),
    };
    state.block(&module.body, false, false, 0)?;
    if (module.stage == Stage::Compute) != state.workgroup {
        return Err(Error::Invalid("thread group declaration"));
    }
    Ok(())
}

struct State {
    stage: Stage,
    workgroup: bool,
    immediate: bool,
    bindings: HashSet<(Register, u32)>,
}

impl State {
    fn block(
        &mut self,
        body: &Block,
        loop_body: bool,
        breakable: bool,
        depth: usize,
    ) -> Result<(), Error> {
        if depth > MAX_CONTROL_FLOW_DEPTH {
            return Err(Error::Invalid("control flow nesting"));
        }
        for statement in body {
            match statement {
                Statement::Declare(decl) => {
                    if decl.opcode.category() != Category::Declaration {
                        return Err(Error::Invalid("declaration opcode"));
                    }
                    verify_declaration(decl)?;
                    if let Some(operand) = &decl.operand {
                        verify_operand(operand)?;
                    }
                    if let Some(binding) = decl.binding
                        && !self.bindings.insert((binding.register, binding.slot))
                    {
                        return Err(Error::Invalid("duplicate binding"));
                    }
                    if decl.opcode == Opcode::DclThreadGroup {
                        let [x, y, z] = decl.data.as_slice() else {
                            return Err(Error::Invalid("workgroup dimensions"));
                        };
                        if self.workgroup || [x, y, z].contains(&&0) {
                            return Err(Error::Invalid("workgroup dimensions"));
                        }
                        self.workgroup = true;
                    }
                    if decl.opcode == Opcode::DclTemps && !matches!(&decl.data[..], [0..=4096]) {
                        return Err(Error::Invalid("temporary register count"));
                    }
                    if decl.opcode == Opcode::DclIndexableTemp
                        && !matches!(&decl.data[..], [_, 1..=4096, 1..=4])
                    {
                        return Err(Error::Invalid("indexable temporary declaration"));
                    }
                }
                Statement::Execute(op) => {
                    if op.opcode.category() != Category::Operation {
                        return Err(Error::Invalid("ordinary instruction opcode"));
                    }
                    if (matches!(op.opcode, Opcode::Break | Opcode::Breakc) && !breakable)
                        || (matches!(op.opcode, Opcode::Continue | Opcode::Continuec) && !loop_body)
                    {
                        return Err(Error::Invalid("branch outside containing construct"));
                    }
                    if op.opcode == Opcode::Discard && self.stage != Stage::Pixel {
                        return Err(Error::Invalid("discard outside pixel stage"));
                    }
                    verify_instruction(op)?;
                }
                Statement::If {
                    test,
                    accept,
                    reject,
                } => {
                    if test.opcode != Opcode::If {
                        return Err(Error::Invalid("if opcode"));
                    }
                    verify_instruction(test)?;
                    self.block(accept, loop_body, breakable, depth + 1)?;
                    if let Some(reject) = reject {
                        self.block(reject, loop_body, breakable, depth + 1)?;
                    }
                }
                Statement::Loop(body) => self.block(body, true, true, depth + 1)?,
                Statement::Switch { selector, cases } => {
                    verify_operand(selector)?;
                    let mut values = HashSet::new();
                    for case in cases {
                        if !values.insert(case.value) {
                            return Err(Error::Invalid("duplicate switch label"));
                        }
                        self.block(&case.body, loop_body, true, depth + 1)?;
                    }
                }
                Statement::ImmediateConstants(values) => {
                    if self.immediate || values.len() > 4096 {
                        return Err(Error::Invalid("immediate constant buffer"));
                    }
                    self.immediate = true;
                }
                Statement::CustomData { .. } => {}
            }
        }
        Ok(())
    }
}

fn verify_instruction(op: &Instruction) -> Result<(), Error> {
    let (destinations, sources) = op.opcode.types();
    if (op.destinations.len(), op.sources.len()) != (destinations.len(), sources.len()) {
        return Err(Error::Invalid("instruction arity"));
    }
    for destination in &op.destinations {
        verify_operand(destination)?;
        if destination.register().is_none()
            || destination.modifier != Modifier::None
            || !matches!(
                destination.components,
                Components::Mask(_) | Components::Scalar | Components::None
            )
        {
            return Err(Error::Invalid("instruction destination"));
        }
    }
    for source in &op.sources {
        verify_operand(source)?;
    }
    Ok(())
}

fn verify_operand(op: &Operand) -> Result<(), Error> {
    let valid = match op.components {
        Components::Mask(mask) => mask <= 15,
        Components::Swizzle(lanes) => lanes.iter().all(|&lane| lane < 4),
        Components::Select(lane) => lane < 4,
        Components::None | Components::Scalar => true,
    };
    if !valid {
        return Err(Error::Invalid("operand components"));
    }
    match &op.value {
        OperandValue::Register { register, indices } => {
            let valid = match register {
                Register::Temp
                | Register::Sampler
                | Register::Resource
                | Register::ImmediateConstantBuffer
                | Register::Label
                | Register::Stream
                | Register::UnorderedAccessView
                | Register::ThreadGroupSharedMemory
                | Register::InputPatchConstant => indices.len() == 1,
                Register::Input | Register::Output => matches!(indices.len(), 1 | 2),
                Register::IndexableTemp
                | Register::InputControlPoint
                | Register::OutputControlPoint => indices.len() == 2,
                Register::InputPrimitiveId
                | Register::OutputDepth
                | Register::Null
                | Register::Rasterizer
                | Register::OutputCoverageMask
                | Register::OutputControlPointId
                | Register::InputForkInstanceId
                | Register::InputJoinInstanceId
                | Register::InputDomainPoint
                | Register::InputThreadId
                | Register::InputThreadGroupId
                | Register::InputThreadIdInGroup
                | Register::InputCoverageMask
                | Register::InputThreadIdInGroupFlattened
                | Register::InputGsInstanceId
                | Register::OutputDepthGreaterEqual
                | Register::OutputDepthLessEqual
                | Register::CycleCounter
                | Register::OutputStencilRef
                | Register::InnerCoverage => indices.is_empty(),
                Register::FunctionBody
                | Register::FunctionTable
                | Register::Interface
                | Register::FunctionInput
                | Register::FunctionOutput
                | Register::ThisPointer => indices.len() <= 3,
                Register::ConstantBuffer => false,
            };
            if !valid {
                return Err(Error::Invalid("operand register indices"));
            }
            for index in indices {
                verify_index(index, 1)?;
            }
        }
        OperandValue::Constant { slot, byte_offset } => {
            verify_index(slot, 1)?;
            verify_index(byte_offset, 16)?;
        }
        OperandValue::Immediate(_) | OperandValue::Immediate64(_) => {}
    }
    Ok(())
}

fn verify_index(index: &Index, alignment: u64) -> Result<(), Error> {
    let base = match index {
        Index::Direct(value) => *value,
        Index::Relative { base, register } => {
            verify_operand(register)?;
            *base
        }
    };
    if !base.is_multiple_of(alignment) {
        return Err(Error::Invalid("unaligned constant buffer access"));
    }
    Ok(())
}

fn verify_declaration(decl: &Declaration) -> Result<(), Error> {
    use Opcode::*;
    let expected = if matches!(
        decl.opcode,
        DclResource
            | DclUnorderedAccessViewTyped
            | DclIndexRange
            | DclMaxOutputVertexCount
            | DclInputSgv
            | DclInputSiv
            | DclInputPsSgv
            | DclInputPsSiv
            | DclOutputSgv
            | DclOutputSiv
            | DclTemps
            | DclFunctionBody
            | DclHsMaxTessfactor
            | DclHsForkPhaseInstanceCount
            | DclHsJoinPhaseInstanceCount
            | DclUnorderedAccessViewStructured
            | DclThreadGroupSharedMemoryRaw
            | DclResourceStructured
            | DclGsInstanceCount
    ) {
        Some(1)
    } else if decl.opcode == DclThreadGroupSharedMemoryStructured {
        Some(2)
    } else if matches!(decl.opcode, DclIndexableTemp | DclThreadGroup) {
        Some(3)
    } else if decl.opcode == DclFunctionTable {
        match decl.data.as_slice() {
            [_, count, ids @ ..] if ids.len() == *count as usize => None,
            _ => return Err(Error::Invalid("function table declaration")),
        }
    } else if decl.opcode == DclInterface {
        use crate::shader::dxbc::fields::decode_interface_table_length;
        match decl.data.as_slice() {
            [_, _, lengths, tables @ ..]
                if tables.len() == decode_interface_table_length(*lengths) as usize =>
            {
                None
            }
            _ => return Err(Error::Invalid("interface declaration")),
        }
    } else {
        Some(0)
    };
    if let Some(expected) = expected
        && decl.data.len() != expected
    {
        return Err(Error::Invalid("declaration word count"));
    }
    if let Some(binding) = decl.binding {
        if matches!(binding.kind, BindingKind::Structured { stride, .. } if stride == 0 || stride % 4 != 0)
        {
            return Err(Error::Invalid("structured buffer stride"));
        }
        if matches!(binding.kind, BindingKind::ConstantBuffer { bytes, .. } if bytes > 65_536) {
            return Err(Error::Invalid("constant buffer size"));
        }
    }
    Ok(())
}
