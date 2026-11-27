//! Link the SM5 this[] table and resolve uniform class-member bindings.

use super::*;
use std::collections::BTreeSet;

pub(super) fn lower(module: &mut Module, instances: &[InterfaceBinding]) -> Result<(), Error> {
    let mut constants = module
        .body
        .iter_mut()
        .find_map(|statement| match statement {
            Statement::ImmediateConstants(values) => Some(values),
            _ => None,
        });
    let first = constants.as_ref().map_or(0, |values| values.len()) as u64;
    let reflection = analyze(module);
    let mut pass = Members {
        instances,
        first,
        indexed_this: false,
        next_temp: reflection.temporary_count,
        bindings: reflection.bindings,
    };
    module.body = pass.block(std::mem::take(&mut module.body))?;
    if let Some(declaration) = module
        .body
        .iter_mut()
        .find_map(|statement| match statement {
            Statement::Declare(declaration) if declaration.opcode == Opcode::DclTemps => {
                Some(declaration)
            }
            _ => None,
        })
    {
        declaration.data[0] = pass.next_temp;
    } else if pass.next_temp != 0 {
        module.body.insert(
            0,
            Statement::Declare(Declaration {
                opcode: Opcode::DclTemps,
                operand: None,
                controls: 0,
                data: vec![pass.next_temp],
                binding: None,
                interpolation: None,
            }),
        );
    }
    if pass.indexed_this {
        constants = module
            .body
            .iter_mut()
            .find_map(|statement| match statement {
                Statement::ImmediateConstants(values) => Some(values),
                _ => None,
            });
        let offsets = instances.iter().map(|instance| instance.offsets);
        if let Some(values) = constants {
            values.extend(offsets);
        } else {
            module
                .body
                .insert(0, Statement::ImmediateConstants(offsets.collect()));
        }
    }
    for binding in pass.bindings {
        let opcode = match binding.kind {
            BindingKind::ConstantBuffer { .. } => Opcode::DclConstantBuffer,
            BindingKind::Sampler(_) => Opcode::DclSampler,
            BindingKind::Typed { .. } => Opcode::DclResource,
            BindingKind::Raw { .. } => Opcode::DclResourceRaw,
            BindingKind::Structured { .. } => Opcode::DclResourceStructured,
        };
        let existing = module
            .body
            .iter_mut()
            .find_map(|statement| match statement {
                Statement::Declare(declaration)
                    if declaration.binding.is_some_and(|prior| {
                        (prior.register, prior.slot) == (binding.register, binding.slot)
                    }) =>
                {
                    Some(declaration)
                }
                _ => None,
            });
        if let Some(existing) = existing {
            existing.binding = Some(binding);
        } else {
            module.body.insert(
                0,
                Statement::Declare(Declaration {
                    opcode,
                    operand: None,
                    controls: 0,
                    data: vec![
                        0;
                        usize::from(matches!(
                            opcode,
                            Opcode::DclResource | Opcode::DclResourceStructured
                        ))
                    ],
                    binding: Some(binding),
                    interpolation: None,
                }),
            );
        }
    }
    Ok(())
}

type Known = std::collections::HashMap<u64, [Option<u32>; 4]>;

struct Members<'a> {
    instances: &'a [InterfaceBinding],
    first: u64,
    indexed_this: bool,
    next_temp: u32,
    bindings: Vec<Binding>,
}

impl Members<'_> {
    fn operand(&mut self, operand: &mut Operand, known: &Known) {
        let indices: &mut [Index] = match &mut operand.value {
            OperandValue::Register { indices, .. } => indices,
            OperandValue::Constant { slot, byte_offset } => {
                self.index(slot, 1, known);
                self.index(byte_offset, 16, known);
                return;
            }
            _ => return,
        };
        for index in indices {
            self.index(index, 1, known);
        }
        if let OperandValue::Register {
            register: Register::ThisPointer,
            indices,
        } = &mut operand.value
            && let [index] = indices.as_mut_slice()
        {
            match index {
                Index::Direct(slot) => {
                    operand.value = OperandValue::Immediate(
                        self.instances
                            .get(*slot as usize)
                            .map_or([0; 4], |instance| instance.offsets),
                    );
                }
                Index::Relative { base, .. } => {
                    *base += self.first;
                    self.indexed_this = true;
                    if let OperandValue::Register { register, .. } = &mut operand.value {
                        *register = Register::ImmediateConstantBuffer;
                    }
                }
            }
        }
    }

    fn index(&mut self, index: &mut Index, stride: u32, known: &Known) {
        if let Index::Relative { base, register } = index {
            self.operand(register, known);
            if let Some(value) = literal(register, known)[0] {
                *index = Index::Direct(
                    u64::from((*base as u32 / stride).wrapping_add(value)) * u64::from(stride),
                );
            }
        }
    }

    fn block(&mut self, statements: Block) -> Result<Block, Error> {
        let mut output = Vec::new();
        let mut known = Known::new();
        for mut statement in statements {
            match &mut statement {
                Statement::Execute(op) => {
                    let members: Vec<_> = op
                        .sources
                        .iter()
                        .enumerate()
                        .filter_map(|(index, operand)| {
                            let (kind, slot) = binding_slot(operand)?;
                            matches!(slot, Index::Relative { .. }).then_some((index, kind))
                        })
                        .collect();
                    for operand in op.destinations.iter_mut().chain(&mut op.sources) {
                        self.operand(operand, &known);
                    }
                    let values = if op.opcode == Opcode::Mov && !op.saturate {
                        literal(&op.sources[0], &known)
                    } else {
                        [None; 4]
                    };
                    for destination in &op.destinations {
                        if let Some((Register::Temp, [Index::Direct(register)])) =
                            destination.register()
                        {
                            let lanes = known.entry(*register).or_default();
                            for (i, value) in values.into_iter().enumerate() {
                                if destination.components.mask() & (1 << i) != 0 {
                                    lanes[i] = value;
                                }
                            }
                        }
                    }
                    if !members.is_empty() {
                        output.push(self.select(op.clone(), &members)?);
                        continue;
                    }
                }
                Statement::If {
                    test,
                    accept,
                    reject,
                } => {
                    self.condition(&mut test.sources[0], &known, &mut output)?;
                    *accept = self.block(std::mem::take(accept))?;
                    if let Some(reject) = reject {
                        *reject = self.block(std::mem::take(reject))?;
                    }
                    known.clear();
                }
                Statement::Loop(body) => {
                    *body = self.block(std::mem::take(body))?;
                    known.clear();
                }
                Statement::Switch { selector, cases } => {
                    self.condition(selector, &known, &mut output)?;
                    for case in cases {
                        case.body = self.block(std::mem::take(&mut case.body))?;
                    }
                    known.clear();
                }
                _ => (),
            }
            output.push(statement);
        }
        Ok(output)
    }

    fn condition(
        &mut self,
        source: &mut Operand,
        known: &Known,
        output: &mut Block,
    ) -> Result<(), Error> {
        let indexed_buffer = matches!(
            binding_slot(source),
            Some((Register::ConstantBuffer, Index::Relative { .. }))
        );
        self.operand(source, known);
        if !indexed_buffer {
            return Ok(());
        }
        if let Some((Register::ConstantBuffer, index)) = binding_slot(source)
            && let Some(slot) = index.direct()
            && let Some(kind) = self.kind(
                Register::ConstantBuffer,
                slot,
                Opcode::Mov,
                &ResourceOperands::default(),
            )?
        {
            self.declare(Register::ConstantBuffer, slot, Some(kind));
            return Ok(());
        }
        let mut destination = Operand {
            value: OperandValue::Register {
                register: Register::Temp,
                indices: vec![Index::Direct(u64::from(self.next_temp))],
            },
            components: Components::Mask(1),
            ..immediate(0)
        };
        self.next_temp += 1;
        // Preserve source modifiers on the consumer, whose operand type may
        // differ from Mov's typeless copy.
        let mut input = source.clone();
        input.modifier = Modifier::None;
        input.precision = Precision::Default;
        output.push(self.select(
            call_instruction(Opcode::Mov, vec![destination.clone()], vec![input]),
            &[(0, Register::ConstantBuffer)],
        )?);
        destination.components = Components::Select(0);
        destination.modifier = source.modifier;
        destination.precision = source.precision;
        *source = destination;
        Ok(())
    }

    fn select(
        &mut self,
        op: Instruction,
        members: &[(usize, Register)],
    ) -> Result<Statement, Error> {
        let Some((&(source, register), remaining)) = members.split_first() else {
            return Ok(Statement::Execute(op));
        };
        let (_, index) = binding_slot(&op.sources[source]).unwrap();
        if let Some(slot) = index.direct() {
            if let Some(kind) = self.kind(register, slot, op.opcode, &op.resource)? {
                self.declare(register, slot, Some(kind));
                return self.select(op, remaining);
            }
            return self.unbound(op, source, register, remaining);
        }
        let Index::Relative {
            base,
            register: selector,
        } = index
        else {
            unreachable!()
        };
        let base = *base as u32;
        let selector = (**selector).clone();
        let mut slots = BTreeSet::new();
        for instance in self.instances {
            let [cb, _, texture, sampler] = instance.offsets;
            let [constants, textures, samplers] = instance.sizes;
            let range = match register {
                Register::ConstantBuffer if constants != 0 => cb..cb + 1,
                Register::Resource => texture..texture + textures,
                Register::Sampler => sampler..sampler + samplers,
                _ => continue,
            };
            slots.extend(range);
        }
        slots.extend(
            self.bindings
                .iter()
                .filter(|binding| binding.register == register)
                .map(|binding| binding.slot),
        );
        let mut cases = Vec::with_capacity(slots.len() + 1);
        for slot in slots {
            if let Some(kind) = self.kind(register, slot, op.opcode, &op.resource)? {
                self.declare(register, slot, Some(kind));
                let mut selected = op.clone();
                let index = match &mut selected.sources[source].value {
                    OperandValue::Constant { slot, .. } => slot,
                    OperandValue::Register { indices, .. } => &mut indices[0],
                    _ => unreachable!(),
                };
                *index = Index::Direct(u64::from(slot));
                cases.push(case(
                    Some(slot.wrapping_sub(base)),
                    self.select(selected, remaining)?,
                ));
            }
        }
        cases.push(case(None, self.unbound(op, source, register, remaining)?));
        Ok(Statement::Switch { selector, cases })
    }

    fn unbound(
        &mut self,
        mut op: Instruction,
        source: usize,
        register: Register,
        remaining: &[(usize, Register)],
    ) -> Result<Statement, Error> {
        if register == Register::ConstantBuffer {
            op.sources[source].value = OperandValue::Immediate([0; 4]);
            self.select(op, remaining)
        } else {
            Ok(Statement::Execute(call_instruction(
                Opcode::Mov,
                op.destinations,
                vec![immediate(0)],
            )))
        }
    }

    fn kind(
        &self,
        register: Register,
        slot: u32,
        opcode: Opcode,
        resource: &ResourceOperands,
    ) -> Result<Option<BindingKind>, Error> {
        let prior = self
            .bindings
            .iter()
            .find(|binding| (binding.register, binding.slot) == (register, slot))
            .map(|binding| binding.kind);
        Ok(match register {
            Register::ConstantBuffer if slot < 14 => {
                let bytes = self
                    .instances
                    .iter()
                    .filter(|instance| instance.offsets[0] == slot && instance.sizes[0] != 0)
                    .map(|instance| (instance.offsets[1] + instance.sizes[0]) * 16)
                    .max()
                    .unwrap_or(0);
                let prior = match prior {
                    Some(BindingKind::ConstantBuffer { bytes, .. }) => bytes,
                    _ => 0,
                };
                Some(BindingKind::ConstantBuffer {
                    bytes: bytes.max(prior),
                    dynamic_indexing: true,
                })
            }
            Register::Resource if slot < 128 => match resource.dimension {
                Some((ResourceDimension::RawBuffer, _)) => Some(BindingKind::Raw { bytes: 0 }),
                Some((ResourceDimension::StructuredBuffer, stride)) => {
                    Some(BindingKind::Structured {
                        stride,
                        elements: 0,
                    })
                }
                Some((dimension, _)) => {
                    let returns = resource
                        .returns
                        .ok_or(Error::Invalid("class texture return type"))?;
                    Some(BindingKind::Typed {
                        dimension,
                        samples: 0,
                        returns,
                    })
                }
                None => prior,
            },
            Register::Sampler if slot < 16 => Some(BindingKind::Sampler(
                if matches!(
                    opcode,
                    Opcode::SampleC | Opcode::SampleCLz | Opcode::Gather4C | Opcode::Gather4PoC
                ) {
                    SamplerMode::Comparison
                } else {
                    SamplerMode::Default
                },
            )),
            _ => None,
        })
    }

    fn declare(&mut self, register: Register, slot: u32, kind: Option<BindingKind>) {
        let Some(kind) = kind else { return };
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .find(|binding| (binding.register, binding.slot) == (register, slot))
        {
            binding.kind = kind;
        } else {
            self.bindings.push(Binding {
                register,
                slot,
                kind,
                flags: 0,
            });
        }
    }
}

fn binding_slot(operand: &Operand) -> Option<(Register, &Index)> {
    match &operand.value {
        OperandValue::Constant { slot, .. } => Some((Register::ConstantBuffer, slot)),
        OperandValue::Register {
            register: register @ (Register::Resource | Register::Sampler),
            indices,
        } => Some((*register, indices.first()?)),
        _ => None,
    }
}

fn literal(operand: &Operand, known: &Known) -> [Option<u32>; 4] {
    if operand.modifier != Modifier::None || operand.precision != Precision::Default {
        return [None; 4];
    }
    let values = match &operand.value {
        OperandValue::Immediate(values) => values.map(Some),
        OperandValue::Register {
            register: Register::Temp,
            indices,
        } => {
            let [Index::Direct(register)] = indices.as_slice() else {
                return [None; 4];
            };
            known.get(register).copied().unwrap_or_default()
        }
        _ => [None; 4],
    };
    operand
        .components
        .swizzle()
        .map(|lane| values[lane as usize])
}

fn immediate(value: u32) -> Operand {
    Operand {
        value: OperandValue::Immediate([value; 4]),
        components: Components::Scalar,
        modifier: Modifier::None,
        precision: Precision::Default,
        non_uniform: false,
    }
}

fn case(value: Option<u32>, statement: Statement) -> Case {
    Case {
        value,
        body: vec![
            statement,
            Statement::Execute(call_instruction(Opcode::Break, Vec::new(), Vec::new())),
        ],
    }
}
