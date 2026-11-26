use super::{ParseError, fields::*, parse};
use crate::shader::ir::*;
use std::fmt::Write;

pub fn disassemble(bytes: &[u8]) -> Result<String, ParseError> {
    let module = parse(bytes)?;
    let model = match module.model {
        Model::Sm40 | Model::Level91 | Model::Level93 => "4_0",
        Model::Sm41 => "4_1",
        Model::Sm50 => "5_0",
    };
    let mut output = format!("{}_{model}\n", module.stage.prefix());
    block(&mut output, &module.body, 0);
    Ok(output)
}

fn block(output: &mut String, body: &Block, indent: usize) {
    let pad = "  ".repeat(indent);
    for statement in body {
        match statement {
            Statement::Declare(decl) => writeln!(output, "{pad}{}", declaration(decl)).unwrap(),
            Statement::Execute(op) => writeln!(output, "{pad}{}", operation(op)).unwrap(),
            Statement::If {
                test,
                accept,
                reject,
            } => {
                writeln!(output, "{pad}{}", operation(test)).unwrap();
                block(output, accept, indent + 1);
                if let Some(reject) = reject {
                    writeln!(output, "{pad}else ").unwrap();
                    block(output, reject, indent + 1);
                }
                writeln!(output, "{pad}endif ").unwrap();
            }
            Statement::Loop(body) => {
                writeln!(output, "{pad}loop ").unwrap();
                block(output, body, indent + 1);
                writeln!(output, "{pad}endloop ").unwrap();
            }
            Statement::Switch { selector, cases } => {
                writeln!(
                    output,
                    "{pad}switch {}",
                    operand(selector, ValueType::Uint, false)
                )
                .unwrap();
                for case in cases {
                    if let Some(value) = case.value {
                        writeln!(output, "{pad}  case l({value})").unwrap();
                    } else {
                        writeln!(output, "{pad}  default ").unwrap();
                    }
                    block(output, &case.body, indent + 1);
                }
                writeln!(output, "{pad}endswitch ").unwrap();
            }
            Statement::ImmediateConstants(rows) => {
                let rows: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        let words: Vec<_> = row
                            .iter()
                            .map(|&value| literal(value, ValueType::Bits))
                            .collect();
                        format!("{{ {} }}", words.join(", ")).replace(" }", "}")
                    })
                    .collect();
                writeln!(
                    output,
                    "{pad}dcl_immediateConstantBuffer {{ {} }}",
                    rows.join(",\n                              ")
                )
                .unwrap();
            }
            Statement::CustomData { class, words } => {
                writeln!(output, "{pad}customdata {class}, {words:?}").unwrap();
            }
        }
    }
}

fn operation(op: &Instruction) -> String {
    use Opcode::*;
    let mut name = op.opcode.mnemonic().to_owned();
    if matches!(op.opcode, If | Breakc | Callc | Continuec | Retc | Discard) {
        name.push_str(if op.test_nonzero { "_nz" } else { "_z" });
    }
    if op.opcode == Sync {
        for (flag, suffix) in [
            (
                D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GLOBAL,
                "_uglobal",
            ),
            (D3D11_SB_SYNC_UNORDERED_ACCESS_VIEW_MEMORY_GROUP, "_ugroup"),
            (D3D11_SB_SYNC_THREAD_GROUP_SHARED_MEMORY, "_g"),
            (D3D11_SB_SYNC_THREADS_IN_GROUP, "_t"),
        ] {
            if op.controls & flag != 0 {
                name.push_str(suffix);
            }
        }
        return name;
    }
    if op.saturate {
        name.push_str("_sat");
    }
    if op.resource.offset.is_some() {
        name.push_str("_aoffimmi");
    }
    if op.resource.dimension.is_some() || op.resource.returns.is_some() {
        name.push_str("_indexable");
    }
    if let Some([x, y, z]) = op.resource.offset {
        write!(name, "({x},{y},{z})").unwrap();
    }
    if let Some((dim, stride)) = op.resource.dimension {
        write!(name, "({}", dimension(dim)).unwrap();
        if stride != 0 {
            write!(name, ", stride={stride}").unwrap();
        }
        name.push(')');
    }
    if let Some(returns) = op.resource.returns {
        write!(name, "{}", returns_string(returns)).unwrap();
    }
    if op.opcode == Resinfo {
        match decode_resinfo_instruction_return_type(op.controls) {
            D3D10_SB_RESINFO_INSTRUCTION_RETURN_UINT => name.push_str("_uint"),
            D3D10_SB_RESINFO_INSTRUCTION_RETURN_RCPFLOAT => name.push_str("_rcpFloat"),
            _ => {}
        }
    }
    let (dest, src) = op.opcode.types();
    let operands: Vec<_> = op
        .destinations
        .iter()
        .zip(dest)
        .map(|(op, &ty)| operand(op, ty, false))
        .chain(
            op.sources
                .iter()
                .zip(src)
                .map(|(op, &ty)| operand(op, ty, false)),
        )
        .collect();
    let separator = if op.opcode == Mov { "," } else { ", " };
    // The typeless mov uses a compact literal, but its operand separator matches other instructions.
    let operands = if op.opcode == Mov
        && op
            .sources
            .first()
            .is_some_and(|source| matches!(source.value, OperandValue::Immediate(_)))
    {
        let literal = operands[1].replace(", ", separator);
        format!("{}, {literal}", operands[0])
    } else {
        operands.join(", ")
    };
    format!("{name} {operands}")
}

fn declaration(decl: &Declaration) -> String {
    use Opcode::*;
    let name = decl.opcode.mnemonic();
    let controls = decl.controls;
    let argument = decl
        .operand
        .as_ref()
        .map(|op| operand(op, ValueType::Bits, true))
        .unwrap_or_default();
    if decl.opcode == DclGlobalFlags {
        let names = [
            "refactoringAllowed",
            "enableDoublePrecisionFloatOps",
            "forceEarlyDepthStencil",
            "enableRawAndStructuredBuffers",
            "skipOptimization",
            "enableMinimumPrecision",
            "enableDoubleExtensions",
            "enableShaderExtensions",
            "allResourcesBound",
        ];
        let flags: Vec<_> = names
            .into_iter()
            .enumerate()
            .filter_map(|(i, flag)| (controls & (1 << (11 + i)) != 0).then_some(flag))
            .collect();
        return format!("dcl_globalFlags {}", flags.join(" | "));
    }
    if let Some(binding) = decl.binding {
        return match binding.kind {
            BindingKind::ConstantBuffer {
                bytes,
                dynamic_indexing,
            } => {
                let indexing = if dynamic_indexing {
                    "dynamicIndexed"
                } else {
                    "immediateIndexed"
                };
                format!(
                    "dcl_constantbuffer CB{}[{}], {indexing}",
                    binding.slot,
                    bytes / 16
                )
            }
            BindingKind::Sampler(mode) => format!(
                "dcl_sampler {argument}, {}",
                match mode {
                    SamplerMode::Default => "mode_default",
                    SamplerMode::Comparison => "mode_comparison",
                    SamplerMode::Mono => "mode_mono",
                }
            ),
            BindingKind::Typed {
                dimension: dim,
                samples,
                returns,
            } => {
                let prefix = if binding.register == Register::Resource {
                    "dcl_resource"
                } else {
                    "dcl_uav_typed"
                };
                let samples = if samples != 0 {
                    format!(" ({samples})")
                } else {
                    String::new()
                };
                format!(
                    "{prefix}_{}{samples} {} {argument}",
                    dimension(dim),
                    returns_string(returns)
                )
            }
            BindingKind::Raw { bytes } => {
                if binding.register == Register::ThreadGroupSharedMemory {
                    format!("dcl_tgsm_raw {argument}, {bytes}")
                } else if binding.register == Register::UnorderedAccessView {
                    format!("dcl_uav_raw {argument}")
                } else {
                    format!("dcl_resource_raw {argument}")
                }
            }
            BindingKind::Structured { stride, elements } => {
                if binding.register == Register::ThreadGroupSharedMemory {
                    format!("dcl_tgsm_structured {argument}, {stride}, {elements}")
                } else if binding.register == Register::UnorderedAccessView {
                    format!("dcl_uav_structured {argument}, {stride}")
                } else {
                    format!("dcl_resource_structured {argument}, {stride}")
                }
            }
        };
    }
    if let Some(mode) = decl.interpolation {
        let mode = match mode {
            Interpolation::Undefined => "",
            Interpolation::Flat => "constant ",
            Interpolation::Perspective => "linear ",
            Interpolation::PerspectiveCentroid => "linear centroid ",
            Interpolation::Linear => "linear noperspective ",
            Interpolation::LinearCentroid => "linear noperspective centroid ",
            Interpolation::PerspectiveSample => "linear sample ",
            Interpolation::LinearSample => "linear noperspective sample ",
        };
        let system = decl
            .data
            .first()
            .map_or(String::new(), |&value| format!(", {}", system_value(value)));
        return format!("{name} {mode}{argument}{system}");
    }
    if matches!(
        decl.opcode,
        DclInputSgv | DclInputSiv | DclOutputSgv | DclOutputSiv
    ) {
        return format!("{name} {argument}, {}", system_value(decl.data[0]));
    }
    if decl.opcode == DclGsInputPrimitive {
        let primitive = match decode_gs_input_primitive(controls) {
            1 => "point".into(),
            2 => "line".into(),
            3 => "triangle".into(),
            6 => "lineadj".into(),
            7 => "triangleadj".into(),
            n @ 8..=39 => format!("{}_control_point_patch", n - 7),
            n => n.to_string(),
        };
        return format!("dcl_inputprimitive {primitive} ");
    }
    if decl.opcode == DclGsOutputPrimitiveTopology {
        let topology = match decode_gs_output_primitive_topology(controls) {
            1 => "pointlist",
            2 => "linelist",
            3 => "linestrip",
            4 => "trianglelist",
            5 => "trianglestrip",
            10 => "linelistadj",
            11 => "linestripadj",
            12 => "trianglelistadj",
            13 => "trianglestripadj",
            _ => "undefined",
        };
        return format!("dcl_outputtopology {topology} ");
    }
    if decl.opcode == DclMaxOutputVertexCount {
        return format!("dcl_maxout {}", decl.data[0]);
    }
    if decl.opcode == DclIndexRange {
        return format!("dcl_indexrange {argument} {}", decl.data[0]);
    }
    if decl.opcode == DclInputControlPointCount {
        return format!("{name} {}", decode_input_control_point_count(controls));
    }
    if decl.opcode == DclOutputControlPointCount {
        return format!("{name} {}", decode_output_control_point_count(controls));
    }
    if decl.opcode == DclTessDomain {
        let value = ["undefined", "isoline", "tri", "quad"]
            .get(decode_tess_domain(controls) as usize)
            .unwrap_or(&"undefined");
        return format!("dcl_tessellator_domain domain_{value}");
    }
    if decl.opcode == DclTessPartitioning {
        let value = [
            "undefined",
            "integer",
            "pow2",
            "fractional_odd",
            "fractional_even",
        ]
        .get(decode_tess_partitioning(controls) as usize)
        .unwrap_or(&"undefined");
        return format!("dcl_tessellator_partitioning partitioning_{value}");
    }
    if decl.opcode == DclTessOutputPrimitive {
        let value = ["undefined", "point", "line", "triangle_cw", "triangle_ccw"]
            .get(decode_tess_output_primitive(controls) as usize)
            .unwrap_or(&"undefined");
        return format!("dcl_tessellator_output_primitive output_{value}");
    }
    if argument.is_empty() {
        let args = decl
            .data
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{name} {args}")
    } else {
        format!("{name} {argument}")
    }
}

fn literal(bits: u32, ty: ValueType) -> String {
    let value = f32::from_bits(bits);
    let floating = ty == ValueType::Float
        || (ty == ValueType::Bits && value.is_normal() && (1.0e-6..1.0e9).contains(&value.abs()));
    if floating {
        if value.is_nan() {
            "nan".into()
        } else if value.is_infinite() {
            if value.is_sign_negative() {
                "-inf".into()
            } else {
                "inf".into()
            }
        } else {
            format!("{value:.6}")
        }
    } else {
        (bits as i32).to_string()
    }
}

fn operand(op: &Operand, ty: ValueType, declaration: bool) -> String {
    let indices = |prefix: &str, indices: &[Index], inline| {
        let mut name = prefix.to_string();
        for (i, address) in indices.iter().enumerate() {
            let index = index(address, 1);
            if i == 0 && inline && matches!(address, Index::Direct(_)) {
                name.push_str(&index);
            } else {
                write!(name, "[{index}]").unwrap();
            }
        }
        name
    };
    let mut name = match &op.value {
        OperandValue::Immediate(values) => {
            let count = if op.components == Components::Scalar {
                1
            } else {
                4
            };
            let values: Vec<_> = values[..count]
                .iter()
                .map(|&bits| literal(bits, ty))
                .collect();
            return format!("l({})", values.join(", "));
        }
        OperandValue::Immediate64(values) => {
            let count = if op.components == Components::Scalar {
                1
            } else {
                2
            };
            let values: Vec<_> = values[..count]
                .iter()
                .map(|&bits| format!("{:.6}l", f64::from_bits(bits)))
                .collect();
            return format!("d({})", values.join(", "));
        }
        OperandValue::Constant { slot, byte_offset } => {
            format!("cb{}[{}]", index(slot, 1), index(byte_offset, 16))
        }
        OperandValue::Register {
            register,
            indices: addresses,
        } => {
            let (prefix, inline) = match register {
                Register::Temp => ("r", true),
                Register::Input => ("v", addresses.len() == 1),
                Register::Output => ("o", true),
                Register::IndexableTemp => ("x", true),
                Register::Sampler => ("s", true),
                Register::Resource => ("t", true),
                Register::ImmediateConstantBuffer => ("icb", false),
                Register::Label => ("l", true),
                Register::InputPrimitiveId => ("vPrim", true),
                Register::OutputDepth => ("oDepth", true),
                Register::Null => ("null", true),
                Register::Rasterizer => ("rasterizer", true),
                Register::OutputCoverageMask => ("oMask", true),
                Register::Stream => ("m", true),
                Register::FunctionBody => ("fb", true),
                Register::FunctionTable => ("ft", true),
                Register::Interface => ("fp", true),
                Register::FunctionInput => ("fi", true),
                Register::FunctionOutput => ("fo", true),
                Register::OutputControlPointId => ("vOutputControlPointID", true),
                Register::InputForkInstanceId => ("vForkInstanceID", true),
                Register::InputJoinInstanceId => ("vJoinInstanceID", true),
                Register::InputControlPoint => ("vicp", false),
                Register::OutputControlPoint => ("vocp", false),
                Register::InputPatchConstant => ("vpc", true),
                Register::InputDomainPoint => ("vDomain", true),
                Register::ThisPointer => ("this", true),
                Register::UnorderedAccessView => ("u", true),
                Register::ThreadGroupSharedMemory => ("g", true),
                Register::InputThreadId => ("vThreadID", true),
                Register::InputThreadGroupId => ("vThreadGroupID", true),
                Register::InputThreadIdInGroup => ("vThreadIDInGroup", true),
                Register::InputCoverageMask => ("vCoverage", true),
                Register::InputThreadIdInGroupFlattened => ("vThreadIDInGroupFlattened", true),
                Register::InputGsInstanceId => ("vGSInstanceID", true),
                Register::OutputDepthGreaterEqual => ("oDepthGE", true),
                Register::OutputDepthLessEqual => ("oDepthLE", true),
                Register::CycleCounter => ("cycleCounter", true),
                Register::OutputStencilRef => ("oStencilRef", true),
                Register::InnerCoverage => ("vInnerCoverage", true),
                Register::ConstantBuffer => ("cb", true),
            };
            indices(prefix, addresses, inline)
        }
    };
    let lanes = b"xyzw";
    let sys_decl = declaration
        && matches!(
            op.register(),
            Some((
                Register::UnorderedAccessView
                    | Register::ThreadGroupSharedMemory
                    | Register::Resource,
                _
            ))
        );
    if !sys_decl {
        let selected: Vec<_> = match op.components {
            Components::None | Components::Scalar => Vec::new(),
            Components::Mask(mask) => (0..4).filter(|lane| mask & (1 << lane) != 0).collect(),
            Components::Swizzle(lanes) => lanes.into(),
            Components::Select(lane) => vec![lane],
        };
        if !selected.is_empty() {
            name.push('.');
            name.extend(
                selected
                    .into_iter()
                    .map(|lane| lanes[lane as usize] as char),
            );
        }
    }
    match op.modifier {
        Modifier::None => name,
        Modifier::Negate => format!("-{name}"),
        Modifier::Absolute => format!("|{name}|"),
        Modifier::AbsoluteNegate => format!("-|{name}|"),
    }
}

fn index(address: &Index, stride: u64) -> String {
    match address {
        Index::Direct(value) => (value / stride).to_string(),
        Index::Relative { base, register } => {
            let register = operand(register, ValueType::Int, false);
            format!("{register} + {}", base / stride)
        }
    }
}

fn dimension(dimension: ResourceDimension) -> &'static str {
    use ResourceDimension::*;
    match dimension {
        Buffer => "buffer",
        Texture1D => "texture1d",
        Texture2D => "texture2d",
        Texture2DMs => "texture2dms",
        Texture3D => "texture3d",
        TextureCube => "texturecube",
        Texture1DArray => "texture1darray",
        Texture2DArray => "texture2darray",
        Texture2DMsArray => "texture2dmsarray",
        TextureCubeArray => "texturecubearray",
        RawBuffer => "raw_buffer",
        StructuredBuffer => "structured_buffer",
    }
}

fn returns_string(returns: [ResourceType; 4]) -> String {
    use ResourceType::*;
    let names = returns.map(|resource_type| match resource_type {
        Float => "float",
        Uint => "uint",
        Int => "sint",
        Double => "double",
        Unorm => "unorm",
        Snorm => "snorm",
        Mixed => "mixed",
        Continued => "continued",
        Unused => "unused",
    });
    format!("({})", names.join(","))
}

fn system_value(value: u32) -> &'static str {
    match value {
        1 => "position",
        2 => "clipDistance",
        3 => "cullDistance",
        4 => "renderTargetArrayIndex",
        5 => "viewportArrayIndex",
        6 => "vertexID",
        7 => "primitiveID",
        8 => "instanceID",
        9 => "isFrontFace",
        10 => "sampleIndex",
        11 => "finalQuadUeq0EdgeTessFactor",
        12 => "finalQuadVeq0EdgeTessFactor",
        13 => "finalQuadUeq1EdgeTessFactor",
        14 => "finalQuadVeq1EdgeTessFactor",
        15 => "finalQuadUInsideTessFactor",
        16 => "finalQuadVInsideTessFactor",
        17 => "finalTriUeq0EdgeTessFactor",
        18 => "finalTriVeq0EdgeTessFactor",
        19 => "finalTriWeq0EdgeTessFactor",
        20 => "finalTriInsideTessFactor",
        21 => "finalLineDetailTessFactor",
        22 => "finalLineDensityTessFactor",
        _ => "undefined",
    }
}
