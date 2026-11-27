//! Shared IR writer entries called by expansion compute shaders.

mod hull;
pub(crate) use hull::hull;

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
    writer.clamped_sampler_declarations();
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

/// Storage slots above the shader-resource and expansion bindings.
pub const VERTEX_STREAM_BASE: u32 = 192;
pub const DRAW_ARGUMENTS_SLOT: u32 = 224;
pub const DRAW_STATE_SLOT: u32 = 33;

pub(crate) fn draw_state(source: &mut String, fetch_count: usize) {
    let vectors = fetch_count.div_ceil(2).max(1);
    writeln!(
        source,
        "struct DxDrawState {{
            first: u32, base_vertex: u32, first_instance: u32, primitives: u32,
            index_offset: u32, count: u32, argument_word: u32, instances: u32,
            fetch: array<vec4<u32>, {vectors}>,
        }}
        @group(0) @binding({DRAW_STATE_SLOT}) var<uniform> dx_draw_state: DxDrawState;"
    )
    .unwrap();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawStart {
    Direct,
    Indirect { indexed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InputStep {
    Vertex,
    Instance(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct VertexFetch {
    pub layout: FetchLayout,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FetchLayout {
    pub register: u32,
    pub mask: u8,
    pub stream: u32,
    pub step: InputStep,
    pub format: VertexFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexFormat {
    pub bytes: u32,
    pub bits: [u8; 4],
    pub kinds: [VertexComponent; 4],
    pub offsets: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexComponent {
    Float,
    Sint,
    Snorm,
    Uint,
    Unorm,
}

pub fn vertex(
    module: &Module,
    variant: &Variant,
    fragment: Option<&Reflection>,
    options: Options<'_>,
    fetches: &[FetchLayout],
    start: DrawStart,
) -> Result<Translation, Error> {
    let pulled = fetched(module, variant, options, fetches)?;
    let mut module = module.clone();
    let reflection = super::super::analysis::analyze(&module);
    let interface = passes::variant::apply(&mut module, &reflection, variant, fragment)?;
    let position = reflection
        .signatures
        .output
        .iter()
        .find(|element| element.system_value == SystemValue::Position)
        .ok_or(Error::Invalid("vertex position"))?
        .register;
    let mut writer = Writer::new(&module, &reflection, options);
    if interface.native_clip {
        writer.line("enable clip_distances;");
    }
    writer.source.push_str(&pulled.source);
    let (vertex_start, instance_start) = match start {
        DrawStart::Direct => ("dx_draw_state.base_vertex", "dx_draw_state.first_instance"),
        DrawStart::Indirect { indexed } => {
            writer.line(&format!(
                "@group(3) @binding({DRAW_ARGUMENTS_SLOT}) var<storage, read> dx_draw: array<u32>;"
            ));
            let vertex_offset = if indexed { 3 } else { 2 };
            writer.line(&format!("const DX_VERTEX_WORD = {vertex_offset}u;"));
            (
                "dx_draw[dx_draw_state.argument_word + DX_VERTEX_WORD]",
                "dx_draw[dx_draw_state.argument_word + DX_VERTEX_WORD + 1u]",
            )
        }
    };
    writer.open("struct DxPulledVertex");
    writer.line("@builtin(position) position: vec4<f32>,");
    writer.varying_fields(&interface);
    writer.clip_field(&interface);
    writer.close();
    writer.open("@vertex fn vertex_main(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance_id: u32) -> DxPulledVertex");
    writer.line(&format!("let instance_start = {instance_start};"));
    writer.line(&format!(
        "let o = dx_fetch(vertex_id, instance_id, {vertex_start}, instance_start);"
    ));
    let mut outputs = vec![format!("bitcast<vec4<f32>>(o[{position}])")];
    outputs.extend(super::variant_interface::variant_outputs(&interface, "o"));
    writer.line(&format!("return DxPulledVertex({});", outputs.join(", ")));
    writer.close();
    Ok(Translation {
        source: writer.source,
        entry_point: "vertex_main",
        reflection,
        locations: pulled.locations,
        counters: pulled.counters,
        per_sample_uav_effects: false,
    })
}

pub(crate) fn fetched(
    module: &Module,
    variant: &Variant,
    options: Options<'_>,
    fetches: &[FetchLayout],
) -> Result<Pulled, Error> {
    let mut pulled = emit(module, &variant.pull_input(), Invocation::Vertex, options)?;
    let reflection = super::super::analysis::analyze(module);
    let mut writer = Writer::new(module, &reflection, options);
    writer.source = std::mem::take(&mut pulled.source);
    draw_state(&mut writer.source, fetches.len());
    let streams: std::collections::BTreeSet<_> = fetches.iter().map(|fetch| fetch.stream).collect();
    for stream in streams {
        let binding = VERTEX_STREAM_BASE + stream;
        writer.line(&format!(
            "@group(3) @binding({binding}) var<storage, read> dx_stream_{stream}: array<u32>;"
        ));
        writer.open(&format!("fn dx_byte_{stream}(at: u32) -> u32"));
        writer.line(&format!(
            "return (dx_stream_{stream}[at / 4u] >> ((at % 4u) * 8u)) & 255u;"
        ));
        writer.close();
    }
    writer.open("fn dx_fetch(vertex_id: u32, instance_id: u32, vertex_start: u32, instance_start: u32) -> DxOutput");
    writer.line(&format!("var v: array<vec4<u32>, {}>;", pulled.inputs));
    for input in &reflection.signatures.input {
        match input.system_value {
            SystemValue::VertexId => writer.line(&format!(
                "v[{}].x = vertex_id - vertex_start;",
                input.register
            )),
            SystemValue::InstanceId => writer.line(&format!(
                "v[{}].x = instance_id - instance_start;",
                input.register
            )),
            _ => (),
        }
    }
    for (index, fetch) in fetches.iter().enumerate() {
        emit_fetch(&mut writer, index, *fetch)?;
    }
    writer.line("return dx_run(v);");
    writer.close();
    pulled.source = writer.source;
    Ok(pulled)
}

fn emit_fetch(writer: &mut Writer<'_>, index: usize, fetch: FetchLayout) -> Result<(), Error> {
    use VertexComponent::*;
    let FetchLayout {
        register,
        mask,
        stream,
        step,
        format,
    } = fetch;
    let index_expr = match step {
        InputStep::Vertex => "vertex_id".into(),
        InputStep::Instance(0) => "instance_start".into(),
        InputStep::Instance(rate) => {
            format!("instance_start + (instance_id - instance_start) / {rate}u")
        }
    };
    let address = format!("dx_address_{index}");
    let pair = if index.is_multiple_of(2) { "xy" } else { "zw" };
    writer.line(&format!(
        "let dx_fetch_{index} = dx_draw_state.fetch[{}].{pair};
         let {address} = dx_fetch_{index}.x + ({index_expr}) * dx_fetch_{index}.y;",
        index / 2
    ));
    let bytes = format.bytes;
    let bound = format!("dx_size_{index}");
    writer.line(&format!(
        "let {bound} = arrayLength(&dx_stream_{stream}) * 4u;"
    ));
    writer.open(&format!(
        "if {address} <= {bound} && {bytes}u <= {bound} - {address}"
    ));
    let mut component = 0;
    for lane in 0..4 {
        if mask & (1 << lane) == 0 {
            continue;
        }
        let bits = u32::from(format.bits[component]);
        let kind = format.kinds[0];
        let value = if bits == 0 {
            if component == 3 {
                match kind {
                    Sint | Uint => "1u",
                    _ => "0x3f800000u",
                }
                .into()
            } else {
                "0u".into()
            }
        } else {
            let bit = u32::from(format.offsets[component]);
            let first = bit / 8;
            let shift = bit % 8;
            let raw = (0..(bits + shift).div_ceil(8))
                .map(|byte| {
                    format!(
                        "(dx_byte_{stream}({address} + {}u) << {}u)",
                        first + byte,
                        byte * 8
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            let maximum = 2u64.pow(bits) - 1;
            let raw = format!("((({raw}) >> {shift}u) & {maximum}u)");
            let sign = 32 - bits;
            let signed = format!("(bitcast<i32>({raw} << {sign}u) >> {sign}u)");
            match format.kinds[component] {
                Uint | Sint if bits == 32 => raw,
                Uint => raw,
                Sint => format!("bitcast<u32>({signed})"),
                Float if bits == 32 => raw,
                Float if bits == 16 => format!("bitcast<u32>(unpack2x16float({raw}).x)"),
                Float if bits == 10 || bits == 11 => {
                    let mantissa = bits - 5;
                    format!(
                        "bitcast<u32>(unpack2x16float({raw} << {}u).x)",
                        10 - mantissa
                    )
                }
                Unorm => format!("bitcast<u32>(f32({raw}) / {maximum}.0)"),
                Snorm => format!("bitcast<u32>(max(-1.0, f32({signed}) / {}.0))", maximum / 2),
                Float => {
                    return Err(Error::Invalid("vertex fetch format"));
                }
            }
        };
        writer.line(&format!("v[{register}][{lane}] = {value};"));
        component += 1;
    }
    writer.close();
    Ok(())
}
