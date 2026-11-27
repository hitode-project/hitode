//! Fragment epilogues for ordered, single-sample float color attachments.

use crate::pipeline::{BlendComponent, BlendOp, ColorTarget, Factor};
use naga::{Expression as E, Handle, Span, Type, TypeInner};

type Value = Handle<E>;

#[derive(Clone, Copy)]
pub(crate) struct AttachmentBlend<'a> {
    pub colors: &'a [ColorTarget],
    pub slots: u8,
    pub reads: u8,
    pub alpha_to_coverage: bool,
    pub set: u32,
    pub push_offset: u32,
}

pub(crate) fn specialize(
    module: &mut naga::Module,
    entry: &str,
    blend: AttachmentBlend<'_>,
    heap_push: Option<Handle<naga::GlobalVariable>>,
) {
    let function = &mut module
        .entry_points
        .iter_mut()
        .find(|ep| ep.stage == naga::ShaderStage::Fragment && ep.name == entry)
        .unwrap()
        .function;
    let Some(result) = function.result.as_mut() else {
        return;
    };
    let original = result.ty;
    let fields = match &module.types[original].inner {
        TypeInner::Struct { members, .. } => members.clone(),
        _ => vec![naga::StructMember {
            name: None,
            ty: original,
            binding: result.binding.clone(),
            offset: 0,
        }],
    };
    let location = |member: &naga::StructMember| match member.binding {
        Some(naga::Binding::Location {
            location,
            blend_src,
            ..
        }) => Some((location, blend_src.unwrap_or(0))),
        _ => None,
    };
    let first = fields
        .iter()
        .position(|member| location(member) == Some((0, 0)));
    let second = fields
        .iter()
        .position(|member| location(member) == Some((0, 1)))
        // The compact D3D11 shader keeps SV_Target1 at location 1.
        .or_else(|| {
            fields
                .iter()
                .position(|member| location(member) == Some((1, 0)))
        });
    let alpha_to_coverage = blend.alpha_to_coverage
        && blend.reads & 1 != 0
        && first.is_some()
        && !fields.iter().any(|member| {
            member.binding == Some(naga::Binding::BuiltIn(naga::BuiltIn::SampleMask))
        });
    let output = if alpha_to_coverage {
        let offset = match module.types[original].inner {
            TypeInner::Struct { span, .. } => span,
            _ => 16,
        };
        let mask = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Scalar(naga::Scalar::U32),
            },
            Span::UNDEFINED,
        );
        let mut members = fields.clone();
        members.push(naga::StructMember {
            name: None,
            ty: mask,
            binding: Some(naga::Binding::BuiltIn(naga::BuiltIn::SampleMask)),
            offset,
        });
        let ty = module.types.insert(
            Type {
                name: None,
                inner: TypeInner::Struct {
                    members,
                    span: offset + 16,
                },
            },
            Span::UNDEFINED,
        );
        *result = naga::FunctionResult { ty, binding: None };
        ty
    } else {
        original
    };
    let struct_result = matches!(module.types[original].inner, TypeInner::Struct { .. });
    let mut plans = Vec::new();
    let image = module.types.insert(
        Type {
            name: None,
            inner: TypeInner::Image {
                dim: naga::ImageDimension::D2,
                arrayed: false,
                class: naga::ImageClass::Sampled {
                    kind: naga::ScalarKind::Float,
                    multi: false,
                },
            },
        },
        Span::UNDEFINED,
    );
    let expressions = &mut function.expressions;
    let zero = append(expressions, E::Literal(naga::Literal::F32(0.0)));
    let one = append(expressions, E::Literal(naga::Literal::F32(1.0)));
    let izero = append(expressions, E::Literal(naga::Literal::I32(0)));
    let no_coverage = append(expressions, E::Literal(naga::Literal::U32(0)));
    let covered = append(expressions, E::Literal(naga::Literal::U32(1)));
    for (slot, color) in slots(blend.slots).zip(blend.colors) {
        if blend.reads & (1 << slot) == 0 {
            continue;
        }
        let Some(field) = fields
            .iter()
            .position(|member| location(member) == Some((slot, 0)))
        else {
            continue;
        };
        let global = module.global_variables.append(
            naga::GlobalVariable {
                name: None,
                space: naga::AddressSpace::Handle,
                binding: Some(naga::ResourceBinding {
                    group: blend.set,
                    binding: slot,
                }),
                ty: image,
                init: None,
                memory_decorations: Default::default(),
            },
            Span::UNDEFINED,
        );
        plans.push((
            field,
            color.blend.unwrap(),
            append(expressions, E::GlobalVariable(global)),
        ));
    }
    if plans.is_empty() {
        return;
    }
    let constant = if plans.iter().any(|&(_, (rgb, alpha), _)| {
        [rgb.src, rgb.dst, alpha.src, alpha.dst]
            .iter()
            .any(|factor| matches!(factor, Factor::Constant | Factor::OneMinusConstant))
    }) {
        let push = heap_push.unwrap_or_else(|| {
            let ty = fields[plans[0].0].ty;
            module.global_variables.append(
                naga::GlobalVariable {
                    name: None,
                    space: naga::AddressSpace::Immediate,
                    binding: None,
                    ty,
                    init: None,
                    memory_decorations: Default::default(),
                },
                Span::UNDEFINED,
            )
        });
        Some(append(expressions, E::GlobalVariable(push)))
    } else {
        None
    };
    let mut epilogue = Epilogue {
        expressions,
        struct_result,
        output,
        fields: &fields,
        plans: &plans,
        first: first.filter(|_| alpha_to_coverage),
        second,
        constant,
        push_word: heap_push.map(|_| blend.push_offset / 4),
        zero,
        one,
        izero,
        no_coverage,
        covered,
    };
    epilogue.block(&mut function.body);
}

fn slots(mask: u8) -> impl Iterator<Item = u32> {
    (0..8).filter(move |slot| mask & (1 << slot) != 0)
}

fn append(arena: &mut naga::Arena<E>, expression: E) -> Value {
    arena.append(expression, Span::UNDEFINED)
}

struct Epilogue<'a> {
    expressions: &'a mut naga::Arena<E>,
    struct_result: bool,
    output: Handle<Type>,
    fields: &'a [naga::StructMember],
    plans: &'a [(usize, (BlendComponent, BlendComponent), Value)],
    first: Option<usize>,
    second: Option<usize>,
    constant: Option<Value>,
    push_word: Option<u32>,
    zero: Value,
    one: Value,
    izero: Value,
    no_coverage: Value,
    covered: Value,
}

impl Epilogue<'_> {
    fn add(&mut self, expression: E) -> Value {
        append(self.expressions, expression)
    }

    fn access(&mut self, base: Value, index: u32) -> Value {
        self.add(E::AccessIndex { base, index })
    }

    fn load(&mut self, pointer: Value) -> Value {
        self.add(E::Load { pointer })
    }

    fn binary(&mut self, op: naga::BinaryOperator, left: Value, right: Value) -> Value {
        self.add(E::Binary { op, left, right })
    }

    fn minimum(&mut self, fun: naga::MathFunction, a: Value, b: Value) -> Value {
        // D3D min/max select the non-NaN argument. GLSL.std.450 FMin/FMax
        // leave the result undefined when exactly one argument is NaN.
        let result = self.add(E::Math {
            fun,
            arg: a,
            arg1: Some(b),
            arg2: None,
            arg3: None,
        });
        let nan_b = self.add(E::Relational {
            fun: naga::RelationalFunction::IsNan,
            argument: b,
        });
        let result = self.add(E::Select {
            condition: nan_b,
            accept: a,
            reject: result,
        });
        let nan_a = self.add(E::Relational {
            fun: naga::RelationalFunction::IsNan,
            argument: a,
        });
        self.add(E::Select {
            condition: nan_a,
            accept: b,
            reject: result,
        })
    }

    fn constant(&mut self, lane: u32) -> Value {
        let Some(constant) = self.constant else {
            return self.zero;
        };
        if let Some(word) = self.push_word {
            let array = self.access(constant, 0);
            let pointer = self.access(array, word + lane);
            let bits = self.load(pointer);
            self.add(E::As {
                expr: bits,
                kind: naga::ScalarKind::Float,
                convert: None,
            })
        } else {
            let pointer = self.access(constant, lane);
            self.load(pointer)
        }
    }

    fn block(&mut self, block: &mut naga::Block) {
        let mut output = naga::Block::new();
        for (mut statement, span) in std::mem::take(block).span_into_iter() {
            match &mut statement {
                naga::Statement::Block(block) => self.block(block),
                naga::Statement::If { accept, reject, .. } => {
                    self.block(accept);
                    self.block(reject);
                }
                naga::Statement::Switch { cases, .. } => {
                    for case in cases {
                        self.block(&mut case.body);
                    }
                }
                naga::Statement::Loop {
                    body, continuing, ..
                } => {
                    self.block(body);
                    self.block(continuing);
                }
                naga::Statement::Return { value: Some(value) } => {
                    let start = self.expressions.len();
                    *value = self.color_outputs(*value);
                    output.push(
                        naga::Statement::Emit(self.expressions.range_from(start)),
                        span,
                    );
                }
                _ => {}
            }
            output.push(statement, span);
        }
        *block = output;
    }

    fn color_outputs(&mut self, result: Value) -> Value {
        let original = if self.struct_result {
            (0..self.fields.len())
                .map(|field| self.access(result, field as u32))
                .collect()
        } else {
            vec![result]
        };
        let mut output = original.clone();
        for &(field, (rgb, alpha), image) in self.plans {
            let coordinate = self.add(E::Splat {
                size: naga::VectorSize::Bi,
                value: self.izero,
            });
            let dst = self.add(E::ImageLoad {
                image,
                coordinate,
                array_index: None,
                sample: None,
                level: Some(self.izero),
            });
            let src = original[field];
            let src1 = self.second.map_or(src, |second| original[second]);
            let components = (0..4)
                .map(|lane| self.channel(if lane == 3 { alpha } else { rgb }, src, src1, dst, lane))
                .collect();
            output[field] = self.add(E::Compose {
                ty: self.fields[field].ty,
                components,
            });
        }
        if let Some(first) = self.first {
            let alpha = self.access(original[first], 3);
            let covered = self.binary(naga::BinaryOperator::GreaterEqual, alpha, self.one);
            output.push(self.add(E::Select {
                condition: covered,
                accept: self.covered,
                reject: self.no_coverage,
            }));
        }
        if self.struct_result || self.first.is_some() {
            self.add(E::Compose {
                ty: self.output,
                components: output,
            })
        } else {
            output[0]
        }
    }

    fn channel(
        &mut self,
        blend: BlendComponent,
        src: Value,
        src1: Value,
        dst: Value,
        lane: u32,
    ) -> Value {
        use naga::{BinaryOperator as Bin, MathFunction as Math};
        let s = self.access(src, lane);
        let d = self.access(dst, lane);
        let op = match blend.op {
            BlendOp::Min => return self.minimum(Math::Min, s, d),
            BlendOp::Max => return self.minimum(Math::Max, s, d),
            BlendOp::Add => Bin::Add,
            BlendOp::Subtract | BlendOp::ReverseSubtract => Bin::Subtract,
        };
        let factor = |this: &mut Self, factor: Factor| {
            let (value, invert) = match factor {
                Factor::Zero => (this.zero, false),
                Factor::One => (this.one, false),
                Factor::Src | Factor::OneMinusSrc => (s, factor == Factor::OneMinusSrc),
                Factor::Dst | Factor::OneMinusDst => (d, factor == Factor::OneMinusDst),
                Factor::SrcAlpha | Factor::OneMinusSrcAlpha => {
                    (this.access(src, 3), factor == Factor::OneMinusSrcAlpha)
                }
                Factor::DstAlpha | Factor::OneMinusDstAlpha => {
                    (this.access(dst, 3), factor == Factor::OneMinusDstAlpha)
                }
                Factor::Src1 | Factor::OneMinusSrc1 => {
                    (this.access(src1, lane), factor == Factor::OneMinusSrc1)
                }
                Factor::Src1Alpha | Factor::OneMinusSrc1Alpha => {
                    (this.access(src1, 3), factor == Factor::OneMinusSrc1Alpha)
                }
                Factor::Constant | Factor::OneMinusConstant => {
                    (this.constant(lane), factor == Factor::OneMinusConstant)
                }
                Factor::SrcAlphaSaturated if lane == 3 => (this.one, false),
                Factor::SrcAlphaSaturated => {
                    let src_alpha = this.access(src, 3);
                    let dst_alpha = this.access(dst, 3);
                    let remaining = this.binary(Bin::Subtract, this.one, dst_alpha);
                    (this.minimum(Math::Min, src_alpha, remaining), false)
                }
            };
            if invert {
                this.binary(Bin::Subtract, this.one, value)
            } else {
                value
            }
        };
        let sf = factor(self, blend.src);
        let df = factor(self, blend.dst);
        let s = self.binary(Bin::Multiply, s, sf);
        let d = self.binary(Bin::Multiply, d, df);
        if blend.op == BlendOp::ReverseSubtract {
            self.binary(op, d, s)
        } else {
            self.binary(op, s, d)
        }
    }
}

/// Change the reserved texture loads into the subpass attachment reads they
/// represent. Keep real sampled images that share the Naga type intact.
pub(crate) fn input_attachments(words: &mut Vec<u32>, set: u32) {
    use super::compile::instructions;
    use std::collections::{HashMap, HashSet};

    let mut sets = HashSet::new();
    let mut bindings = HashMap::new();
    let mut ptrs = HashMap::new();
    let mut images = HashMap::new();
    for i in instructions(words) {
        match i[0] & 0xffff {
            71 if i[2] == 34 && i[3] == set => {
                sets.insert(i[1]);
            }
            71 if i[2] == 33 => {
                bindings.insert(i[1], i[3]);
            }
            32 => {
                ptrs.insert(i[1], (i[2], i[3]));
            }
            25 => {
                images.insert(i[1], i);
            }
            _ => {}
        }
    }
    if sets.is_empty() {
        return;
    }
    let mut result = words[..5].to_vec();
    // InputAttachment and the D3D float arithmetic modes. Signed-zero/Inf/NaN
    // preservation and SPV_KHR_float_controls are already present.
    result.extend([2 << 16 | 17, 40, 2 << 16 | 17, 4465, 2 << 16 | 17, 4467]);
    let mut bound = words[3];
    let mut types = HashMap::new();
    let mut vars = HashMap::new();
    let mut loads = HashSet::new();
    let mut entry = None;
    for i in instructions(words) {
        match i[0] & 0xffff {
            15 => entry = Some(i[2]),
            16 if entry.is_some() => {
                let entry = entry.take().unwrap();
                result.extend([4 << 16 | 16, entry, 4460, 32, 4 << 16 | 16, entry, 4462, 32]);
            }
            // The binding number in our separate set is the subpass index.
            71 if i[2] == 33 && sets.contains(&i[1]) => {
                result.extend([4 << 16 | 71, i[1], 43, bindings[&i[1]]]);
            }
            // OpVariable: insert a SubpassData image and its pointer.
            59 if sets.contains(&i[2]) => {
                let (space, image) = ptrs[&i[1]];
                let (ty, pointer) = *types.entry(i[1]).or_insert_with(|| {
                    let ty = bound;
                    let pointer = bound + 1;
                    bound += 2;
                    let start = result.len();
                    result.extend_from_slice(images[&image]);
                    result[start + 1] = ty;
                    result[start + 3] = 6; // DimSubpassData
                    result[start + 7] = 2; // Known to be used without a sampler.
                    result.extend([4 << 16 | 32, pointer, space, ty]);
                    (ty, pointer)
                });
                vars.insert(i[2], ty);
                result.extend([i[0], pointer, i[2], i[3]]);
                continue;
            }
            // OpLoad: use the new image type.
            61 if vars.contains_key(&i[3]) => {
                loads.insert(i[2]);
                result.extend([i[0], vars[&i[3]], i[2]]);
                result.extend_from_slice(&i[3..]);
                continue;
            }
            // OpImageFetch => OpImageRead without the Lod operand.
            95 if loads.contains(&i[3]) => {
                result.push(5 << 16 | 98);
                result.extend_from_slice(&i[1..5]);
                continue;
            }
            _ => {}
        }
        result.extend_from_slice(i);
    }
    result[3] = bound;
    *words = result;
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "
        struct Out {
            @location(0) color: vec4<f32>,
            @location(1) factor: vec4<f32>,
        }
        @fragment fn main(@location(0) color: vec4<f32>, @location(1) factor: vec4<f32>) -> Out {
            return Out(color, factor);
        }
    ";

    fn component(src: Factor, dst: Factor, op: BlendOp) -> BlendComponent {
        BlendComponent { src, dst, op }
    }

    #[derive(Clone, Debug)]
    enum Cpu {
        Scalar(f32),
        Fields(Vec<Cpu>),
    }

    impl Cpu {
        fn vector(values: [f32; 4]) -> Self {
            Self::Fields(values.into_iter().map(Self::Scalar).collect())
        }
        fn scalar(self) -> f32 {
            let Self::Scalar(value) = self else {
                panic!("{self:?}")
            };
            value
        }
        fn at(&self, index: u32) -> Self {
            let Self::Fields(fields) = self else {
                panic!("{self:?}")
            };
            fields[index as usize].clone()
        }
    }

    fn evaluate(
        expressions: &naga::Arena<E>,
        value: Value,
        src: [f32; 4],
        src1: [f32; 4],
        dst: [f32; 4],
        constant: [f32; 4],
    ) -> Cpu {
        use naga::{BinaryOperator as Bin, MathFunction as Math};
        let eval = |value| evaluate(expressions, value, src, src1, dst, constant);
        let scalar = |value| eval(value).scalar();
        Cpu::Scalar(match &expressions[value] {
            E::Literal(naga::Literal::F32(value)) => *value,
            E::Literal(naga::Literal::U32(value)) => *value as f32,
            E::FunctionArgument(0) => return Cpu::vector(src),
            E::FunctionArgument(1) => return Cpu::vector(src1),
            E::GlobalVariable(_) => return Cpu::vector(constant),
            E::ImageLoad { .. } => return Cpu::vector(dst),
            E::Load { pointer } => return eval(*pointer),
            E::AccessIndex { base, index } => return eval(*base).at(*index),
            E::Compose { components, .. } => {
                return Cpu::Fields(components.iter().map(|&member| eval(member)).collect());
            }
            E::Binary { op, left, right } => {
                let (a, b) = (scalar(*left), scalar(*right));
                match op {
                    Bin::Add => a + b,
                    Bin::Subtract => a - b,
                    Bin::Multiply => a * b,
                    Bin::GreaterEqual => u32::from(a >= b) as f32,
                    _ => panic!("{op:?}"),
                }
            }
            E::Relational {
                fun: naga::RelationalFunction::IsNan,
                argument,
            } => u32::from(scalar(*argument).is_nan()) as f32,
            E::Select {
                condition,
                accept,
                reject,
            } => {
                return eval(if scalar(*condition) != 0.0 {
                    *accept
                } else {
                    *reject
                });
            }
            E::Math {
                fun,
                arg,
                arg1: Some(b),
                ..
            } => match fun {
                Math::Min => scalar(*arg).min(scalar(*b)),
                Math::Max => scalar(*arg).max(scalar(*b)),
                _ => panic!("{fun:?}"),
            },
            expr => panic!("{expr:?}"),
        })
    }

    #[test]
    fn float_epilogue_matches_ordered_cpu_blending() {
        use BlendOp::*;
        use Factor::*;
        let src = [0.5, 2.0, -3.0, 0.5];
        let src1 = [0.25, 2.0, 0.5, 0.125];
        let dst = [4.0, -0.5, 8.0, 0.75];
        let k = [0.0, 0.25, 2.0, 0.5];
        for (rgb, alpha, expected) in [
            (
                component(One, Src1, Add),
                component(Constant, OneMinusSrc1Alpha, ReverseSubtract),
                [1.5, 1.0, 1.0, 0.40625],
            ),
            (
                component(SrcAlphaSaturated, OneMinusConstant, Subtract),
                component(SrcAlphaSaturated, Zero, Add),
                [-3.875, 0.875, 7.25, 0.5],
            ),
            (
                component(Zero, Zero, Min),
                component(Zero, Zero, Max),
                [0.5, -0.5, -3.0, 0.75],
            ),
        ] {
            let mut module = naga::front::wgsl::parse_str(SOURCE).unwrap();
            let colors = [ColorTarget {
                format: crate::Format::Rgba32Float,
                blend: Some((rgb, alpha)),
                write_mask: 15,
            }];
            specialize(
                &mut module,
                "main",
                AttachmentBlend {
                    colors: &colors,
                    slots: 1,
                    reads: 1,
                    alpha_to_coverage: true,
                    set: 1,
                    push_offset: 0,
                },
                None,
            );
            let info = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            let function = &module.entry_points[0].function;
            let naga::Statement::Return { value: Some(value) } = function.body.last().unwrap()
            else {
                panic!("missing output")
            };
            let result = evaluate(&function.expressions, *value, src, src1, dst, k);
            assert_eq!(
                [0, 1, 2, 3].map(|lane| result.at(0).at(lane).scalar()),
                expected
            );
            assert_eq!(result.at(2).scalar(), 0.0);
            let mut words = naga::back::spv::write_vec(
                &module,
                &info,
                &naga::back::spv::Options {
                    lang_version: (1, 3),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
            super::super::compile::legalize(&mut words, true);
            input_attachments(&mut words, 1);
            assert!(super::super::compile::instructions(&words).any(|i| i[0] & 0xffff == 98));
        }
    }
}
