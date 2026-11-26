//! Semantic lowering shared by shader execution and emission.

use super::*;
use crate::shader::analysis::{Reflection, analyze};

pub(crate) fn prepare(module: &Module) -> Result<(Module, Reflection), Error> {
    verify(module)?;
    let reflection = analyze(module);
    let mut module = module.clone();
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
