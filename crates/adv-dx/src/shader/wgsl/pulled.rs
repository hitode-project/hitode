//! Shared IR writer entries called by expansion compute shaders.

use super::interface::register_count;
use super::*;
use crate::shader::variant::Variant;

pub(crate) struct Pulled {
    pub source: String,
    pub locations: Vec<BindingLocation>,
    pub counters: Vec<(u32, u32)>,
    pub inputs: u32,
    pub outputs: u32,
}

#[derive(Clone, Copy)]
pub(crate) enum Invocation {
    Vertex,
    Domain {
        points: u32,
        control: u32,
        patch: u32,
    },
    Geometry {
        inputs: u32,
        max_vertices: u32,
    },
}

pub(crate) fn emit(
    module: &Module,
    variant: &Variant,
    invocation: Invocation,
    options: Options<'_>,
) -> Result<Pulled, Error> {
    let (mut module, reflection) = passes::prepare(module)?;
    let interface = passes::variant::apply(&mut module, &reflection, variant, None)?;
    if !interface.pulled {
        return Err(Error::Invalid("expansion source variant"));
    }
    if reflection.doubles {
        return Err(Error::DoublePrecision);
    }
    let inputs = register_count(&module.signatures.input);
    let outputs = register_count(&module.signatures.output);
    let mut writer = Writer::new(
        &module,
        &reflection,
        Options {
            storage_formats: variant.storage_formats(),
            ..options
        },
    );
    writer.helpers(&reflection);
    writer.declarations(&module, &reflection)?;
    writer.line(&format!("alias DxOutput = array<vec4<u32>, {outputs}>;"));
    match invocation {
        Invocation::Vertex => {
            writer.open(&format!(
                "fn dx_run(v: array<vec4<u32>, {inputs}>) -> DxOutput"
            ));
            writer.result = "o".into();
        }
        Invocation::Domain {
            points,
            control,
            patch,
        } => {
            writer.open(&format!("fn dx_run(v_domain: vec4<u32>, v_primitive: vec4<u32>, v_cp: array<array<vec4<u32>, {control}>, {points}>, v_patch: array<vec4<u32>, {}>) -> DxOutput", patch.max(1)));
            writer.result = "o".into();
        }
        Invocation::Geometry {
            inputs: count,
            max_vertices,
        } => {
            writer.line(&format!(
                "var<private> dx_emitted: array<DxOutput, {max_vertices}>;"
            ));
            writer.line(&format!(
                "var<private> dx_tags: array<u32, {max_vertices}>;"
            ));
            writer.line(&format!(
                "var<private> dx_end: array<bool, {max_vertices}>;"
            ));
            writer.line("var<private> dx_last: array<u32, 4>;");
            writer.line("var<private> dx_emitted_count: u32;");
            writer.open("fn dx_emit(stream: u32, value: DxOutput)");
            writer.open(&format!("if dx_emitted_count < {max_vertices}u"));
            writer.line("dx_emitted[dx_emitted_count] = value;");
            writer.line("dx_tags[dx_emitted_count] = stream;");
            writer.line("dx_end[dx_emitted_count] = false;");
            writer.line("dx_emitted_count += 1u;");
            writer.line("dx_last[stream] = dx_emitted_count;");
            writer.close();
            writer.close();
            writer.open("fn dx_cut(stream: u32)");
            writer.open("if dx_last[stream] != 0u");
            writer.line("dx_end[dx_last[stream] - 1u] = true;");
            writer.line("dx_last[stream] = 0u;");
            writer.close();
            writer.close();
            writer.open(&format!("fn dx_run(v: array<array<vec4<u32>, {inputs}>, {count}>, v_primitive: vec4<u32>, v_gs_instance: vec4<u32>)"));
            writer.line("dx_emitted_count = 0u;");
            writer.line("dx_last = array<u32, 4>();");
        }
    }
    writer.line("var o: DxOutput;");
    writer.temporaries(&module, &reflection)?;
    let terminates = writer.block(&module.body)?;
    if !terminates && !writer.result.is_empty() {
        writer.line("return o;");
    }
    writer.close();
    Ok(Pulled {
        source: writer.source,
        locations: writer.locations,
        counters: writer.counters,
        inputs,
        outputs,
    })
}

pub(crate) fn raster(
    module: &Module,
    variant: &Variant,
    fragment: Option<&Reflection>,
    registers: u32,
    vertices: u32,
    stream: u32,
) -> Result<String, Error> {
    use crate::shader::expand::RASTER_SLOT;
    let mut module = module.clone();
    module
        .signatures
        .output
        .retain(|element| element.stream == stream);
    let reflection = crate::shader::analysis::analyze(&module);
    let mut interface = passes::variant::apply(&mut module, &reflection, variant, fragment)?;
    if variant.storage_varyings()
        && let Some(fragment) = fragment
    {
        passes::variant::storage_varyings(
            &mut interface,
            &reflection,
            &reflection,
            fragment,
            vertices,
            registers,
        );
    }
    let position = reflection
        .signatures
        .output
        .iter()
        .find(|e| e.system_value == SystemValue::Position)
        .ok_or(Error::Invalid("expansion raster position"))?
        .register;
    let mut writer = Writer::new(&module, &reflection, Options::default());
    if interface.native_clip {
        writer.line("enable clip_distances;");
    }
    writer.line(&format!("@group(3) @binding({RASTER_SLOT}) var<storage, read> dx_raster: array<array<vec4<u32>, {}>>;", registers + 1));
    writer.open("struct DxExpandedVertex");
    writer.line("@builtin(position) position: vec4<f32>,");
    writer.varying_fields(&interface);
    writer.clip_field(&interface);
    writer.close();
    writer.open("@vertex fn vertex_main(@builtin(vertex_index) index: u32) -> DxExpandedVertex");
    writer.line("let o = dx_raster[index];");
    let bin = variant.bin();
    let test = if variant.binned() {
        format!(
            "o[{registers}].x == {}u && o[{registers}].y == {}u",
            bin.viewport, bin.first_layer
        )
    } else {
        "true".into()
    };
    let mut outputs = vec![format!(
        "select(vec4<f32>(2.0, 2.0, 2.0, 1.0), bitcast<vec4<f32>>(o[{position}]), {test})"
    )];
    if let Some(storage) = &interface.storage {
        outputs.push(format!(
            "index / {}u * {}u",
            storage.vertices, storage.vertices
        ));
        let basis = format!(
            "vec2<f32>(select(0.0, 1.0, index % {}u == 0u), select(0.0, 1.0, index % {}u == 1u))",
            storage.vertices, storage.vertices
        );
        outputs.extend(storage.modes.iter().map(|_| basis.clone()));
    }
    outputs.extend(super::variant_interface::variant_outputs(&interface, "o"));
    writer.line(&format!("return DxExpandedVertex({});", outputs.join(", ")));
    writer.close();
    Ok(writer.source)
}
