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
            let inputs = fields(module, &module.signatures.input, true, variant)?;
            let outputs = fields(module, &module.signatures.output, false, variant)?;
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
            let mut result: Vec<_> = outputs.iter().map(Field::output).collect();
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
            for field in inputs {
                self.line(&field.input());
            }
            if let Some(variant) = variant_inputs {
                self.variant_inputs(variant);
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

    fn input(&self) -> String {
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
            if self.register.starts_with("v_") {
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
        if self.ty == "u32" || self.ty == "vec4<u32>" {
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
    let mode = module.body.iter().find_map(|stmt| {
        if let Statement::Declare(decl) = stmt
            && let Some(mode) = decl.interpolation
            && let Some((Register::Input, [index])) =
                decl.operand.as_ref().and_then(Operand::register)
            && index.direct() == Some(register)
        {
            return Some(mode);
        }
        None
    });
    match mode {
        Some(Interpolation::Flat) => " @interpolate(flat)",
        Some(Interpolation::PerspectiveCentroid) => " @interpolate(perspective, centroid)",
        Some(Interpolation::Linear) => " @interpolate(linear)",
        Some(Interpolation::LinearCentroid) => " @interpolate(linear, centroid)",
        Some(Interpolation::PerspectiveSample) => " @interpolate(perspective, sample)",
        Some(Interpolation::LinearSample) => " @interpolate(linear, sample)",
        Some(Interpolation::Undefined | Interpolation::Perspective) | None => "",
    }
}
