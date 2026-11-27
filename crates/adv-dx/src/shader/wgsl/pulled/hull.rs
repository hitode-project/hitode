use super::*;
use crate::shader::ir::passes::variant::lanes;

pub(crate) fn hull(
    module: &Module,
    previous: &Reflection,
    input_points: u32,
    output_points: u32,
    options: Options<'_>,
) -> Result<Pulled, Error> {
    let (module, reflection) = passes::prepare(module)?;
    let inputs = register_count(&module.signatures.input);
    let outputs = register_count(&module.signatures.output);
    let patch = register_count(&module.signatures.patch_constant);
    let previous_outputs = register_count(&previous.signatures.output);
    let mut writer = Writer::new(&module, &reflection, options);
    writer.helpers(&reflection);
    writer.declarations(&module, &reflection)?;
    writer.line(&format!(
        "alias DxVsOutput = array<vec4<u32>, {previous_outputs}>;"
    ));
    writer.line(&format!(
        "alias DxHullInput = array<array<vec4<u32>, {inputs}>, {input_points}>;"
    ));
    writer.line(&format!(
        "alias DxControlPoint = array<vec4<u32>, {outputs}>;"
    ));
    writer.line(&format!(
        "alias DxControl = array<DxControlPoint, {output_points}>;"
    ));
    writer.line(&format!("alias DxPatch = array<vec4<u32>, {patch}>;"));
    writer.open("struct DxHullOutput");
    writer.line("control: DxControl,");
    writer.line("values: DxPatch,");
    writer.close();
    writer.result = "o".into();

    let mut phases = Vec::new();
    for (index, statement) in module.body.iter().enumerate() {
        if let Statement::Execute(op) = statement
            && matches!(
                op.opcode,
                Opcode::HsControlPointPhase | Opcode::HsForkPhase | Opcode::HsJoinPhase
            )
        {
            phases.push((index, op.opcode));
        }
    }
    let mut control = None;
    let mut constants = Vec::new();
    for (phase, &(start, opcode)) in phases.iter().enumerate() {
        let end = phases
            .get(phase + 1)
            .map_or(module.body.len(), |&(start, _)| start);
        let body = &module.body[start + 1..end];
        let phase_module = Module {
            body: body.to_vec(),
            ..module.clone()
        };
        let name = format!("dx_phase_{phase}");
        let output = if opcode == Opcode::HsControlPointPhase {
            control = Some(name.clone());
            writer.open(&format!("fn {name}(v: DxHullInput, v_cp: DxHullInput, v_control_id: vec4<u32>, v_primitive: vec4<u32>) -> DxControlPoint"));
            "var o: DxControlPoint;"
        } else {
            let count = body
                .iter()
                .find_map(|statement| match statement {
                    Statement::Declare(decl)
                        if matches!(
                            decl.opcode,
                            Opcode::DclHsForkPhaseInstanceCount
                                | Opcode::DclHsJoinPhaseInstanceCount
                        ) =>
                    {
                        decl.data.first().copied()
                    }
                    _ => None,
                })
                .unwrap_or(1);
            constants.push((name.clone(), count));
            writer.open(&format!("fn {name}(v: DxHullInput, v_cp: DxHullInput, o_cp: DxControl, v_phase_id: vec4<u32>, v_primitive: vec4<u32>, v_patch: DxPatch) -> DxPatch"));
            "var o = v_patch;"
        };
        writer.line(output);
        writer.temporaries(&phase_module, &reflection)?;
        if !writer.block(body)? {
            writer.line("return o;");
        }
        writer.close();
    }
    writer.open(&format!(
        "fn dx_hull(vs: array<DxVsOutput, {input_points}>, primitive_id: u32) -> DxHullOutput"
    ));
    writer.line("var v: DxHullInput;");
    writer.open(&format!(
        "for (var point = 0u; point < {input_points}u; point++)"
    ));
    for input in &module.signatures.input {
        if let Some(output) = previous.signatures.output.iter().find(|output| {
            output.semantic.eq_ignore_ascii_case(&input.semantic)
                && output.semantic_index == input.semantic_index
        }) {
            for (from, to) in lanes(output).zip(lanes(input)) {
                writer.line(&format!(
                    "v[point][{}][{}] = vs[point][{}][{}];",
                    to.register, to.component, from.register, from.component,
                ));
            }
        }
    }
    writer.close();
    writer.line("var out: DxHullOutput;");
    writer.open(&format!(
        "for (var point = 0u; point < {output_points}u; point++)"
    ));
    if let Some(control) = control {
        writer.line(&format!(
            "out.control[point] = {control}(v, v, vec4<u32>(point), vec4<u32>(primitive_id));"
        ));
    } else {
        for output in &module.signatures.output {
            if let Some(input) = module.signatures.input.iter().find(|input| {
                input.semantic.eq_ignore_ascii_case(&output.semantic)
                    && input.semantic_index == output.semantic_index
            }) {
                for (from, to) in lanes(input).zip(lanes(output)) {
                    writer.line(&format!(
                        "out.control[point][{}][{}] = v[point][{}][{}];",
                        to.register, to.component, from.register, from.component,
                    ));
                }
            }
        }
    }
    writer.close();
    for (function, count) in constants {
        writer.open(&format!(
            "for (var instance = 0u; instance < {count}u; instance++)"
        ));
        writer.line(&format!("out.values = {function}(v, v, out.control, vec4<u32>(instance), vec4<u32>(primitive_id), out.values);"));
        writer.close();
    }
    writer.line("return out;");
    writer.close();
    writer.clamped_sampler_declarations();
    Ok(Pulled {
        source: writer.source,
        locations: writer.locations,
        counters: writer.counters,
        inputs,
        outputs,
    })
}
