//! Direct3D 9 program libraries from `Model::Legacy` modules.
//!
//! The D3D9 renderer composes a vertex and a pixel library with its own stage
//! wrapper. Each library defines `<prefix>_run(input: ShaderInputs) -> ShaderOutputs`
//! and reads `<prefix>_constants` and `<prefix>_sample_<slot>`, which the wrapper
//! supplies. Every global the library defines carries its stage prefix, so both
//! libraries fit in one WGSL module.

use super::{Writer, alu, *};

pub(crate) struct Library<'a> {
    pub prefix: &'static str,
    /// Multiply through a helper that keeps 0 * infinity and NaN operands NaN
    /// when the backend does not preserve IEEE special values.
    pub guard_products: bool,
    /// Each IR input register the program reads, with its `vec4<f32>` source.
    pub inputs: &'a [(u32, String)],
    /// Input registers, which bounds the `v` array.
    pub input_count: u32,
    pub output_count: u32,
    /// Statements setting `result: ShaderOutputs` from the output registers `o`.
    pub result: &'a str,
    /// The `vec4` read of row `i` of each constant buffer slot.
    pub constants: &'a [String],
}

const GUARDED_PRODUCT: &str = r#"
fn dx_mul(a: u32, b: u32) -> u32 {
    let x = a & 0x7fffffffu;
    let y = b & 0x7fffffffu;
    if x > 0x7f800000u || y > 0x7f800000u || (x == 0x7f800000u && y == 0u) || (y == 0x7f800000u && x == 0u) {
        return 0x7fc00000u;
    }
    return bitcast<u32>(bitcast<f32>(a) * bitcast<f32>(b));
}
"#;

pub(crate) fn emit(module: &Module, library: &Library<'_>) -> Result<String, Error> {
    if module.model != Model::Legacy || !matches!(module.stage, Stage::Vertex | Stage::Pixel) {
        return Err(Error::Invalid("legacy library stage"));
    }
    let (module, reflection) = passes::prepare(module)?;
    let mut writer = Writer::new(&module, &reflection, Options::default());
    writer.legacy = Some(library);
    writer.source.push_str(alu::HELPERS);
    if library.guard_products {
        writer.source.push_str(GUARDED_PRODUCT);
    }
    for (slot, row) in library.constants.iter().enumerate() {
        writer.line(&format!(
            "fn dx_cb{slot}(i: u32) -> vec4<u32> {{ return bitcast<vec4<u32>>({row}); }}"
        ));
    }
    let outputs = library.output_count;
    writer.open(&format!(
        "fn dx_result(o: array<vec4<u32>, {outputs}>) -> ShaderOutputs"
    ));
    writer.line("var result: ShaderOutputs;");
    writer.source.push_str(library.result);
    writer.line("return result;");
    writer.close();
    writer.open("fn dx_run(input: ShaderInputs) -> ShaderOutputs");
    if !library.inputs.is_empty() {
        writer.line(&format!(
            "var v: array<vec4<u32>, {}>;",
            library.input_count
        ));
        for (register, value) in library.inputs {
            writer.line(&format!("v[{register}] = bitcast<vec4<u32>>({value});"));
        }
    }
    writer.line(&format!("var o: array<vec4<u32>, {outputs}>;"));
    writer.temporaries(&module, &reflection)?;
    writer.result = "dx_result(o)".into();
    if !writer.block(&module.body)? {
        writer.line("return dx_result(o);");
    }
    writer.close();
    let prefix = library.prefix;
    Ok(writer
        .source
        .replace("dx_", &format!("{prefix}_dx_"))
        .replace(&format!("{prefix}_dx_run("), &format!("{prefix}_run(")))
}

/// A product lane through the guarded helper.
pub(super) fn product(opcode: Opcode, args: &[String], lane: usize) -> String {
    let lane = LANES.as_bytes()[lane] as char;
    let product = format!("dx_mul({}.{lane}, {}.{lane})", args[0], args[1]);
    if opcode == Opcode::Mad {
        format!(
            "bitcast<u32>(bitcast<f32>({product}) + bitcast<f32>({}.{lane}))",
            args[2]
        )
    } else {
        product
    }
}

impl Writer<'_> {
    /// Sample through the wrapper. A plain sample carries the D3D9 mode in its
    /// controls: 0 plain, 1 projected, 2 biased and 3 explicit level from `w`.
    pub(super) fn legacy_sample(
        &mut self,
        library: &Library<'_>,
        op: &Instruction,
        args: &[String],
    ) -> Result<String, Error> {
        let slot = op
            .sources
            .get(1)
            .and_then(Operand::register)
            .and_then(|(_, indices)| indices.first().and_then(Index::direct))
            .ok_or(Error::Invalid("legacy texture slot"))?;
        let prefix = library.prefix;
        let name = format!("t{}", self.serial);
        let value = match op.opcode {
            Opcode::Sample => format!(
                "{prefix}_sample_{slot}(bitcast<vec4<f32>>({}), {}u)",
                args[0], op.controls
            ),
            Opcode::SampleD => format!(
                "{prefix}_sample_grad_{slot}(bitcast<vec4<f32>>({}), bitcast<vec4<f32>>({}), bitcast<vec4<f32>>({}))",
                args[0], args[1], args[2]
            ),
            other => return Err(Error::UnsupportedOpcode(other)),
        };
        self.line(&format!("let {name} = bitcast<vec4<u32>>({value});"));
        Ok(name)
    }
}
