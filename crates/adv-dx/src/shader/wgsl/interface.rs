use super::*;
use crate::shader::dxbc::fields::D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL;

#[derive(Clone)]
struct Field {
    name: String,
    attribute: String,
    ty: &'static str,
    register: &'static str,
    component: Option<char>,
}

impl Writer<'_> {
    pub(super) fn entry(
        &mut self,
        entry: &str,
        module: &Module,
        reflection: &Reflection,
        variant: Option<&passes::variant::Interface>,
    ) -> Result<(), Error> {
        let input_count = register_count(&module.signatures.input);
        let output_count = register_count(&module.signatures.output);
        if self.stage == Stage::Compute {
            let [x, y, z] = reflection
                .workgroup_size
                .ok_or(Error::Invalid("compute workgroup"))?;
            self.open(&format!(
                "@compute @workgroup_size({x}, {y}, {z}) fn {entry}(
    @builtin(global_invocation_id) dispatch_id: vec3<u32>,
    @builtin(workgroup_id) group_id: vec3<u32>,
    @builtin(local_invocation_id) local_id: vec3<u32>,
    @builtin(local_invocation_index) local_index: u32
)"
            ));
            self.line("let dx_dispatch_id = vec4<u32>(dispatch_id, 0u);");
            self.line("let dx_group_id = vec4<u32>(group_id, 0u);");
            self.line("let dx_local_id = vec4<u32>(local_id, 0u);");
            self.line("let dx_local_index = vec4<u32>(local_index);");
        } else {
            let mut inputs = fields(module, &module.signatures.input, true, variant)?;
            if self.stage == Stage::Pixel && self.options.raster_samples == 1 {
                // A single-sample target supplies index zero. The input register
                // already starts at zero, so this variant needs no sample-rate builtin.
                inputs.retain(|field| field.attribute != "@builtin(sample_index)");
            }
            if reflection
                .reads
                .contains_key(&(Register::InputCoverageMask, Some(0)))
                && !inputs.iter().any(|field| field.register == "v_coverage")
            {
                inputs.push(Field {
                    name: "coverage".into(),
                    attribute: "@builtin(sample_mask)".into(),
                    ty: "u32",
                    register: "v_coverage",
                    component: Some('x'),
                });
            }
            let mut eval_position = String::new();
            let mut eval_coverage = String::new();
            let mut eval_sample = String::new();
            if reflection.interpolation_queries {
                eval_position =
                    builtin_input(&mut inputs, "position", "dx_eval_position", "vec4<f32>");
                if self.options.raster_samples > 1 {
                    eval_coverage =
                        builtin_input(&mut inputs, "sample_mask", "dx_eval_coverage", "u32");
                    if reflection.interpolation.values().any(|mode| {
                        matches!(
                            mode,
                            Interpolation::PerspectiveSample | Interpolation::LinearSample
                        )
                    }) {
                        eval_sample =
                            builtin_input(&mut inputs, "sample_index", "dx_eval_sample", "u32");
                    }
                }
                for field in &mut inputs {
                    if field.ty == "vec4<f32>" && field.attribute.starts_with("@location(") {
                        field.attribute = field
                            .attribute
                            .replace(", centroid)", ")")
                            .replace(", sample)", ")");
                    }
                }
            }
            let mut outputs = fields(module, &module.signatures.output, false, variant)?;
            let sample_mask = self.options.raster_sample_mask;
            if sample_mask != u32::MAX && !outputs.iter().any(|f| f.register == "o_coverage") {
                outputs.push(Field {
                    name: "coverage".into(),
                    attribute: "@builtin(sample_mask)".into(),
                    ty: "u32",
                    register: "",
                    component: None,
                });
            }
            let vertex = self.stage == Stage::Vertex;
            let variant_inputs = variant.filter(|_| !vertex);
            let variant_outputs = variant.filter(|_| vertex);
            if let Some(storage) = variant_inputs.and_then(|i| i.storage.as_ref()) {
                self.storage_varying_binding(storage);
            }
            if variant_inputs.is_some_and(|i| i.count_fragments) {
                self.line(&format!("@group(3) @binding({}) var<storage, read_write> dx_statistics: array<atomic<u32>>;", super::STATISTICS_SLOT));
            }
            let input_name = if self.stage == Stage::Vertex {
                "DxVertexInput"
            } else {
                "DxPixelInput"
            };
            let output_name = if self.stage == Stage::Vertex {
                "DxVertexOutput"
            } else {
                "DxPixelOutput"
            };
            let has_inputs = self.interface(input_name, &inputs, variant_inputs, false);
            let has_outputs = self.interface(output_name, &outputs, variant_outputs, true);
            let args = if !has_inputs {
                String::new()
            } else {
                format!("input: {input_name}")
            };
            let attrib = if self.stage == Stage::Vertex {
                "@vertex"
            } else if reflection.global_flags & D3D11_SB_GLOBAL_FLAG_FORCE_EARLY_DEPTH_STENCIL != 0
            {
                "@fragment @early_depth_test(force)"
            } else if module
                .signatures
                .output
                .iter()
                .any(|e| e.system_value == SystemValue::DepthGreaterEqual)
            {
                "@fragment @early_depth_test(greater_equal)"
            } else if module
                .signatures
                .output
                .iter()
                .any(|e| e.system_value == SystemValue::DepthLessEqual)
            {
                "@fragment @early_depth_test(less_equal)"
            } else {
                "@fragment"
            };
            let returns = if !has_outputs {
                String::new()
            } else {
                format!(" -> {output_name}")
            };
            self.open(&format!("{attrib} fn {entry}({args}){returns}"));
            let mut result: Vec<_> = outputs
                .iter()
                .map(|field| {
                    let value = match field.register {
                        "" => format!("{sample_mask}u"),
                        "o_coverage" if sample_mask != u32::MAX => {
                            format!("o_coverage.x & {sample_mask}u")
                        }
                        _ => field.output(),
                    };
                    let alpha = self.stage == Stage::Pixel
                        && (0..8).any(|slot| {
                            self.options.alpha_targets & (1 << slot) != 0
                                && field.attribute == format!("@location({slot})")
                        });
                    if alpha {
                        format!("({value}).wyzw")
                    } else {
                        value
                    }
                })
                .collect();
            if let Some(variant) = variant_outputs {
                result.extend(super::variant_interface::variant_outputs(variant, "o"));
            }
            self.result = if !has_outputs {
                String::new()
            } else {
                format!("{output_name}({})", result.join(", "))
            };
            self.line(&format!("var v: array<vec4<u32>, {input_count}>;"));
            self.line(&format!("var o: array<vec4<u32>, {output_count}>;"));
            for field in &inputs {
                if !field.register.is_empty() {
                    self.line(&field.input(sample_mask));
                }
            }
            if let Some(variant) = variant_inputs {
                self.variant_inputs(variant);
            }
            if reflection.interpolation_queries {
                if !eval_coverage.is_empty() && sample_mask != u32::MAX {
                    eval_coverage = format!("({eval_coverage} & {sample_mask}u)");
                }
                self.interpolation_planes(
                    module,
                    input_count,
                    &eval_position,
                    &eval_coverage,
                    &eval_sample,
                );
            }
            if outputs.iter().any(|output| output.register == "o_depth") {
                self.line("var o_depth: vec4<u32>;");
            }
            if outputs.iter().any(|output| output.register == "o_coverage") {
                self.line("var o_coverage: vec4<u32>;");
            }
        }
        self.temporaries(module, reflection)
    }

    pub(super) fn temporaries(
        &mut self,
        module: &Module,
        reflection: &Reflection,
    ) -> Result<(), Error> {
        self.line(&format!(
            "var r: array<vec4<u32>, {}>;",
            reflection.temporary_count.max(1)
        ));
        for statement in &module.body {
            if let Statement::Declare(declaration) = statement
                && declaration.opcode == Opcode::DclIndexableTemp
            {
                let [register, count, _] = declaration.data[..] else {
                    return Err(Error::Invalid("indexable temporary"));
                };
                self.line(&format!("var x{register}: array<vec4<u32>, {count}>;"));
            }
        }
        Ok(())
    }

    fn interpolation_planes(
        &mut self,
        module: &Module,
        count: u32,
        position: &str,
        coverage: &str,
        sample: &str,
    ) {
        // Attribute/w and reciprocal w are linear in screen space. Recover
        // both planes before branching so eval can use varying offsets.
        for plane in ["base", "dx", "dy"] {
            self.line(&format!("var dx_eval_{plane}: array<vec4<f32>, {count}>;"));
        }
        self.line(&format!("var dx_eval_w: array<vec3<f32>, {count}>;"));
        let samples = self.options.raster_samples;
        if samples == 1 {
            self.line("let dx_centroid_offset = vec2<f32>(0.0);");
        } else {
            let mask = ((1u64 << samples) - 1) as u32;
            self.line(&format!("let dx_centroid_offset = select(dx_sample_position({samples}u, firstTrailingBit({coverage})), vec2<f32>(0.0), {coverage} == {mask}u);"));
        }
        let mut seen = HashSet::new();
        for element in &module.signatures.input {
            if element.system_value != SystemValue::None || !seen.insert(element.register) {
                continue;
            }
            let index = element.register.to_string();
            let mode = interpolation_mode(module, element.register);
            let float = element.value_type == ValueType::Float;
            let perspective = float
                && !matches!(
                    mode,
                    Some(
                        Interpolation::Flat
                            | Interpolation::Linear
                            | Interpolation::LinearCentroid
                            | Interpolation::LinearSample
                    )
                );
            let value = format!("bitcast<vec4<f32>>(v[{index}])");
            let divisor = if perspective {
                let w = format!("{position}.w");
                format!("vec3<f32>({w}, dpdxFine({w}), dpdyFine({w}))")
            } else {
                "vec3<f32>(1.0, 0.0, 0.0)".into()
            };
            self.line(&format!("dx_eval_w[{index}] = {divisor};"));
            self.line(&format!(
                "let dx_plane_{index} = {value} * dx_eval_w[{index}].x;"
            ));
            self.line(&format!("dx_eval_base[{index}] = dx_plane_{index};"));
            if float && !matches!(mode, Some(Interpolation::Flat)) {
                self.line(&format!(
                    "dx_eval_dx[{index}] = dpdxFine(dx_plane_{index});"
                ));
                self.line(&format!(
                    "dx_eval_dy[{index}] = dpdyFine(dx_plane_{index});"
                ));
            }
            let offset = match mode {
                Some(Interpolation::PerspectiveCentroid | Interpolation::LinearCentroid) => {
                    Some("dx_centroid_offset".into())
                }
                Some(Interpolation::PerspectiveSample | Interpolation::LinearSample)
                    if samples > 1 =>
                {
                    Some(format!("dx_sample_position({samples}u, {sample})"))
                }
                _ => None,
            };
            if let Some(offset) = offset {
                let expression = Self::interpolate_at(&index, &offset);
                self.line(&format!("v[{index}] = bitcast<vec4<u32>>({expression});"));
            }
        }
    }

    pub(super) fn evaluate_input(
        &self,
        op: &Instruction,
        args: &[String],
    ) -> Result<String, Error> {
        let Some((Register::Input, [index])) = op.sources[0].register() else {
            return Err(Error::UnsupportedOpcode(op.opcode));
        };
        let index = self.index(index, 1)?;
        let offset = match op.opcode {
            Opcode::EvalCentroid => "dx_centroid_offset".into(),
            Opcode::EvalSampleIndex => format!(
                "dx_sample_position({}u, {}.x)",
                self.options.raster_samples, args[1]
            ),
            Opcode::EvalSnapped => format!(
                "(vec2<f32>(bitcast<vec2<i32>>({}.xy << vec2<u32>(28u)) >> vec2<u32>(28u)) * 0.0625)",
                args[1]
            ),
            other => return Err(Error::UnsupportedOpcode(other)),
        };
        let lanes: String = op.sources[0]
            .components
            .swizzle()
            .into_iter()
            .map(|lane| LANES.as_bytes()[lane as usize] as char)
            .collect();
        Ok(format!(
            "bitcast<vec4<u32>>(({}).{lanes})",
            Self::interpolate_at(&index, &offset)
        ))
    }

    fn interpolate_at(index: &str, offset: &str) -> String {
        format!(
            "dx_evaluate(dx_eval_base[{index}], dx_eval_dx[{index}], dx_eval_dy[{index}], dx_eval_w[{index}], {offset})"
        )
    }

    fn interface(
        &mut self,
        name: &str,
        fields: &[Field],
        variant: Option<&passes::variant::Interface>,
        output: bool,
    ) -> bool {
        if fields.is_empty()
            && !variant.is_some_and(|i| {
                !i.varyings.is_empty() || i.storage.is_some() || output && i.native_clip
            })
        {
            return false;
        }
        self.open(&format!("struct {name}"));
        for field in fields {
            self.line(&format!(
                "{} {}: {},",
                field.attribute, field.name, field.ty
            ));
        }
        if let Some(variant) = variant {
            self.varying_fields(variant);
            if output {
                self.clip_field(variant);
            }
        }
        self.close();
        true
    }
}

impl Field {
    fn location(&self) -> String {
        if matches!(self.register, "v" | "o") {
            format!("{}[{}]", self.register, &self.name[1..])
        } else {
            self.register.into()
        }
    }

    fn input(&self, sample_mask: u32) -> String {
        let field = format!("input.{}", self.name);
        let value = if self.ty == "bool" {
            format!("select(0u, 0xffffffffu, {field})")
        } else if self.ty == "u32" || self.ty == "vec4<u32>" {
            field
        } else {
            format!(
                "bitcast<{}>({field})",
                if self.component.is_some() {
                    "u32"
                } else {
                    "vec4<u32>"
                }
            )
        };
        let register = self.location();
        if let Some(component) = self.component {
            if self.register == "v_coverage" && sample_mask != u32::MAX {
                format!("let {register} = vec4<u32>({value} & {sample_mask}u);")
            } else if self.register.starts_with("v_") {
                format!("let {register} = vec4<u32>({value});")
            } else {
                format!("{register}.{component} = {value};")
            }
        } else {
            format!("{register} = {value};")
        }
    }

    fn output(&self) -> String {
        let mut register = self.location();
        if let Some(component) = self.component {
            register.push('.');
            register.push(component);
        }
        if self.attribute == "@builtin(frag_depth)" {
            format!("bitcast<f32>(dx_sat({register}))")
        } else if self.ty == "u32" || self.ty == "vec4<u32>" {
            register
        } else {
            format!("bitcast<{}>({register})", self.ty)
        }
    }
}

pub(super) fn register_count(elements: &[SignatureElement]) -> u32 {
    elements
        .iter()
        .filter_map(|element| element.register.checked_add(1))
        .max()
        .unwrap_or(1)
}

fn fields(
    module: &Module,
    elements: &[SignatureElement],
    input: bool,
    variant: Option<&passes::variant::Interface>,
) -> Result<Vec<Field>, Error> {
    let register = if input { "v" } else { "o" };
    let mut fields: Vec<Field> = Vec::new();
    for element in elements {
        if variant.is_some()
            && input == (module.stage == Stage::Pixel)
            && matches!(
                element.system_value,
                SystemValue::None | SystemValue::ClipDistance
            )
        {
            continue;
        }
        let first = LANES
            .as_bytes()
            .get(element.mask.trailing_zeros() as usize)
            .map(|&lane| lane as char);
        let name = format!("{register}{}", element.register);
        let location = |slot, interpolate: bool| -> Result<Field, Error> {
            let ty = match element.value_type {
                ValueType::Bits | ValueType::Uint => "vec4<u32>",
                ValueType::Int => "vec4<i32>",
                ValueType::Float => "vec4<f32>",
                ValueType::Double | ValueType::Binding | ValueType::Opaque => {
                    return Err(Error::Invalid("stage interface value type"));
                }
            };
            let interp = if interpolate {
                interpolation(module, element.register, ty)
            } else {
                ""
            };
            Ok(Field {
                name: name.clone(),
                attribute: format!("@location({slot}){interp}"),
                ty,
                register,
                component: None,
            })
        };
        let builtin = |builtin, ty, component, register, name| Field {
            name,
            attribute: format!("@builtin({builtin})"),
            ty,
            component,
            register,
        };
        let field = match element.system_value {
            SystemValue::None => {
                location(element.register, input == (module.stage == Stage::Pixel))?
            }
            SystemValue::Target => location(element.semantic_index, false)?,
            SystemValue::Position if module.stage == Stage::Vertex && input => {
                location(element.register, false)?
            }
            SystemValue::Position => builtin("position", "vec4<f32>", None, register, name),
            SystemValue::VertexId => builtin("vertex_index", "u32", first, register, name),
            SystemValue::InstanceId => builtin("instance_index", "u32", first, register, name),
            SystemValue::FrontFace => builtin("front_facing", "bool", first, register, name),
            SystemValue::SampleIndex => builtin("sample_index", "u32", first, register, name),
            SystemValue::PrimitiveId if element.register != u32::MAX => {
                builtin("primitive_index", "u32", first, register, name)
            }
            SystemValue::PrimitiveId => builtin(
                "primitive_index",
                "u32",
                Some('x'),
                "v_primitive",
                "primitive".into(),
            ),
            SystemValue::Coverage if input => builtin(
                "sample_mask",
                "u32",
                Some('x'),
                "v_coverage",
                "coverage".into(),
            ),
            SystemValue::Coverage => builtin(
                "sample_mask",
                "u32",
                Some('x'),
                "o_coverage",
                "coverage".into(),
            ),
            SystemValue::Depth | SystemValue::DepthGreaterEqual | SystemValue::DepthLessEqual => {
                builtin("frag_depth", "f32", Some('x'), "o_depth", "depth".into())
            }
            SystemValue::ClipDistance
            | SystemValue::CullDistance
            | SystemValue::RenderTargetArrayIndex
            | SystemValue::ViewportArrayIndex
            | SystemValue::TessFactor
            | SystemValue::InsideTessFactor
            | SystemValue::StencilRef
            | SystemValue::InnerCoverage => {
                return Err(Error::Invalid("unsupported WGSL interface system value"));
            }
        };
        if let Some(previous) = fields.iter().find(|previous| previous.name == field.name) {
            if previous.attribute != field.attribute
                || previous.ty != field.ty
                || previous.component != field.component
            {
                return Err(Error::Invalid("incompatible packed signature"));
            }
        } else {
            fields.push(field);
        }
    }
    Ok(fields)
}

fn interpolation(module: &Module, register: u32, ty: &str) -> &'static str {
    if ty != "vec4<f32>" {
        return " @interpolate(flat)";
    }
    match interpolation_mode(module, register) {
        Some(Interpolation::Flat) => " @interpolate(flat)",
        Some(Interpolation::PerspectiveCentroid) => " @interpolate(perspective, centroid)",
        Some(Interpolation::Linear) => " @interpolate(linear)",
        Some(Interpolation::LinearCentroid) => " @interpolate(linear, centroid)",
        Some(Interpolation::PerspectiveSample) => " @interpolate(perspective, sample)",
        Some(Interpolation::LinearSample) => " @interpolate(linear, sample)",
        Some(Interpolation::Undefined | Interpolation::Perspective) | None => "",
    }
}

fn interpolation_mode(module: &Module, register: u32) -> Option<Interpolation> {
    module.body.iter().find_map(|stmt| {
        if let Statement::Declare(decl) = stmt
            && let Some(mode) = decl.interpolation
            && let Some((Register::Input, [index])) =
                decl.operand.as_ref().and_then(Operand::register)
            && index.direct() == Some(register)
        {
            return Some(mode);
        }
        None
    })
}

fn builtin_input(fields: &mut Vec<Field>, builtin: &str, name: &str, ty: &'static str) -> String {
    let attribute = format!("@builtin({builtin})");
    if let Some(field) = fields.iter().find(|field| field.attribute == attribute) {
        return format!("input.{}", field.name);
    }
    fields.push(Field {
        name: name.into(),
        attribute,
        ty,
        register: "",
        component: None,
    });
    format!("input.{name}")
}

pub(super) const SAMPLE_POSITIONS: &str = "
fn dx_sample_position(count: u32, index: u32) -> vec2<f32> {
    if index >= count { return vec2<f32>(0.0); }
    var p = vec2<i32>(0);
    switch count {
        case 2: { p = array(vec2(4, 4), vec2(-4, -4))[index]; }
        case 4: { p = array(vec2(-2, -6), vec2(6, -2), vec2(-6, 2), vec2(2, 6))[index]; }
        case 8: {
            p = array(vec2(1, -3), vec2(-1, 3), vec2(5, 1), vec2(-3, -5),
                vec2(-5, 5), vec2(-7, -1), vec2(3, 7), vec2(7, -7))[index];
        }
        case 16: {
            p = array(vec2(1, 1), vec2(-1, -3), vec2(-3, 2), vec2(4, -1),
                vec2(-5, -2), vec2(2, 5), vec2(5, 3), vec2(3, -5),
                vec2(-2, 6), vec2(0, -7), vec2(-4, -6), vec2(-6, 4),
                vec2(-8, 0), vec2(7, -4), vec2(6, 7), vec2(-7, -8))[index];
        }
        default: {}
    }
    return vec2<f32>(p) * 0.0625;
}

fn dx_evaluate(v: vec4<f32>, dx: vec4<f32>, dy: vec4<f32>, w: vec3<f32>, offset: vec2<f32>) -> vec4<f32> {
    return (v + dx * offset.x + dy * offset.y) / (w.x + dot(w.yz, offset));
}
";
