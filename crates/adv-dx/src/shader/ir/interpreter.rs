//! Scalar execution before variant lowering. An error means the caller has no bounds proof.

mod alu;

use super::*;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::ops::ControlFlow;

/// Register values by `(register, index)`. A shader touches few input and
/// output registers, so a scan beats hashing and clearing keeps the storage.
#[derive(Clone, Debug, Default)]
pub struct Registers(Vec<((Register, u32), Vector)>);

impl Registers {
    pub fn get(&self, key: &(Register, u32)) -> Option<&Vector> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: (Register, u32), value: Vector) -> Option<Vector> {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some((_, slot)) => Some(std::mem::replace(slot, value)),
            None => {
                self.0.push((key, value));
                None
            }
        }
    }

    fn entry(&mut self, key: (Register, u32)) -> &mut Vector {
        let n = match self.0.iter().position(|(k, _)| *k == key) {
            Some(n) => n,
            None => {
                self.0.push((key, [0; 4]));
                self.0.len() - 1
            }
        };
        &mut self.0[n].1
    }

    pub fn iter(&self) -> impl Iterator<Item = (&(Register, u32), &Vector)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

impl std::ops::Index<&(Register, u32)> for Registers {
    type Output = Vector;

    fn index(&self, key: &(Register, u32)) -> &Vector {
        self.get(key).expect("register not present")
    }
}

impl<const N: usize> From<[((Register, u32), Vector); N]> for Registers {
    fn from(entries: [((Register, u32), Vector); N]) -> Self {
        let mut registers = Self::default();
        for (key, value) in entries {
            registers.insert(key, value);
        }
        registers
    }
}

#[derive(Clone, Debug, Default)]
pub struct Inputs {
    pub registers: Registers,
}

pub type Constants = HashMap<u32, Vec<Vector>>;

/// Constant buffer rows by slot; a row outside the buffer reads as zero.
pub trait ConstantBuffers {
    fn row(&self, slot: u32, row: u32) -> Vector;
}

impl ConstantBuffers for Constants {
    fn row(&self, slot: u32, row: u32) -> Vector {
        self.get(&slot)
            .and_then(|rows| rows.get(row as usize))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Outputs {
    pub registers: Registers,
    pub discarded: bool,
}

/// Storage one run fills, kept across runs of the same or another program.
#[derive(Clone, Debug, Default)]
pub struct Scratch {
    temps: Vec<Vector>,
    /// Indexable temporaries; only the first `arrays_live` are declared in this run.
    arrays: Vec<(u32, Vec<Vector>)>,
    arrays_live: usize,
    pub outputs: Outputs,
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
    /// Supplies the derivative of a source across the caller's pixel quad.
    fn derivative(&mut self, instruction: &Instruction, _: Vector) -> Result<Vector, Error> {
        Err(Error::UnsupportedOpcode(instruction.opcode))
    }

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
    constants: &impl ConstantBuffers,
    resources: &mut impl Resources,
) -> Result<Outputs, Error> {
    Prepared::new(module)?.run(inputs, constants, resources)
}

/// A module after the shared semantic passes, for repeated execution.
#[derive(Clone, Debug)]
pub struct Prepared {
    module: Module,
    temporary_count: u32,
    /// The entry block ends here; shared subroutine bodies follow.
    entry: usize,
    routines: HashMap<u64, std::ops::Range<usize>>,
    fuel: usize,
}

impl Prepared {
    pub fn new(module: &Module) -> Result<Self, Error> {
        if matches!(module.stage, Stage::Geometry | Stage::Hull | Stage::Domain) {
            return Err(Error::UnsupportedStage(module.stage));
        }
        let (module, reflection) = passes::prepare(module)?;
        if reflection.doubles {
            return Err(Error::DoublePrecision);
        }
        let (entry, routines) = passes::subroutines(&module.body);
        Ok(Self {
            module,
            temporary_count: reflection.temporary_count,
            entry,
            routines: routines.into_iter().collect(),
            fuel: crate::shader::MAX_EXECUTION_STEPS,
        })
    }

    /// Statements one run may execute before it stops with `ExecutionLimit`.
    pub fn with_fuel(mut self, fuel: usize) -> Self {
        self.fuel = fuel;
        self
    }

    pub fn run(
        &self,
        inputs: &Inputs,
        constants: &impl ConstantBuffers,
        resources: &mut impl Resources,
    ) -> Result<Outputs, Error> {
        let mut scratch = Scratch::default();
        self.run_in(inputs, constants, resources, &mut scratch)?;
        Ok(scratch.outputs)
    }

    /// Run with storage from `scratch`, leaving the result in `scratch.outputs`.
    pub fn run_in<'s>(
        &self,
        inputs: &Inputs,
        constants: &impl ConstantBuffers,
        resources: &mut impl Resources,
        scratch: &'s mut Scratch,
    ) -> Result<&'s Outputs, Error> {
        scratch.temps.clear();
        scratch.temps.resize(self.temporary_count as usize, [0; 4]);
        scratch.arrays_live = 0;
        scratch.outputs.registers.clear();
        scratch.outputs.discarded = false;
        let mut machine = Machine {
            inputs,
            constants,
            resources,
            scratch,
            immediate: &[],
            fuel: self.fuel,
            flush: self.module.model.flushes_denormals(),
            body: &self.module.body,
            routines: &self.routines,
        };
        let _ = machine.block(&self.module.body[..self.entry])?;
        Ok(&scratch.outputs)
    }
}

impl Scratch {
    fn array(&self, array: u32) -> Option<&[Vector]> {
        self.arrays[..self.arrays_live]
            .iter()
            .find(|(n, _)| *n == array)
            .map(|(_, rows)| rows.as_slice())
    }

    fn array_mut(&mut self, array: u32) -> Option<&mut [Vector]> {
        self.arrays[..self.arrays_live]
            .iter_mut()
            .find(|(n, _)| *n == array)
            .map(|(_, rows)| rows.as_mut_slice())
    }

    fn declare_array(&mut self, array: u32, rows: usize) {
        let live = &mut self.arrays[..self.arrays_live];
        let slot = match live.iter().position(|(n, _)| *n == array) {
            Some(n) => n,
            None => {
                if self.arrays_live == self.arrays.len() {
                    self.arrays.push((array, Vec::new()));
                }
                self.arrays_live += 1;
                self.arrays_live - 1
            }
        };
        let (n, values) = &mut self.arrays[slot];
        *n = array;
        values.clear();
        values.resize(rows, [0; 4]);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Branch {
    Return,
    Break,
    Continue,
}

struct Machine<'a, C: ?Sized, R> {
    inputs: &'a Inputs,
    constants: &'a C,
    resources: &'a mut R,
    scratch: &'a mut Scratch,
    immediate: &'a [Vector],
    fuel: usize,
    flush: bool,
    body: &'a [Statement],
    routines: &'a HashMap<u64, std::ops::Range<usize>>,
}

impl<'a, C: ConstantBuffers + ?Sized, R: Resources> Machine<'a, C, R> {
    fn tick(&mut self) -> Result<(), Error> {
        self.fuel = self.fuel.checked_sub(1).ok_or(Error::ExecutionLimit)?;
        Ok(())
    }

    fn block(&mut self, block: &'a [Statement]) -> Result<ControlFlow<Branch>, Error> {
        for statement in block {
            self.tick()?;
            let branch = match statement {
                Statement::Declare(decl) => {
                    if decl.opcode == Opcode::DclIndexableTemp {
                        self.scratch
                            .declare_array(decl.data[0], decl.data[1] as usize);
                    }
                    ControlFlow::Continue(())
                }
                Statement::ImmediateConstants(values) => {
                    self.immediate = values;
                    ControlFlow::Continue(())
                }
                Statement::CustomData { .. } => ControlFlow::Continue(()),
                Statement::Execute(op) if matches!(op.opcode, Opcode::Call | Opcode::Callc) => {
                    self.call(op)?
                }
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

    /// Run a shared subroutine body. Its return resumes the caller; a discard
    /// ends the program.
    fn call(&mut self, op: &Instruction) -> Result<ControlFlow<Branch>, Error> {
        if op.opcode == Opcode::Callc {
            let value = self.read(&op.sources[1], ValueType::Uint, false)?[0];
            if (value != 0) != op.test_nonzero {
                return Ok(ControlFlow::Continue(()));
            }
        }
        let label = passes::call_target(op)?;
        let range = self
            .routines
            .get(&label)
            .ok_or(Error::Invalid("call target"))?
            .clone();
        let body = self.body;
        Ok(match self.block(&body[range])? {
            ControlFlow::Break(Branch::Return) if self.scratch.outputs.discarded => {
                ControlFlow::Break(Branch::Return)
            }
            _ => ControlFlow::Continue(()),
        })
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
            self.scratch.outputs.discarded = op.opcode == Discard;
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
            .collect::<Result<SmallVec<[Vector; 6]>, _>>()?;
        let mut values = if matches!(
            op.opcode,
            DerivRtx | DerivRty | DerivRtxCoarse | DerivRtyCoarse | DerivRtxFine | DerivRtyFine
        ) {
            [self.resources.derivative(op, args[0])?, [0; 4]]
        } else if op.opcode.is_interpolation() {
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
                self.constants.row(slot, row)
            }
            OperandValue::Register {
                register: Register::IndexableTemp,
                indices,
            } => {
                let array = self.index(&indices[0], 1)?;
                let row = self.index(&indices[1], 1)?;
                self.scratch
                    .array(array)
                    .and_then(|rows| rows.get(row as usize))
                    .copied()
                    .unwrap_or_default()
            }
            OperandValue::Register { .. } => {
                let (register, index) = self.location(operand)?;
                match register {
                    Register::Temp => *self
                        .scratch
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
                        .scratch
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
            let value = alu::modify(value, operand.modifier, ty, self.flush);
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
                .scratch
                .array_mut(array)
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
                    .scratch
                    .temps
                    .get_mut(index as usize)
                    .ok_or(Error::MissingInput(register, index))?,
                Register::Output
                | Register::OutputDepth
                | Register::OutputDepthGreaterEqual
                | Register::OutputDepthLessEqual
                | Register::OutputCoverageMask
                | Register::OutputStencilRef => {
                    self.scratch.outputs.registers.entry((register, index))
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
