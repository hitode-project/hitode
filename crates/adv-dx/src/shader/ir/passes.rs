//! Semantic lowering shared by shader execution and emission.

pub(crate) mod variant;

use super::*;
use crate::shader::analysis::{Reflection, analyze};

pub(crate) fn prepare(module: &Module) -> Result<(Module, Reflection), Error> {
    verify(module)?;
    let mut reflection = analyze(module);
    let mut module = module.clone();
    if inline_calls(&mut module, reflection.temporary_count)? {
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

fn static_label(statement: &Statement) -> Option<u64> {
    let Statement::Execute(op) = statement else {
        return None;
    };
    match (op.opcode, op.sources.first()?.register()?) {
        (Opcode::Label, (Register::Label, [Index::Direct(label)])) => Some(*label),
        _ => None,
    }
}

fn inline_calls(module: &mut Module, flag: u32) -> Result<bool, Error> {
    let mut labels = module
        .body
        .iter()
        .enumerate()
        .filter_map(|(index, stmt)| static_label(stmt).map(|label| (index, label)))
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
        let routine = *self
            .routines
            .get(label)
            .ok_or(Error::Invalid("call target"))?;
        if self.active.len() == crate::shader::MAX_FLOW_DEPTH {
            return Ok(Vec::new());
        }
        if self.active.contains(label) {
            return Err(Error::Invalid("recursive call"));
        }
        self.active.push(*label);
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
