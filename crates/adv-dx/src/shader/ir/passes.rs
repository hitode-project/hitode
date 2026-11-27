//! Semantic lowering shared by shader execution and emission.

mod interface_members;
pub(crate) mod variant;

use super::*;
use crate::shader::analysis::{Reflection, analyze};

pub(crate) fn prepare(module: &Module) -> Result<(Module, Reflection), Error> {
    verify(module)?;
    let mut reflection = analyze(module);
    let mut module = module.clone();
    if inline_calls(&mut module, reflection.temporary_count, None)? {
        reflection = analyze(&module);
    }
    flush_denormals(&mut module, &reflection);
    lower_half_conversion(&mut module, &reflection);
    Ok((module, reflection))
}

pub fn flush_denormals(module: &mut Module, _: &Reflection) {
    instructions(&mut module.body, &mut |instruction| {
        use Opcode::*;
        if matches!(
            instruction.opcode,
            Add | Div
                | Dp2
                | Dp3
                | Dp4
                | Eq
                | Exp
                | Frc
                | Ftoi
                | Ftou
                | Ge
                | Log
                | Lt
                | Mad
                | Min
                | Max
                | Mul
                | Ne
                | RoundNe
                | RoundNi
                | RoundPi
                | RoundZ
                | Rsq
                | Sqrt
                | Sincos
                | Rcp
                | F32ToF16
                | F32ToF16Rtz
                | DerivRtx
                | DerivRty
                | DerivRtxCoarse
                | DerivRtxFine
                | DerivRtyCoarse
                | DerivRtyFine
        ) {
            instruction.flush_inputs = true;
            instruction.flush_outputs = instruction.opcode.types().0.contains(&ValueType::Float);
        }
        if instruction.saturate {
            instruction.flush_outputs = true;
        }
    });
}

pub fn lower_half_conversion(module: &mut Module, _: &Reflection) {
    instructions(&mut module.body, &mut |instruction| {
        if instruction.opcode == Opcode::F32ToF16 {
            instruction.opcode = Opcode::F32ToF16Rtz;
        }
    });
}

fn instructions(block: &mut Block, f: &mut impl FnMut(&mut Instruction)) {
    for statement in block {
        match statement {
            Statement::Execute(instruction) => f(instruction),
            Statement::If {
                test,
                accept,
                reject,
            } => {
                f(test);
                instructions(accept, f);
                if let Some(reject) = reject {
                    instructions(reject, f);
                }
            }
            Statement::Loop(body) => instructions(body, f),
            Statement::Switch { cases, .. } => {
                for case in cases {
                    instructions(&mut case.body, f);
                }
            }
            Statement::Declare(_)
            | Statement::ImmediateConstants(_)
            | Statement::CustomData { .. } => {}
        }
    }
}

const FUNCTION_BODY: u64 = 1 << 32;

pub struct InterfaceBinding {
    pub table: u32,
    pub offsets: Vector,
    pub sizes: [u32; 3],
}

/// Replace interface calls and link the members used by the selected instances.
pub fn specialize_interfaces(
    module: &Module,
    bindings: &[InterfaceBinding],
) -> Result<Module, Error> {
    verify(module)?;
    let mut module = module.clone();
    let temporary_count = analyze(&module).temporary_count;
    let tables: Vec<_> = bindings.iter().map(|binding| binding.table).collect();
    inline_calls(&mut module, temporary_count, Some(&tables))?;
    module.body.retain(|statement| !matches!(statement, Statement::Declare(decl)
        if matches!(decl.opcode, Opcode::DclInterface | Opcode::DclFunctionBody | Opcode::DclFunctionTable)));
    interface_members::lower(&mut module, bindings)?;
    verify(&module)?;
    Ok(module)
}

fn static_label(statement: &Statement, linkage: bool) -> Option<u64> {
    let Statement::Execute(op) = statement else {
        return None;
    };
    match (op.opcode, op.sources.first()?.register()?) {
        (Opcode::Label, (Register::Label, [Index::Direct(label)])) => Some(*label),
        (Opcode::Label, (Register::FunctionBody, [Index::Direct(body)])) if linkage => {
            Some(FUNCTION_BODY | *body)
        }
        _ => None,
    }
}

fn inline_calls(module: &mut Module, flag: u32, tables: Option<&[u32]>) -> Result<bool, Error> {
    let mut labels = module
        .body
        .iter()
        .enumerate()
        .filter_map(|(index, stmt)| {
            static_label(stmt, tables.is_some()).map(|label| (index, label))
        })
        .peekable();
    let Some(&(entry_end, _)) = labels.peek() else {
        return Ok(false);
    };
    let mut routines = std::collections::HashMap::new();
    while let Some((start, label)) = labels.next() {
        let end = labels.peek().map_or(module.body.len(), |&(index, _)| index);
        routines.insert(label, &module.body[start + 1..end]);
    }
    let mut inliner = CallInliner {
        routines,
        tables,
        declarations: &module.body[..entry_end],
        flag: OperandValue::Register {
            register: Register::Temp,
            indices: vec![Index::Direct(u64::from(flag))],
        },
        active: Vec::new(),
        remaining: crate::shader::MAX_SHADER_TOKENS,
    };
    let mut body = inliner.block(&module.body[..entry_end])?;
    if let Some(Statement::Declare(decl)) = body
        .iter_mut()
        .find(|stmt| matches!(stmt, Statement::Declare(decl) if decl.opcode == Opcode::DclTemps))
    {
        decl.data[0] = flag + 1;
    } else {
        body.insert(
            0,
            Statement::Declare(Declaration {
                opcode: Opcode::DclTemps,
                operand: None,
                controls: 0,
                data: vec![flag + 1],
                binding: None,
                interpolation: None,
            }),
        );
    }
    module.body = body;
    Ok(true)
}

struct CallInliner<'a> {
    routines: std::collections::HashMap<u64, &'a [Statement]>,
    tables: Option<&'a [u32]>,
    declarations: &'a [Statement],
    flag: OperandValue,
    active: Vec<u64>,
    remaining: usize,
}

impl CallInliner<'_> {
    fn flag(&self, components: Components) -> Operand {
        Operand {
            value: self.flag.clone(),
            components,
            modifier: Modifier::None,
            precision: Precision::Default,
            non_uniform: false,
        }
    }

    fn set_flag(&self, value: u32) -> Statement {
        Statement::Execute(call_instruction(
            Opcode::Mov,
            vec![self.flag(Components::Mask(1))],
            vec![Operand {
                value: OperandValue::Immediate([value; 4]),
                components: Components::Scalar,
                modifier: Modifier::None,
                precision: Precision::Default,
                non_uniform: false,
            }],
        ))
    }

    // Exiting a switch or a nested loop propagates a subroutine return to
    // the single-iteration loop that represents the call.
    fn return_block(&self) -> Block {
        vec![
            self.set_flag(1),
            Statement::Execute(call_instruction(Opcode::Break, Vec::new(), Vec::new())),
        ]
    }

    fn check_return(&self, output: &mut Block) {
        if !self.active.is_empty() {
            output.push(Statement::Execute(call_instruction(
                Opcode::Breakc,
                Vec::new(),
                vec![self.flag(Components::Select(0))],
            )));
        }
    }

    fn call(&mut self, instruction: &Instruction) -> Result<Block, Error> {
        let Some((Register::Label, [Index::Direct(label)])) = instruction.sources[0].register()
        else {
            return Err(Error::Invalid("call target"));
        };
        self.body(*label)
    }

    fn interface_call(&mut self, instruction: &Instruction) -> Result<Block, Error> {
        let Some((Register::Interface, [Index::Direct(base), index])) =
            instruction.sources[0].register()
        else {
            return Err(Error::Invalid("interface call operand"));
        };
        let invalid = Error::Invalid("interface call target");
        let function = instruction.interface_function.unwrap() as usize;
        let interface = self
            .declarations
            .iter()
            .find_map(|stmt| match stmt {
                Statement::Declare(decl)
                    if decl.opcode == Opcode::DclInterface && u64::from(decl.data[0]) == *base =>
                {
                    Some(decl.data.as_slice())
                }
                _ => None,
            })
            .ok_or(invalid)?;
        let count = crate::shader::dxbc::fields::decode_interface_array_length(interface[2]);
        let base = *base as usize;
        let tables = self
            .tables
            .unwrap()
            .get(base..base + count as usize)
            .ok_or(invalid)?;
        let body = |table| {
            if !interface[3..].contains(&table) {
                return Err(invalid);
            }
            self.declarations
                .iter()
                .find_map(|stmt| match stmt {
                    Statement::Declare(decl)
                        if decl.opcode == Opcode::DclFunctionTable && decl.data[0] == table =>
                    {
                        decl.data.get(2 + function)
                    }
                    _ => None,
                })
                .copied()
                .map(|id| FUNCTION_BODY | u64::from(id))
                .ok_or(invalid)
        };
        match index {
            Index::Direct(index) => self.body(body(*tables.get(*index as usize).ok_or(invalid)?)?),
            Index::Relative { base, register } => {
                let targets = tables
                    .iter()
                    .copied()
                    .map(body)
                    .collect::<Result<Vec<_>, _>>()?;
                let cases = targets
                    .into_iter()
                    .enumerate()
                    .map(|(index, target)| {
                        let mut body = self.body(target)?;
                        body.push(Statement::Execute(call_instruction(
                            Opcode::Break,
                            Vec::new(),
                            Vec::new(),
                        )));
                        Ok(Case {
                            value: Some((index as u32).wrapping_sub(*base as u32)),
                            body,
                        })
                    })
                    .collect::<Result<_, Error>>()?;
                Ok(vec![Statement::Switch {
                    selector: (**register).clone(),
                    cases,
                }])
            }
        }
    }

    fn body(&mut self, label: u64) -> Result<Block, Error> {
        let routine = *self
            .routines
            .get(&label)
            .ok_or(Error::Invalid("call target"))?;
        if self.active.len() == crate::shader::MAX_FLOW_DEPTH {
            return Ok(Vec::new());
        }
        if self.active.contains(&label) {
            return Err(Error::Invalid("recursive call"));
        }
        self.active.push(label);
        let mut body = self.block(routine)?;
        self.active.pop();
        body.push(Statement::Execute(call_instruction(
            Opcode::Break,
            Vec::new(),
            Vec::new(),
        )));
        Ok(vec![
            self.set_flag(0),
            Statement::Loop(body),
            self.set_flag(0),
        ])
    }

    fn block(&mut self, statements: &[Statement]) -> Result<Block, Error> {
        let mut output = Vec::new();
        for statement in statements {
            self.remaining = self.remaining.checked_sub(1).ok_or(Error::ExecutionLimit)?;
            match statement {
                Statement::Execute(op) if matches!(op.opcode, Opcode::Call | Opcode::Callc) => {
                    let body = self.call(op)?;
                    if op.opcode == Opcode::Callc {
                        output.push(call_if(op, 1, body));
                    } else {
                        output.extend(body);
                    }
                }
                Statement::Execute(op)
                    if self.tables.is_some() && op.opcode == Opcode::InterfaceCall =>
                {
                    output.extend(self.interface_call(op)?);
                }
                Statement::Execute(op) if !self.active.is_empty() && op.opcode == Opcode::Ret => {
                    output.extend(self.return_block());
                }
                Statement::Execute(op) if !self.active.is_empty() && op.opcode == Opcode::Retc => {
                    output.push(call_if(op, 0, self.return_block()));
                }
                Statement::If {
                    test,
                    accept,
                    reject,
                } => output.push(Statement::If {
                    test: test.clone(),
                    accept: self.block(accept)?,
                    reject: reject.as_ref().map(|body| self.block(body)).transpose()?,
                }),
                Statement::Loop(body) => {
                    output.push(Statement::Loop(self.block(body)?));
                    self.check_return(&mut output);
                }
                Statement::Switch { selector, cases } => {
                    output.push(Statement::Switch {
                        selector: selector.clone(),
                        cases: cases
                            .iter()
                            .map(|case| {
                                Ok(Case {
                                    value: case.value,
                                    body: self.block(&case.body)?,
                                })
                            })
                            .collect::<Result<_, Error>>()?,
                    });
                    self.check_return(&mut output);
                }
                other => output.push(other.clone()),
            }
        }
        Ok(output)
    }
}

fn call_if(op: &Instruction, source: usize, body: Block) -> Statement {
    let mut test = call_instruction(Opcode::If, Vec::new(), vec![op.sources[source].clone()]);
    test.test_nonzero = op.test_nonzero;
    Statement::If {
        test,
        accept: body,
        reject: None,
    }
}

fn call_instruction(
    opcode: Opcode,
    destinations: Vec<Operand>,
    sources: Vec<Operand>,
) -> Instruction {
    Instruction {
        opcode,
        destinations,
        sources,
        saturate: false,
        test_nonzero: true,
        precise: 0,
        controls: 0,
        resource: ResourceOperands::default(),
        interface_function: None,
        flush_inputs: false,
        flush_outputs: false,
    }
}
