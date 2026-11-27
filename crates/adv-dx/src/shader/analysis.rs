//! Binding and component use of a verified shader module.

pub(crate) mod uav;

use super::dxbc::fields::D3D11_SB_GLOBAL_FLAG_ENABLE_DOUBLE_PRECISION_FLOAT_OPS;
use super::ir::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reflection {
    pub stage: Stage,
    pub signatures: Signatures,
    pub bindings: Vec<Binding>,
    /// Indexed by register file and statically selected register. None denotes relative addressing.
    pub reads: HashMap<(Register, Option<u32>), u8>,
    pub writes: HashMap<(Register, Option<u32>), u8>,
    pub atomic_bindings: HashSet<(Register, u32)>,
    pub counter_bindings: HashSet<u32>,
    pub interpolation_queries: bool,
    pub sample_positions: bool,
    pub lod_queries: bool,
    pub global_flags: u32,
    pub feature_flags: u64,
    pub workgroup_size: Option<[u32; 3]>,
    pub temporary_count: u32,
    pub doubles: bool,
    pub discard: bool,
    pub interpolation: HashMap<u32, Interpolation>,
}

pub fn analyze(module: &Module) -> Reflection {
    let mut reflection = Reflection {
        stage: module.stage,
        signatures: module.signatures.clone(),
        bindings: Vec::new(),
        reads: HashMap::new(),
        writes: HashMap::new(),
        atomic_bindings: HashSet::new(),
        counter_bindings: HashSet::new(),
        interpolation_queries: false,
        sample_positions: false,
        lod_queries: false,
        global_flags: 0,
        feature_flags: module.feature_flags,
        workgroup_size: None,
        temporary_count: 0,
        discard: false,
        interpolation: HashMap::new(),
        // D3D_SHADER_REQUIRES_DOUBLES and D3D_SHADER_REQUIRES_11_1_DOUBLE_EXTENSIONS.
        doubles: module.feature_flags & 0x21 != 0,
    };
    reflection.block(&module.body);
    reflection
}

impl Reflection {
    pub(crate) fn sample_frequency(&self) -> bool {
        self.signatures
            .input
            .iter()
            .any(|input| input.system_value == SystemValue::SampleIndex)
            || self.interpolation.values().any(|mode| {
                matches!(
                    mode,
                    Interpolation::PerspectiveSample | Interpolation::LinearSample
                )
            })
    }

    pub(crate) fn writes_uav(&self) -> bool {
        !self.counter_bindings.is_empty()
            || self
                .writes
                .keys()
                .any(|(register, _)| *register == Register::UnorderedAccessView)
    }

    fn block(&mut self, block: &Block) {
        for statement in block {
            match statement {
                Statement::Declare(decl) => {
                    if let Some(mode) = decl.interpolation
                        && let Some((Register::Input, [index])) =
                            decl.operand.as_ref().and_then(Operand::register)
                        && let Some(register) = index.direct()
                    {
                        self.interpolation.insert(register, mode);
                    }
                    if let Some(binding) = decl.binding
                        && binding.register != Register::ThreadGroupSharedMemory
                    {
                        self.bindings.push(binding);
                    }
                    if decl.opcode == Opcode::DclGlobalFlags {
                        self.global_flags |= decl.controls;
                        self.doubles |= decl.controls
                            & D3D11_SB_GLOBAL_FLAG_ENABLE_DOUBLE_PRECISION_FLOAT_OPS
                            != 0;
                    } else if decl.opcode == Opcode::DclThreadGroup {
                        self.workgroup_size = decl.data.as_slice().try_into().ok();
                    } else if decl.opcode == Opcode::DclTemps {
                        self.temporary_count = self.temporary_count.max(decl.data[0]);
                    }
                }
                Statement::Execute(op) => self.instruction(op),
                Statement::If {
                    test,
                    accept,
                    reject,
                } => {
                    self.instruction(test);
                    self.block(accept);
                    if let Some(reject) = reject {
                        self.block(reject);
                    }
                }
                Statement::Loop(body) => self.block(body),
                Statement::Switch { selector, cases } => {
                    self.operand(selector, 1, false);
                    for case in cases {
                        self.block(&case.body);
                    }
                }
                Statement::ImmediateConstants(_) | Statement::CustomData { .. } => {}
            }
        }
    }

    fn instruction(&mut self, op: &Instruction) {
        self.discard |= op.opcode == Opcode::Discard;
        self.interpolation_queries |= op.opcode.is_interpolation();
        self.sample_positions |= op.opcode == Opcode::SamplePos;
        self.lod_queries |= op.opcode == Opcode::Lod;
        let (destinations, sources) = op.opcode.types();
        self.doubles |=
            destinations.contains(&ValueType::Double) || sources.contains(&ValueType::Double);
        let mut lanes = 0;
        for operand in &op.destinations {
            if op.opcode.is_atomic()
                && let Some((register, [slot])) = operand.register()
                && let Some(slot) = slot.direct()
                && matches!(
                    register,
                    Register::UnorderedAccessView | Register::ThreadGroupSharedMemory
                )
            {
                self.atomic_bindings.insert((register, slot));
            }
            lanes |= operand.components.mask();
            self.operand(operand, operand.components.mask(), true);
        }
        let lanes = if op.opcode == Opcode::Dp2 {
            3
        } else if op.opcode == Opcode::Dp3 {
            7
        } else if op.opcode == Opcode::Dp4 || op.opcode.types().1.contains(&ValueType::Binding) {
            15
        } else if lanes == 0 {
            1
        } else {
            lanes
        };
        for operand in &op.sources {
            if op.opcode.is_counter()
                && let Some((Register::UnorderedAccessView, [slot])) = operand.register()
                && let Some(slot) = slot.direct()
            {
                self.counter_bindings.insert(slot);
            }
            let selected = source_components(operand, lanes);
            self.operand(operand, selected, false);
        }
    }

    fn operand(&mut self, operand: &Operand, mask: u8, write: bool) {
        let (register, indices, slot) = match &operand.value {
            OperandValue::Register { register, indices } => {
                let slot = indices.last().map_or(Some(0), Index::direct);
                (*register, indices.as_slice(), slot)
            }
            OperandValue::Constant { slot, byte_offset } => {
                self.relative(slot);
                self.relative(byte_offset);
                (Register::ConstantBuffer, &[][..], slot.direct())
            }
            OperandValue::Immediate(_) | OperandValue::Immediate64(_) => return,
        };
        if register != Register::Null {
            let access = if write {
                &mut self.writes
            } else {
                &mut self.reads
            };
            *access.entry((register, slot)).or_default() |= mask;
        }
        for index in indices {
            self.relative(index);
        }
    }

    fn relative(&mut self, index: &Index) {
        if let Index::Relative { register, .. } = index {
            self.operand(register, register.components.mask(), false);
        }
    }
}

fn source_components(operand: &Operand, lanes: u8) -> u8 {
    operand
        .components
        .swizzle()
        .into_iter()
        .enumerate()
        .fold(0, |mask, (lane, source)| {
            mask | (((lanes >> lane) & 1) << source)
        })
}
