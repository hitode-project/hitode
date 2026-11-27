//! UAV-result dependencies in a pixel shader with calls inlined.

use super::*;
use crate::shader::ir::Opcode::*;

type Taint = HashMap<(Register, Option<u32>), u8>;

pub(crate) fn effects_depend_on_uav(module: &Module) -> bool {
    let end = block(&module.body, Taint::new(), false, &mut None, &mut None);
    end.map_or(true, |state| {
        state.is_some_and(|state| tainted_output(&state))
    })
}

// Each join retains lanes tainted on any incoming path. A loop rechecks its
// body as new taint reaches its header through a back edge.
fn merge(target: &mut Taint, source: Taint) -> bool {
    let mut changed = false;
    for (key, lanes) in source {
        let current = target.entry(key).or_default();
        changed |= lanes & !*current != 0;
        *current |= lanes;
    }
    changed
}

fn join(target: &mut Option<Taint>, source: Option<Taint>) {
    if let Some(source) = source {
        merge(target.get_or_insert_with(Taint::new), source);
    }
}

fn block(
    body: &Block,
    mut state: Taint,
    control: bool,
    breaks: &mut Option<Taint>,
    continues: &mut Option<Taint>,
) -> Result<Option<Taint>, ()> {
    for statement in body {
        match statement {
            Statement::Execute(op) => {
                if matches!(
                    op.opcode,
                    Break | Breakc | Continue | Continuec | Ret | Retc | Discard
                ) {
                    if control
                        || op
                            .sources
                            .first()
                            .is_some_and(|test| reads(&state, test, 1))
                    {
                        return Err(());
                    }
                    match op.opcode {
                        Break | Breakc => join(breaks, Some(state.clone())),
                        Continue | Continuec => join(continues, Some(state.clone())),
                        Ret | Retc if tainted_output(&state) => return Err(()),
                        _ => {}
                    }
                    if matches!(op.opcode, Break | Continue | Ret) {
                        return Ok(None);
                    }
                } else {
                    instruction(op, &mut state, control)?;
                }
            }
            Statement::If {
                test,
                accept,
                reject,
            } => {
                let branch = control || reads(&state, &test.sources[0], 1);
                let mut next = block(accept, state.clone(), branch, breaks, continues)?;
                let otherwise = match reject {
                    Some(reject) => block(reject, state, branch, breaks, continues)?,
                    None => Some(state),
                };
                join(&mut next, otherwise);
                let Some(next) = next else { return Ok(None) };
                state = next;
            }
            Statement::Loop(body) => {
                let mut exits = None;
                loop {
                    let mut back = None;
                    let end = block(body, state.clone(), control, &mut exits, &mut back)?;
                    join(&mut back, end);
                    if !back.is_some_and(|back| merge(&mut state, back)) {
                        break;
                    }
                }
                let Some(next) = exits else { return Ok(None) };
                state = next;
            }
            Statement::Switch { selector, cases } => {
                let branch = control || reads(&state, selector, 1);
                let mut exits = None;
                let mut fallthrough = None;
                for case in cases {
                    let mut entry = state.clone();
                    if let Some(previous) = fallthrough {
                        merge(&mut entry, previous);
                    }
                    fallthrough = block(&case.body, entry, branch, &mut exits, continues)?;
                }
                join(&mut exits, fallthrough);
                if cases.iter().all(|case| case.value.is_some()) {
                    join(&mut exits, Some(state));
                }
                let Some(next) = exits else { return Ok(None) };
                state = next;
            }
            _ => {}
        }
    }
    Ok(Some(state))
}

fn instruction(op: &Instruction, state: &mut Taint, control: bool) -> Result<(), ()> {
    let (destinations, sources) = op.opcode.types();
    let uav_effect = writes_uav(op);
    let uses_uav = op.sources.iter().any(is_uav);
    let result = uav_effect || uses_uav && matches!(op.opcode, LdUavTyped | LdRaw | LdStructured);
    let depends = |lanes| op.sources.iter().any(|source| reads(state, source, lanes));
    if uav_effect
        && (control || depends(15) || op.destinations.iter().any(|dst| relative(state, dst)))
    {
        return Err(());
    }
    // Read every source before committing any destination of a multi-result op.
    let writes = op
        .destinations
        .iter()
        .zip(destinations)
        .filter_map(|(dst, ty)| {
            let key = register_key(dst)?;
            if *ty == ValueType::Binding || key.0 == Register::Null {
                return None;
            }
            let mask = dst.components.mask();
            let tainted = if result || control || relative(state, dst) {
                mask
            } else {
                (0..4).fold(0, |tainted, lane| {
                    let bit = 1 << lane;
                    let inputs = match op.opcode {
                        Dp2 => 3,
                        Dp3 => 7,
                        Dp4 => 15,
                        _ if sources.contains(&ValueType::Binding)
                            || op.opcode.is_interpolation() =>
                        {
                            15
                        }
                        _ => bit,
                    };
                    if mask & bit != 0 && depends(inputs) {
                        tainted | bit
                    } else {
                        tainted
                    }
                })
            };
            Some((key, mask, tainted))
        })
        .collect::<smallvec::SmallVec<[_; 2]>>();
    for (key, mask, tainted) in writes {
        let old = state.get(&key).copied().unwrap_or_default();
        // An indexed write may leave other entries of its register array intact.
        let retained = if key.0 == Register::IndexableTemp || key.1.is_none() {
            old
        } else {
            old & !mask
        };
        let lanes = retained | tainted;
        if lanes == 0 {
            state.remove(&key);
        } else {
            state.insert(key, lanes);
        }
    }
    Ok(())
}

fn reads(state: &Taint, operand: &Operand, lanes: u8) -> bool {
    if relative(state, operand) {
        return true;
    }
    let Some((register, slot)) = register_key(operand) else {
        return false;
    };
    let selected = super::source_components(operand, lanes);
    let mask = if let Some(slot) = slot {
        state
            .get(&(register, Some(slot)))
            .copied()
            .unwrap_or_default()
            | state.get(&(register, None)).copied().unwrap_or_default()
    } else {
        state
            .iter()
            .filter(|((kind, _), _)| *kind == register)
            .fold(0, |mask, (_, lanes)| mask | lanes)
    };
    mask & selected != 0
}

fn relative(state: &Taint, operand: &Operand) -> bool {
    let index = |index: &Index| match index {
        Index::Relative { register, .. } => reads(state, register, 1),
        Index::Direct(_) => false,
    };
    match &operand.value {
        OperandValue::Register { indices, .. } => indices.iter().any(index),
        OperandValue::Constant { slot, byte_offset } => index(slot) || index(byte_offset),
        _ => false,
    }
}

fn register_key(operand: &Operand) -> Option<(Register, Option<u32>)> {
    let (register, indices) = operand.register()?;
    Some((register, indices.first().map_or(Some(0), Index::direct)))
}

fn tainted_output(state: &Taint) -> bool {
    state.keys().any(|(register, _)| {
        matches!(
            register,
            Register::Output
                | Register::OutputDepth
                | Register::OutputDepthGreaterEqual
                | Register::OutputDepthLessEqual
                | Register::OutputCoverageMask
                | Register::OutputStencilRef
        )
    })
}

fn is_uav(operand: &Operand) -> bool {
    matches!(operand.register(), Some((Register::UnorderedAccessView, _)))
}

pub(crate) fn writes_uav(op: &Instruction) -> bool {
    op.destinations.iter().any(is_uav) || op.opcode.is_counter()
}

#[cfg(all(test, feature = "gpu"))]
mod tests {
    use super::*;
    use crate::shader::dxbc::{parse, pixel_atomic_fixture};
    use Register::{Output, OutputDepth, Temp};

    // spec: GFX-STAGE-4
    #[test]
    fn uav_results_reach_observable_operations() {
        let mut module = parse(&pixel_atomic_fixture(None, false, false)).unwrap();
        assert!(!effects_depend_on_uav(&module));
        let atomic = module
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Statement::Execute(op) if op.opcode == ImmAtomicIadd => Some(op.clone()),
                _ => None,
            })
            .unwrap();
        let result = atomic.destinations[0].clone();
        let uav = atomic.destinations[1].clone();
        let zero = atomic.sources[0].clone();
        let operation = |opcode, destinations, sources| Instruction {
            opcode,
            destinations,
            sources,
            ..atomic.clone()
        };
        let mov = |register, value| {
            let mut dst = result.clone();
            let indices = if register == OutputDepth {
                vec![]
            } else {
                vec![Index::Direct(0)]
            };
            dst.value = OperandValue::Register { register, indices };
            Statement::Execute(operation(Mov, vec![dst], vec![value]))
        };
        let mut test = result.clone();
        test.components = Components::Select(0);
        let store = |address, value| {
            Statement::Execute(operation(
                StoreStructured,
                vec![uav.clone()],
                vec![address, zero.clone(), value],
            ))
        };
        let load = operation(
            LdStructured,
            vec![result.clone()],
            vec![zero.clone(), zero.clone(), uav.clone()],
        );
        for source in [atomic.clone(), load] {
            for sink in [
                mov(Output, test.clone()),
                mov(OutputDepth, test.clone()),
                Statement::Execute(operation(Discard, vec![], vec![test.clone()])),
                store(test.clone(), zero.clone()),
                store(zero.clone(), test.clone()),
                Statement::If {
                    test: operation(If, vec![], vec![test.clone()]),
                    accept: vec![store(zero.clone(), zero.clone())],
                    reject: None,
                },
            ] {
                module.body = vec![Statement::Execute(source.clone()), sink];
                assert!(effects_depend_on_uav(&module));
            }
        }

        module.body = vec![
            Statement::Execute(atomic.clone()),
            mov(Temp, zero.clone()),
            store(zero.clone(), test.clone()),
        ];
        assert!(!effects_depend_on_uav(&module));

        module.body = vec![Statement::Loop(vec![
            store(zero.clone(), test),
            Statement::Execute(atomic),
        ])];
        assert!(effects_depend_on_uav(&module));
    }
}
