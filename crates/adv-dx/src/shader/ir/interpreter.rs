//! Scalar execution before variant lowering. An error means the caller has no bounds proof.

mod alu;

use super::*;
use std::collections::HashMap;
use std::ops::ControlFlow;

#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub registers: HashMap<(Register, u32), Vector>,
}

pub type Constants = HashMap<u32, Vec<Vector>>;

#[derive(Clone, Debug, Default)]
pub struct Outputs {
    pub registers: HashMap<(Register, u32), Vector>,
    pub discarded: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ResourceAccess {
    pub instruction: Opcode,
    pub binding: (Register, u32),
    /// Sampler slot and channel selection.
    pub sampler: Option<(u32, u8)>,
    pub operands: ResourceOperands,
    pub controls: u32,
}

pub trait Resources {
    fn read(&mut self, access: ResourceAccess, arguments: &[Vector]) -> Result<Vector, Error>;
    fn atomic(&mut self, access: ResourceAccess, arguments: &[Vector]) -> Result<Vector, Error>;
    /// Evaluate an input using the rasterizer state of this invocation.
    fn interpolate(
        &mut self,
        instruction: Opcode,
        _input: u32,
        _swizzle: [u8; 4],
        _arguments: &[Vector],
    ) -> Result<Vector, Error> {
        Err(Error::UnsupportedOpcode(instruction))
    }
    fn write(
        &mut self,
        access: ResourceAccess,
        mask: u8,
        arguments: &[Vector],
    ) -> Result<(), Error>;
}

pub struct NoTextures;

impl Resources for NoTextures {
    fn read(&mut self, access: ResourceAccess, _: &[Vector]) -> Result<Vector, Error> {
        Err(Error::UnsupportedOpcode(access.instruction))
    }

    fn atomic(&mut self, access: ResourceAccess, _: &[Vector]) -> Result<Vector, Error> {
        Err(Error::UnsupportedOpcode(access.instruction))
    }

    fn write(&mut self, access: ResourceAccess, _: u8, _: &[Vector]) -> Result<(), Error> {
        Err(Error::UnsupportedOpcode(access.instruction))
    }
}

pub fn run(
    module: &Module,
    inputs: &Inputs,
    constants: &Constants,
    resources: &mut impl Resources,
) -> Result<Outputs, Error> {
    if matches!(module.stage, Stage::Geometry | Stage::Hull | Stage::Domain) {
        return Err(Error::UnsupportedStage(module.stage));
    }
    let (module, reflection) = passes::prepare(module)?;
    if reflection.doubles {
        return Err(Error::DoublePrecision);
    }
    let mut machine = Machine {
        inputs,
        constants,
        resources,
        temps: vec![[0; 4]; reflection.temporary_count as usize],
        arrays: HashMap::new(),
        immediate: &[],
        outputs: Outputs::default(),
        fuel: crate::shader::MAX_EXECUTION_STEPS,
    };
    let _ = machine.block(&module.body)?;
    Ok(machine.outputs)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Branch {
    Return,
    Break,
    Continue,
}

struct Machine<'a, R> {
    inputs: &'a Inputs,
    constants: &'a Constants,
    resources: &'a mut R,
    temps: Vec<Vector>,
    arrays: HashMap<u32, Vec<Vector>>,
    immediate: &'a [Vector],
    outputs: Outputs,
    fuel: usize,
}

impl<'a, R: Resources> Machine<'a, R> {
    fn tick(&mut self) -> Result<(), Error> {
        self.fuel = self.fuel.checked_sub(1).ok_or(Error::ExecutionLimit)?;
        Ok(())
    }

    fn block(&mut self, block: &'a Block) -> Result<ControlFlow<Branch>, Error> {
        for statement in block {
            self.tick()?;
            let branch = match statement {
                Statement::Declare(decl) => {
                    if decl.opcode == Opcode::DclIndexableTemp {
                        self.arrays
                            .insert(decl.data[0], vec![[0; 4]; decl.data[1] as usize]);
                    }
                    ControlFlow::Continue(())
                }
                Statement::ImmediateConstants(values) => {
                    self.immediate = values;
                    ControlFlow::Continue(())
                }
                Statement::CustomData { .. } => ControlFlow::Continue(()),
                Statement::Execute(instruction) => self.instruction(instruction)?,
                Statement::If {
                    test,
                    accept,
                    reject,
                } => {
                    if self.test(test)? {
                        self.block(accept)?
                    } else if let Some(reject) = reject {
                        self.block(reject)?
                    } else {
                        ControlFlow::Continue(())
                    }
                }
                Statement::Loop(body) => loop {
                    self.tick()?;
                    match self.block(body)? {
                        ControlFlow::Break(Branch::Break) => break ControlFlow::Continue(()),
                        ControlFlow::Break(Branch::Return) => {
                            break ControlFlow::Break(Branch::Return);
                        }
                        ControlFlow::Break(Branch::Continue) | ControlFlow::Continue(()) => {}
                    }
                },
                Statement::Switch { selector, cases } => {
                    let selector = self.read(selector, ValueType::Uint, false)?[0];
                    let start = cases
                        .iter()
                        .position(|case| case.value == Some(selector))
                        .or_else(|| cases.iter().position(|case| case.value.is_none()));
                    let mut branch = ControlFlow::Continue(());
                    if let Some(start) = start {
                        for case in &cases[start..] {
                            branch = self.block(&case.body)?;
                            if branch.is_break() {
                                break;
                            }
                        }
                    }
                    if branch == ControlFlow::Break(Branch::Break) {
                        ControlFlow::Continue(())
                    } else {
                        branch
                    }
                }
            };
            if branch.is_break() {
                return Ok(branch);
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    fn instruction(&mut self, op: &Instruction) -> Result<ControlFlow<Branch>, Error> {
        use Opcode::*;
        if matches!(
            op.opcode,
            Nop | HsDecls | HsControlPointPhase | HsForkPhase | HsJoinPhase
        ) {
            return Ok(ControlFlow::Continue(()));
        }
        if matches!(
            op.opcode,
            Break | Breakc | Continue | Continuec | Ret | Retc | Discard
        ) {
            let conditional = matches!(op.opcode, Breakc | Continuec | Retc | Discard);
            if conditional && !self.test(op)? {
                return Ok(ControlFlow::Continue(()));
            }
            self.outputs.discarded = op.opcode == Discard;
            let branch = if matches!(op.opcode, Break | Breakc) {
                Branch::Break
            } else if matches!(op.opcode, Continue | Continuec) {
                Branch::Continue
            } else {
                Branch::Return
            };
            return Ok(ControlFlow::Break(branch));
        }
        let (dst_types, src_types) = op.opcode.types();
        let args = op
            .sources
            .iter()
            .zip(src_types)
            .filter(|&(_, ty)| *ty != ValueType::Binding)
            .map(|(source, ty)| self.read(source, *ty, op.flush_inputs))
            .collect::<Result<Vec<_>, _>>()?;
        let mut values = if op.opcode.is_interpolation() {
            let value = self.resources.interpolate(
                op.opcode,
                self.location(&op.sources[0])?.1,
                op.sources[0].components.swizzle(),
                &args,
            )?;
            [value, [0; 4]]
        } else if src_types.contains(&ValueType::Binding) || dst_types.contains(&ValueType::Binding)
        {
            self.resource(op, &args)?
        } else {
            alu::evaluate(op, &args)?
        };
        for (((destination, value), ty), index) in op
            .destinations
            .iter()
            .zip(&mut values)
            .zip(dst_types)
            .zip(0..)
        {
            if *ty == ValueType::Binding {
                continue;
            }
            if op.flush_outputs && *ty == ValueType::Float {
                *value = value.map(alu::flush);
            }
            if op.saturate && index == 0 {
                *value = value.map(alu::saturate);
            }
            self.write(destination, *value)?;
        }
        Ok(ControlFlow::Continue(()))
    }

    fn resource(&mut self, op: &Instruction, args: &[Vector]) -> Result<[Vector; 2], Error> {
        let (dst_types, src_types) = op.opcode.types();
        let output = op
            .destinations
            .iter()
            .zip(dst_types)
            .find(|&(_, ty)| *ty == ValueType::Binding);
        let mut binding = None;
        let mut sampler = None;
        let mut swizzle = [0, 1, 2, 3];
        for (operand, _) in op
            .sources
            .iter()
            .zip(src_types)
            .filter(|&(_, ty)| *ty == ValueType::Binding)
        {
            let register = self.location(operand)?;
            if register.0 == Register::Sampler {
                sampler = Some((register.1, operand.components.swizzle()[0]));
            } else {
                binding = Some(register);
                if !matches!(operand.components, Components::None | Components::Scalar) {
                    swizzle = operand.components.swizzle();
                }
            }
        }
        if let Some((operand, _)) = output {
            binding = Some(self.location(operand)?);
        }
        let access = ResourceAccess {
            instruction: op.opcode,
            binding: binding.ok_or(Error::Invalid("resource operand"))?,
            sampler,
            operands: op.resource,
            controls: op.controls,
        };
        let mut result = [[0; 4]; 2];
        if op.opcode.is_atomic() || op.opcode.is_counter() {
            result[0] = self.resources.atomic(access, args)?;
        } else if let Some((operand, _)) = output {
            if !matches!(
                op.opcode,
                Opcode::StoreRaw | Opcode::StoreStructured | Opcode::StoreUavTyped
            ) {
                return Err(Error::UnsupportedOpcode(op.opcode));
            }
            self.resources
                .write(access, operand.components.mask(), args)?;
        } else {
            result[0] = self.resources.read(access, args)?;
            result[0] = swizzle.map(|lane| result[0][lane as usize]);
        }
        Ok(result)
    }

    fn test(&self, instruction: &Instruction) -> Result<bool, Error> {
        Ok(
            (self.read(&instruction.sources[0], ValueType::Uint, false)?[0] != 0)
                == instruction.test_nonzero,
        )
    }

    fn index(&self, index: &Index, stride: u64) -> Result<u32, Error> {
        let value = match index {
            Index::Direct(value) => *value,
            Index::Relative { base, register } => {
                let offset = self.read(register, ValueType::Int, false)?[0] as i32;
                base.wrapping_add_signed(i64::from(offset) * stride as i64)
            }
        };
        Ok((value / stride) as u32)
    }

    fn location(&self, operand: &Operand) -> Result<(Register, u32), Error> {
        let (register, indices) = operand
            .register()
            .ok_or(Error::Invalid("expected register"))?;
        match indices {
            [] => Ok((register, 0)),
            [index] => Ok((register, self.index(index, 1)?)),
            _ => Err(Error::UnsupportedRegister(register)),
        }
    }

    fn read(&self, operand: &Operand, ty: ValueType, flush: bool) -> Result<Vector, Error> {
        let vector = match &operand.value {
            OperandValue::Immediate(value) => *value,
            OperandValue::Immediate64(_) => return Err(Error::DoublePrecision),
            OperandValue::Constant { slot, byte_offset } => {
                let slot = self.index(slot, 1)?;
                let row = self.index(byte_offset, 16)?;
                self.constants
                    .get(&slot)
                    .and_then(|rows| rows.get(row as usize))
                    .copied()
                    .unwrap_or_default()
            }
            OperandValue::Register {
                register: Register::IndexableTemp,
                indices,
            } => {
                let array = self.index(&indices[0], 1)?;
                let row = self.index(&indices[1], 1)?;
                self.arrays
                    .get(&array)
                    .and_then(|rows| rows.get(row as usize))
                    .copied()
                    .unwrap_or_default()
            }
            OperandValue::Register { .. } => {
                let (register, index) = self.location(operand)?;
                match register {
                    Register::Temp => *self
                        .temps
                        .get(index as usize)
                        .ok_or(Error::MissingInput(register, index))?,
                    Register::ImmediateConstantBuffer => self
                        .immediate
                        .get(index as usize)
                        .copied()
                        .unwrap_or_default(),
                    Register::Output
                    | Register::OutputDepth
                    | Register::OutputDepthGreaterEqual
                    | Register::OutputDepthLessEqual
                    | Register::OutputCoverageMask
                    | Register::OutputStencilRef => *self
                        .outputs
                        .registers
                        .get(&(register, index))
                        .ok_or(Error::MissingInput(register, index))?,
                    _ => *self
                        .inputs
                        .registers
                        .get(&(register, index))
                        .ok_or(Error::MissingInput(register, index))?,
                }
            }
        };
        let swizzle = operand.components.swizzle();
        Ok(std::array::from_fn(|lane| {
            let value = vector[swizzle[lane] as usize];
            let value = alu::modify(value, operand.modifier, ty);
            if flush && ty == ValueType::Float {
                alu::flush(value)
            } else {
                value
            }
        }))
    }

    fn write(&mut self, operand: &Operand, value: Vector) -> Result<(), Error> {
        let register = if let Some((Register::IndexableTemp, [array, row])) = operand.register() {
            let array = self.index(array, 1)?;
            let row = self.index(row, 1)?;
            let Some(register) = self
                .arrays
                .get_mut(&array)
                .and_then(|rows| rows.get_mut(row as usize))
            else {
                return Err(Error::MissingInput(Register::IndexableTemp, row));
            };
            register
        } else {
            let (register, index) = self.location(operand)?;
            match register {
                Register::Null => return Ok(()),
                Register::Temp => self
                    .temps
                    .get_mut(index as usize)
                    .ok_or(Error::MissingInput(register, index))?,
                Register::Output
                | Register::OutputDepth
                | Register::OutputDepthGreaterEqual
                | Register::OutputDepthLessEqual
                | Register::OutputCoverageMask
                | Register::OutputStencilRef => {
                    self.outputs.registers.entry((register, index)).or_default()
                }
                _ => return Err(Error::UnsupportedRegister(register)),
            }
        };
        for (lane, value) in value.into_iter().enumerate() {
            if operand.components.mask() & (1 << lane) != 0 {
                register[lane] = value;
            }
        }
        Ok(())
    }
}
