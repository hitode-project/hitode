#![deny(clippy::todo, clippy::unimplemented, clippy::wildcard_enum_match_arm)]

use super::{Body, Instruction as Token, Program, fields::*, operand};
use crate::shader::ir::{self, opcode::Category, *};

pub(super) fn program(program: &Program<'_>) -> Result<Block, Error> {
    let mut tokens = program.instructions.iter().peekable();
    let body = block(&mut tokens, 0)?;
    if tokens.peek().is_some() {
        return Err(Error::Invalid("unmatched control flow"));
    }
    Ok(body)
}

type Tokens<'a, 'b> = std::iter::Peekable<std::slice::Iter<'a, Token<'b>>>;

fn peek(tokens: &mut Tokens<'_, '_>) -> Option<Opcode> {
    if let Some(Token::Operation { opcode, .. }) = tokens.peek() {
        Opcode::decode(*opcode)
    } else {
        None
    }
}

fn end(tokens: &mut Tokens<'_, '_>, opcode: Opcode) -> Result<(), Error> {
    if peek(tokens) != Some(opcode) {
        return Err(Error::Invalid("unmatched control flow"));
    }
    if let Some(Token::Operation {
        body: Body::Operands(args),
        ..
    }) = tokens.next()
        && args.is_empty()
    {
        return Ok(());
    }
    Err(Error::Invalid("control flow delimiter operands"))
}

fn block(tokens: &mut Tokens<'_, '_>, depth: usize) -> Result<Block, Error> {
    if depth > MAX_CONTROL_FLOW_DEPTH {
        return Err(Error::Invalid("control flow nesting"));
    }
    use Opcode::*;
    let mut body = Vec::new();
    while let Some(token) = tokens.peek() {
        let statement = match token {
            Token::CustomData { class, words } => {
                tokens.next();
                match *class {
                    D3D10_SB_CUSTOMDATA_DCL_IMMEDIATE_CONSTANT_BUFFER => {
                        let (rows, tail) = words.as_chunks::<4>();
                        if !tail.is_empty() {
                            return Err(Error::Invalid("immediate constant buffer size"));
                        }
                        Statement::ImmediateConstants(rows.to_vec())
                    }
                    D3D10_SB_CUSTOMDATA_COMMENT
                    | D3D10_SB_CUSTOMDATA_DEBUGINFO
                    | D3D10_SB_CUSTOMDATA_OPAQUE
                    | D3D11_SB_CUSTOMDATA_SHADER_MESSAGE
                    | D3D11_SB_CUSTOMDATA_SHADER_CLIP_PLANE_CONSTANT_MAPPINGS_FOR_DX9 => {
                        Statement::CustomData {
                            class: *class,
                            words: words.to_vec(),
                        }
                    }
                    _ => return Err(Error::Invalid("custom data class")),
                }
            }
            Token::Operation {
                opcode,
                controls,
                extensions,
                body,
                ..
            } => {
                let opcode = Opcode::decode(*opcode).ok_or(Error::Invalid("opcode"))?;
                if matches!(opcode, Else | Endif | Endloop | Endswitch | Case | Default) {
                    break;
                }
                tokens.next();
                match opcode.category() {
                    Category::Operation => {
                        Statement::Execute(operation(opcode, *controls, extensions, body)?)
                    }
                    Category::Declaration => {
                        Statement::Declare(declaration(opcode, *controls, body)?)
                    }
                    Category::Reserved | Category::Custom => {
                        return Err(Error::Invalid("reserved opcode"));
                    }
                    Category::Flow => {
                        let test = operation(opcode, *controls, extensions, body)?;
                        if opcode == If {
                            let accept = block(tokens, depth + 1)?;
                            let reject = if peek(tokens) == Some(Else) {
                                end(tokens, Else)?;
                                Some(block(tokens, depth + 1)?)
                            } else {
                                None
                            };
                            end(tokens, Endif)?;
                            Statement::If {
                                test,
                                accept,
                                reject,
                            }
                        } else if opcode == Loop {
                            let body = block(tokens, depth + 1)?;
                            end(tokens, Endloop)?;
                            Statement::Loop(body)
                        } else {
                            let mut cases = Vec::new();
                            while matches!(peek(tokens), Some(Case | Default)) {
                                let label = if let Some(Token::Operation {
                                    opcode,
                                    controls,
                                    body,
                                    extensions,
                                    ..
                                }) = tokens.next()
                                {
                                    operation(
                                        Opcode::decode(*opcode).unwrap(),
                                        *controls,
                                        extensions,
                                        body,
                                    )?
                                } else {
                                    return Err(Error::Invalid("switch label"));
                                };
                                let value = if label.opcode == Case {
                                    let OperandValue::Immediate(value) = label.sources[0].value
                                    else {
                                        return Err(Error::Invalid("case operand"));
                                    };
                                    Some(value[0])
                                } else {
                                    None
                                };
                                cases.push(ir::Case {
                                    value,
                                    body: block(tokens, depth + 1)?,
                                });
                            }
                            end(tokens, Endswitch)?;
                            Statement::Switch {
                                selector: test.sources.into_iter().next().unwrap(),
                                cases,
                            }
                        }
                    }
                }
            }
        };
        body.push(statement);
    }
    Ok(body)
}

fn operation(
    opcode: Opcode,
    controls: u32,
    extensions: &[super::OpcodeExtension],
    body: &Body<'_>,
) -> Result<ir::Instruction, Error> {
    let (operands, interface_function) = match body {
        Body::Operands(operands) => (operands.as_slice(), None),
        Body::InterfaceCall {
            function,
            interface,
        } => (std::slice::from_ref(interface), Some(*function)),
        Body::Declaration { .. } => return Err(Error::Invalid("expected instruction operands")),
    };
    let (destinations, sources) = opcode.types();
    if operands.len() != destinations.len() + sources.len() {
        return Err(Error::UnsupportedOpcode(opcode));
    }
    let (destinations, sources) = operands.split_at(destinations.len());
    let mut resource = ResourceOperands::default();
    for extension in extensions {
        match extension.kind {
            D3D10_SB_EXTENDED_OPCODE_EMPTY => {}
            D3D10_SB_EXTENDED_OPCODE_SAMPLE_CONTROLS => {
                resource.offset = Some(std::array::from_fn(|lane| {
                    let nibble = decode_immediate_address_offset(lane as u32, extension.controls);
                    (nibble as i32) << 28 >> 28
                }));
            }
            D3D11_SB_EXTENDED_OPCODE_RESOURCE_DIM => {
                resource.dimension = Some((
                    dimension(decode_extended_resource_dimension(extension.controls))?,
                    decode_extended_resource_dimension_structure_stride(extension.controls),
                ));
            }
            D3D11_SB_EXTENDED_OPCODE_RESOURCE_RETURN_TYPE => {
                resource.returns = Some(return_types(|lane| {
                    decode_extended_resource_return_type(extension.controls, lane)
                })?);
            }
            _ => return Err(Error::Invalid("opcode extension")),
        }
    }
    Ok(ir::Instruction {
        opcode,
        destinations: destinations
            .iter()
            .map(convert_operand)
            .collect::<Result<_, _>>()?,
        sources: sources
            .iter()
            .map(convert_operand)
            .collect::<Result<_, _>>()?,
        saturate: controls & D3D10_SB_INSTRUCTION_SATURATE_MASK != 0,
        test_nonzero: decode_instruction_test_boolean(controls)
            == D3D10_SB_INSTRUCTION_TEST_NONZERO,
        precise: decode_instruction_precise_values(controls) as u8,
        controls,
        resource,
        interface_function,
        flush_inputs: false,
        flush_outputs: false,
    })
}

fn declaration(opcode: Opcode, controls: u32, body: &Body<'_>) -> Result<Declaration, Error> {
    let Body::Declaration { operand, words } = body else {
        return Err(Error::Invalid("expected declaration"));
    };
    let converted = operand.as_ref().map(convert_operand).transpose()?;
    let interpolation = if matches!(
        opcode,
        Opcode::DclInputPs | Opcode::DclInputPsSgv | Opcode::DclInputPsSiv
    ) {
        Some(match decode_input_interpolation_mode(controls) {
            D3D10_SB_INTERPOLATION_UNDEFINED => Interpolation::Undefined,
            D3D10_SB_INTERPOLATION_CONSTANT => Interpolation::Flat,
            D3D10_SB_INTERPOLATION_LINEAR => Interpolation::Perspective,
            D3D10_SB_INTERPOLATION_LINEAR_CENTROID => Interpolation::PerspectiveCentroid,
            D3D10_SB_INTERPOLATION_LINEAR_NOPERSPECTIVE => Interpolation::Linear,
            D3D10_SB_INTERPOLATION_LINEAR_NOPERSPECTIVE_CENTROID => Interpolation::LinearCentroid,
            D3D10_SB_INTERPOLATION_LINEAR_SAMPLE => Interpolation::PerspectiveSample,
            D3D10_SB_INTERPOLATION_LINEAR_NOPERSPECTIVE_SAMPLE => Interpolation::LinearSample,
            _ => return Err(Error::Invalid("interpolation mode")),
        })
    } else {
        None
    };
    let binding = match (opcode, converted.as_ref().map(|operand| &operand.value)) {
        (Opcode::DclConstantBuffer, Some(OperandValue::Constant { slot, byte_offset })) => {
            Some(Binding {
                register: Register::ConstantBuffer,
                slot: slot
                    .direct()
                    .ok_or(Error::Invalid("constant buffer slot"))?,
                kind: BindingKind::ConstantBuffer {
                    bytes: byte_offset
                        .direct()
                        .ok_or(Error::Invalid("constant buffer size"))?,
                    dynamic_indexing: controls & D3D10_SB_CONSTANT_BUFFER_ACCESS_PATTERN_MASK != 0,
                },
                flags: 0,
            })
        }
        (_, Some(OperandValue::Register { register, indices })) => {
            use Opcode::*;
            let first = || {
                words
                    .first()
                    .copied()
                    .ok_or(Error::Invalid("binding declaration size"))
            };
            let kind = if opcode == DclSampler {
                Some(BindingKind::Sampler(match decode_sampler_mode(controls) {
                    D3D10_SB_SAMPLER_MODE_DEFAULT => SamplerMode::Default,
                    D3D10_SB_SAMPLER_MODE_COMPARISON => SamplerMode::Comparison,
                    D3D10_SB_SAMPLER_MODE_MONO => SamplerMode::Mono,
                    _ => return Err(Error::Invalid("sampler mode")),
                }))
            } else if matches!(opcode, DclResource | DclUnorderedAccessViewTyped) {
                let returns = first()?;
                Some(BindingKind::Typed {
                    dimension: dimension(decode_resource_dimension(controls))?,
                    samples: if opcode == DclResource {
                        decode_resource_sample_count(controls)
                    } else {
                        0
                    },
                    returns: return_types(|lane| decode_resource_return_type(returns, lane))?,
                })
            } else if matches!(
                opcode,
                DclResourceRaw | DclUnorderedAccessViewRaw | DclThreadGroupSharedMemoryRaw
            ) {
                Some(BindingKind::Raw {
                    bytes: if opcode == DclThreadGroupSharedMemoryRaw {
                        first()?
                    } else {
                        0
                    },
                })
            } else if matches!(
                opcode,
                DclResourceStructured
                    | DclUnorderedAccessViewStructured
                    | DclThreadGroupSharedMemoryStructured
            ) {
                Some(BindingKind::Structured {
                    stride: first()?,
                    elements: if opcode == DclThreadGroupSharedMemoryStructured {
                        *words.get(1).ok_or(Error::Invalid("shared array size"))?
                    } else {
                        0
                    },
                })
            } else {
                None
            };
            kind.map(|kind| {
                Ok(Binding {
                    register: *register,
                    slot: indices
                        .first()
                        .and_then(Index::direct)
                        .ok_or(Error::Invalid("binding slot"))?,
                    kind,
                    flags: if *register == Register::UnorderedAccessView {
                        controls & !D3D10_SB_RESOURCE_DIMENSION_MASK
                    } else {
                        0
                    },
                })
            })
            .transpose()?
        }
        (
            _,
            Some(
                OperandValue::Constant { .. }
                | OperandValue::Immediate(_)
                | OperandValue::Immediate64(_),
            )
            | None,
        ) => None,
    };
    Ok(Declaration {
        opcode,
        operand: converted,
        controls,
        data: words.to_vec(),
        binding,
        interpolation,
    })
}

fn dimension(value: u32) -> Result<ResourceDimension, Error> {
    use ResourceDimension::*;
    Ok(match value {
        D3D10_SB_RESOURCE_DIMENSION_BUFFER => Buffer,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE1D => Texture1D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2D => Texture2D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DMS => Texture2DMs,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE3D => Texture3D,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURECUBE => TextureCube,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE1DARRAY => Texture1DArray,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DARRAY => Texture2DArray,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURE2DMSARRAY => Texture2DMsArray,
        D3D10_SB_RESOURCE_DIMENSION_TEXTURECUBEARRAY => TextureCubeArray,
        D3D11_SB_RESOURCE_DIMENSION_RAW_BUFFER => RawBuffer,
        D3D11_SB_RESOURCE_DIMENSION_STRUCTURED_BUFFER => StructuredBuffer,
        _ => return Err(Error::Invalid("resource dimension")),
    })
}

fn return_types(get: impl Fn(u32) -> u32) -> Result<[ResourceType; 4], Error> {
    use ResourceType::*;
    let mut types = [Unused; 4];
    for (lane, ty) in types.iter_mut().enumerate() {
        *ty = match get(lane as u32) {
            D3D10_SB_RETURN_TYPE_UNORM => Unorm,
            D3D10_SB_RETURN_TYPE_SNORM => Snorm,
            D3D10_SB_RETURN_TYPE_SINT => Int,
            D3D10_SB_RETURN_TYPE_UINT => Uint,
            D3D10_SB_RETURN_TYPE_FLOAT => Float,
            D3D10_SB_RETURN_TYPE_MIXED => Mixed,
            D3D11_SB_RETURN_TYPE_DOUBLE => Double,
            D3D11_SB_RETURN_TYPE_CONTINUED => Continued,
            D3D11_SB_RETURN_TYPE_UNUSED => Unused,
            _ => return Err(Error::Invalid("resource return type")),
        };
    }
    Ok(types)
}

fn convert_operand(operand: &operand::Operand<'_>) -> Result<Operand, Error> {
    let components = match operand.components {
        D3D10_SB_OPERAND_0_COMPONENT => Components::None,
        D3D10_SB_OPERAND_1_COMPONENT => Components::Scalar,
        D3D10_SB_OPERAND_4_COMPONENT => {
            match decode_operand_4_component_selection_mode(operand.controls) {
                D3D10_SB_OPERAND_4_COMPONENT_MASK_MODE => Components::Mask(
                    (decode_operand_4_component_mask(operand.controls)
                        >> D3D10_SB_OPERAND_4_COMPONENT_MASK_SHIFT) as u8,
                ),
                D3D10_SB_OPERAND_4_COMPONENT_SWIZZLE_MODE => {
                    Components::Swizzle(std::array::from_fn(|lane| {
                        decode_operand_4_component_swizzle_source(operand.controls, lane as u32)
                            as u8
                    }))
                }
                D3D10_SB_OPERAND_4_COMPONENT_SELECT_1_MODE => {
                    Components::Select(decode_operand_4_component_select_1(operand.controls) as u8)
                }
                _ => return Err(Error::Invalid("component selection")),
            }
        }
        D3D10_SB_OPERAND_N_COMPONENT => return Err(Error::Invalid("N-component operand")),
        _ => return Err(Error::Invalid("operand components")),
    };
    let mut modifier = Modifier::None;
    let mut precision = Precision::Default;
    let mut non_uniform = false;
    for extension in &operand.extensions {
        match extension.kind {
            D3D10_SB_EXTENDED_OPERAND_EMPTY => {}
            D3D10_SB_EXTENDED_OPERAND_MODIFIER => {
                modifier = match decode_operand_modifier(extension.controls) {
                    D3D10_SB_OPERAND_MODIFIER_NONE => Modifier::None,
                    D3D10_SB_OPERAND_MODIFIER_NEG => Modifier::Negate,
                    D3D10_SB_OPERAND_MODIFIER_ABS => Modifier::Absolute,
                    D3D10_SB_OPERAND_MODIFIER_ABSNEG => Modifier::AbsoluteNegate,
                    _ => return Err(Error::Invalid("operand modifier")),
                };
                precision = match decode_operand_min_precision(extension.controls) {
                    D3D11_SB_OPERAND_MIN_PRECISION_DEFAULT => Precision::Default,
                    D3D11_SB_OPERAND_MIN_PRECISION_FLOAT_16 => Precision::Float16,
                    D3D11_SB_OPERAND_MIN_PRECISION_FLOAT_2_8 => Precision::Float2_8,
                    D3D11_SB_OPERAND_MIN_PRECISION_SINT_16 => Precision::Int16,
                    D3D11_SB_OPERAND_MIN_PRECISION_UINT_16 => Precision::Uint16,
                    _ => return Err(Error::Invalid("operand precision")),
                };
                non_uniform = decode_operand_non_uniform(extension.controls) != 0;
            }
            _ => return Err(Error::Invalid("operand extension")),
        }
    }
    let value = operand_value(operand)?;
    Ok(Operand {
        value,
        components,
        modifier,
        precision,
        non_uniform,
    })
}

fn index(index: &operand::Index<'_>, stride: u64) -> Result<Index, Error> {
    let pair = |[lo, hi]: [u32; 2]| u64::from(lo) | u64::from(hi) << 32;
    let relative = |base: u64, operand| {
        Ok(Index::Relative {
            base: base
                .checked_mul(stride)
                .ok_or(Error::Invalid("relative base offset"))?,
            register: Box::new(convert_operand(operand)?),
        })
    };
    let direct = |base: u64| {
        Ok(Index::Direct(
            base.checked_mul(stride)
                .ok_or(Error::Invalid("index offset"))?,
        ))
    };
    match index {
        operand::Index::Immediate32(base) => direct(u64::from(*base)),
        operand::Index::Immediate64(base) => direct(pair(*base)),
        operand::Index::Relative(operand) => relative(0, operand),
        operand::Index::Immediate32Relative(base, operand) => relative(u64::from(*base), operand),
        operand::Index::Immediate64Relative(base, operand) => relative(pair(*base), operand),
    }
}

fn operand_value(operand: &operand::Operand<'_>) -> Result<OperandValue, Error> {
    use Register::*;
    let register = match operand.kind {
        D3D10_SB_OPERAND_TYPE_TEMP => Temp,
        D3D10_SB_OPERAND_TYPE_INPUT => Input,
        D3D10_SB_OPERAND_TYPE_OUTPUT => Output,
        D3D10_SB_OPERAND_TYPE_INDEXABLE_TEMP => IndexableTemp,
        D3D10_SB_OPERAND_TYPE_IMMEDIATE32 => {
            let mut values = [0; 4];
            values[..operand.literals.len()].copy_from_slice(operand.literals);
            return Ok(OperandValue::Immediate(values));
        }
        D3D10_SB_OPERAND_TYPE_IMMEDIATE64 => {
            let mut values = [0; 4];
            for (value, [lo, hi]) in values.iter_mut().zip(operand.literals.as_chunks::<2>().0) {
                *value = u64::from(*lo) | u64::from(*hi) << 32;
            }
            return Ok(OperandValue::Immediate64(values));
        }
        D3D10_SB_OPERAND_TYPE_SAMPLER => Sampler,
        D3D10_SB_OPERAND_TYPE_RESOURCE => Resource,
        D3D10_SB_OPERAND_TYPE_CONSTANT_BUFFER => {
            let [slot, address] = operand.indices.as_slice() else {
                return Err(Error::Invalid("constant buffer index count"));
            };
            return Ok(OperandValue::Constant {
                slot: index(slot, 1)?,
                byte_offset: index(address, 16)?,
            });
        }
        D3D10_SB_OPERAND_TYPE_IMMEDIATE_CONSTANT_BUFFER => ImmediateConstantBuffer,
        D3D10_SB_OPERAND_TYPE_LABEL => Label,
        D3D10_SB_OPERAND_TYPE_INPUT_PRIMITIVEID => InputPrimitiveId,
        D3D10_SB_OPERAND_TYPE_OUTPUT_DEPTH => OutputDepth,
        D3D10_SB_OPERAND_TYPE_NULL => Null,
        D3D10_SB_OPERAND_TYPE_RASTERIZER => Rasterizer,
        D3D10_SB_OPERAND_TYPE_OUTPUT_COVERAGE_MASK => OutputCoverageMask,
        D3D11_SB_OPERAND_TYPE_STREAM => Stream,
        D3D11_SB_OPERAND_TYPE_FUNCTION_BODY => FunctionBody,
        D3D11_SB_OPERAND_TYPE_FUNCTION_TABLE => FunctionTable,
        D3D11_SB_OPERAND_TYPE_INTERFACE => Interface,
        D3D11_SB_OPERAND_TYPE_FUNCTION_INPUT => FunctionInput,
        D3D11_SB_OPERAND_TYPE_FUNCTION_OUTPUT => FunctionOutput,
        D3D11_SB_OPERAND_TYPE_OUTPUT_CONTROL_POINT_ID => OutputControlPointId,
        D3D11_SB_OPERAND_TYPE_INPUT_FORK_INSTANCE_ID => InputForkInstanceId,
        D3D11_SB_OPERAND_TYPE_INPUT_JOIN_INSTANCE_ID => InputJoinInstanceId,
        D3D11_SB_OPERAND_TYPE_INPUT_CONTROL_POINT => InputControlPoint,
        D3D11_SB_OPERAND_TYPE_OUTPUT_CONTROL_POINT => OutputControlPoint,
        D3D11_SB_OPERAND_TYPE_INPUT_PATCH_CONSTANT => InputPatchConstant,
        D3D11_SB_OPERAND_TYPE_INPUT_DOMAIN_POINT => InputDomainPoint,
        D3D11_SB_OPERAND_TYPE_THIS_POINTER => ThisPointer,
        D3D11_SB_OPERAND_TYPE_UNORDERED_ACCESS_VIEW => UnorderedAccessView,
        D3D11_SB_OPERAND_TYPE_THREAD_GROUP_SHARED_MEMORY => ThreadGroupSharedMemory,
        D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID => InputThreadId,
        D3D11_SB_OPERAND_TYPE_INPUT_THREAD_GROUP_ID => InputThreadGroupId,
        D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP => InputThreadIdInGroup,
        D3D11_SB_OPERAND_TYPE_INPUT_COVERAGE_MASK => InputCoverageMask,
        D3D11_SB_OPERAND_TYPE_INPUT_THREAD_ID_IN_GROUP_FLATTENED => InputThreadIdInGroupFlattened,
        D3D11_SB_OPERAND_TYPE_INPUT_GS_INSTANCE_ID => InputGsInstanceId,
        D3D11_SB_OPERAND_TYPE_OUTPUT_DEPTH_GREATER_EQUAL => OutputDepthGreaterEqual,
        D3D11_SB_OPERAND_TYPE_OUTPUT_DEPTH_LESS_EQUAL => OutputDepthLessEqual,
        D3D11_SB_OPERAND_TYPE_CYCLE_COUNTER => CycleCounter,
        D3D11_SB_OPERAND_TYPE_OUTPUT_STENCIL_REF => OutputStencilRef,
        D3D11_SB_OPERAND_TYPE_INNER_COVERAGE => InnerCoverage,
        _ => return Err(Error::Invalid("operand type")),
    };
    Ok(OperandValue::Register {
        register,
        indices: operand
            .indices
            .iter()
            .map(|value| index(value, 1))
            .collect::<Result<_, _>>()?,
    })
}
