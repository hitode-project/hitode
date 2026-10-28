use super::*;
use std::hash::{Hash, Hasher};
use wgpu::naga::{self, back::msl};
pub(in super::super) type Pipeline = (
    Object<dyn MTLRenderPipelineState>,
    Object<dyn MTLDepthStencilState>,
);
#[derive(Default)]
pub(super) struct Cache {
    pipelines: HashMap<u64, Pipeline>,
    order: VecDeque<u64>,
    samplers: HashMap<Sampler, Object<dyn MTLSamplerState>>,
}
/// Prebuild and first draw share the same device and shader-function cache.
#[derive(Clone)]
pub(in super::super) struct Compiler(Arc<Mutex<Builder>>);
type ShaderFunctions = (std::sync::Weak<str>, [Object<dyn MTLFunction>; 2]);
type DepthKey = (Option<Compare>, bool, Option<Stencil>);

struct Builder {
    device: Object<dyn MTLDevice>,
    functions: HashMap<u64, ShaderFunctions>,
    depth: HashMap<DepthKey, Object<dyn MTLDepthStencilState>>,
}
impl Compiler {
    pub(super) fn new(device: Object<dyn MTLDevice>) -> Self {
        Self(Arc::new(Mutex::new(Builder {
            device,
            functions: HashMap::new(),
            depth: HashMap::new(),
        })))
    }
    pub(in super::super) fn build(
        &self,
        key: &program::Key,
        program: &Program,
    ) -> Result<Pipeline, Error> {
        let mut builder = self.0.lock().unwrap();
        let device = builder.device.clone();
        objc2::rc::autoreleasepool(|_| builder.build(&device, key, program))
    }
}
fn hash(value: &impl Hash) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hash);
    hash.finish()
}
fn error(error: impl std::fmt::Display) -> Error {
    Error::Device(error.to_string())
}
fn version() -> (u8, u8) {
    // Keep the OS selection identical to wgpu-hal 30's Metal adapter.
    if objc2::available!(macos = 26.0, ios = 26.0, tvos = 26.0, visionos = 26.0) {
        (4, 0)
    } else if objc2::available!(macos = 15.0, ios = 18.0, tvos = 18.0, visionos = 2.0) {
        (3, 2)
    } else if objc2::available!(macos = 14.0, ios = 17.0, tvos = 17.0, visionos = 1.0) {
        (3, 1)
    } else if objc2::available!(macos = 13.0, ios = 16.0, tvos = 16.0, visionos = 1.0) {
        (3, 0)
    } else if objc2::available!(macos = 12.0, ios = 15.0, tvos = 15.0, visionos = 1.0) {
        (2, 4)
    } else if objc2::available!(macos = 11.0, ios = 14.0, tvos = 14.0, visionos = 1.0) {
        (2, 3)
    } else if objc2::available!(macos = 10.15, ios = 13.0, tvos = 13.0, visionos = 1.0) {
        (2, 2)
    } else if objc2::available!(macos = 10.14, ios = 12.0, tvos = 12.0, visionos = 1.0) {
        (2, 1)
    } else if objc2::available!(macos = 10.13, ios = 11.0, tvos = 11.0, visionos = 1.0) {
        (2, 0)
    } else if objc2::available!(macos = 10.12, ios = 10.0, tvos = 10.0, visionos = 1.0) {
        (1, 2)
    } else {
        (1, 1)
    }
}
fn format(format: AttributeFormat) -> naga_types::VertexFormat {
    use naga_types::VertexFormat as F;
    match format {
        AttributeFormat::Float1 => F::Float32,
        AttributeFormat::Float2 => F::Float32x2,
        AttributeFormat::Float3 => F::Float32x3,
        AttributeFormat::Float4 => F::Float32x4,
        AttributeFormat::Bgra8Unorm | AttributeFormat::Unorm8x4 => F::Unorm8x4,
        AttributeFormat::Uint8x4 => F::Uint8x4,
        AttributeFormat::Sint16x2 => F::Sint16x2,
        AttributeFormat::Sint16x4 => F::Sint16x4,
        AttributeFormat::Snorm16x2 => F::Snorm16x2,
        AttributeFormat::Snorm16x4 => F::Snorm16x4,
        AttributeFormat::Unorm16x2 => F::Unorm16x2,
        AttributeFormat::Unorm16x4 => F::Unorm16x4,
        AttributeFormat::Uint10x3 | AttributeFormat::Snorm10x3 => F::Uint32,
        AttributeFormat::Float16x2 => F::Float16x2,
        AttributeFormat::Float16x4 => F::Float16x4,
    }
}
impl Builder {
    fn functions(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        key: &program::Key,
        program: &Program,
    ) -> Result<[Object<dyn MTLFunction>; 2], Error> {
        let id = hash(&(
            &program.source,
            &program.textures,
            &key.streams,
            key.raster.topology == Topology::Points,
        ));
        if let Some((_, functions)) = self.functions.get(&id) {
            return Ok(functions.clone());
        }
        program::validate_program(program)?;
        let module = naga::front::wgsl::parse_str(&program.source).map_err(error)?;
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .map_err(error)?;
        let mut resources = msl::EntryPointResources {
            sizes_buffer: Some(3),
            ..Default::default()
        };
        for binding in 0..3 {
            resources.resources.insert(
                naga::ResourceBinding { group: 0, binding },
                msl::BindTarget {
                    buffer: Some(binding as u8),
                    ..Default::default()
                },
            );
        }
        for n in 0..program.textures.len() {
            let slot = program.textures[..n]
                .iter()
                .filter(|&&vertex| vertex == program.textures[n])
                .count() as u8;
            resources.resources.insert(
                naga::ResourceBinding {
                    group: 1,
                    binding: n as u32 * 2,
                },
                msl::BindTarget {
                    texture: Some(slot),
                    ..Default::default()
                },
            );
            resources.resources.insert(
                naga::ResourceBinding {
                    group: 1,
                    binding: n as u32 * 2 + 1,
                },
                msl::BindTarget {
                    sampler: Some(msl::BindSamplerTarget::Resource(slot)),
                    ..Default::default()
                },
            );
        }
        let lang_version = version();
        let options = msl::Options {
            lang_version,
            fake_missing_bindings: false,
            per_entry_point_map: [
                ("vertex_main".into(), resources.clone()),
                ("fragment_main".into(), resources),
            ]
            .into(),
            bounds_check_policies: naga::proc::BoundsCheckPolicies {
                index: naga::proc::BoundsCheckPolicy::Restrict,
                buffer: naga::proc::BoundsCheckPolicy::Restrict,
                image_load: naga::proc::BoundsCheckPolicy::Restrict,
                binding_array: naga::proc::BoundsCheckPolicy::Unchecked,
            },
            ..Default::default()
        };
        let mappings: Vec<_> = key
            .streams
            .iter()
            .enumerate()
            .map(
                |(n, (stride, attributes, instance))| msl::VertexBufferMapping {
                    id: 30 - n as u32,
                    stride: if *stride == 0 {
                        attributes
                            .iter()
                            .map(|a| a.offset + a.format.size())
                            .max()
                            .unwrap()
                    } else {
                        *stride
                    },
                    step_mode: if *stride == 0 {
                        msl::VertexBufferStepMode::Constant
                    } else if *instance {
                        msl::VertexBufferStepMode::ByInstance
                    } else {
                        msl::VertexBufferStepMode::ByVertex
                    },
                    attributes: attributes
                        .iter()
                        .map(|a| msl::AttributeMapping {
                            shader_location: a.location,
                            offset: a.offset,
                            format: format(a.format),
                        })
                        .collect(),
                },
            )
            .collect();
        let mut functions = Vec::new();
        for (stage, name) in [
            (naga::ShaderStage::Vertex, "vertex_main"),
            (naga::ShaderStage::Fragment, "fragment_main"),
        ] {
            let (module, info) = naga::back::pipeline_constants::process_overrides(
                &module,
                &info,
                Some((stage, name)),
                &Default::default(),
            )
            .map_err(error)?;
            let pipeline_options = msl::PipelineOptions {
                entry_point: Some((stage, name.into())),
                allow_and_force_point_size: key.raster.topology == Topology::Points,
                vertex_pulling_transform: true,
                vertex_buffer_mappings: if stage == naga::ShaderStage::Vertex {
                    mappings.clone()
                } else {
                    vec![]
                },
                ..Default::default()
            };
            let (source, translation) =
                msl::write_string(&module, &info, &options, &pipeline_options).map_err(error)?;
            let entry = translation.entry_point_names[0].as_ref().map_err(error)?;
            let compile = MTLCompileOptions::new();
            compile.setLanguageVersion(MTLLanguageVersion(
                ((lang_version.0 as usize) << 16) | (lang_version.1 as usize),
            ));
            if objc2::available!(macos = 11.0, ios = 13.0, tvos = 14.0, visionos = 1.0) {
                compile.setPreserveInvariance(true);
            }
            let library = device
                .newLibraryWithSource_options_error(&NSString::from_str(&source), Some(&compile))
                .map_err(error)?;
            functions.push(
                library
                    .newFunctionWithName(&NSString::from_str(entry))
                    .unwrap(),
            );
        }
        let functions: [_; 2] = functions.try_into().unwrap();
        self.functions
            .retain(|_, (source, _)| source.strong_count() != 0);
        self.functions
            .insert(id, (Arc::downgrade(&program.source), functions.clone()));
        Ok(functions)
    }
    fn build(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        key: &program::Key,
        program: &Program,
    ) -> Result<Pipeline, Error> {
        let functions = self.functions(device, key, program)?;
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(Some(&functions[0]));
        descriptor.setFragmentFunction(Some(&functions[1]));
        unsafe {
            descriptor.setInputPrimitiveTopology(match key.raster.topology {
                Topology::Points => MTLPrimitiveTopologyClass::Point,
                Topology::Lines | Topology::LineStrip => MTLPrimitiveTopologyClass::Line,
                _ => MTLPrimitiveTopologyClass::Triangle,
            });
        }
        let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
        color.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let mask = key.raster.color_write;
        color.setWriteMask(MTLColorWriteMask(
            ((mask & 1) << 3 | (mask & 2) << 1 | (mask & 4) >> 1 | (mask & 8) >> 3) as usize,
        ));
        if let Some(blend) = key.raster.color_blend {
            color.setBlendingEnabled(true);
            color.setSourceRGBBlendFactor(factor(blend.source));
            color.setDestinationRGBBlendFactor(factor(blend.destination));
            color.setRgbBlendOperation(operation(blend.operation));
            let alpha = key.raster.alpha_blend.unwrap_or(blend);
            color.setSourceAlphaBlendFactor(factor(alpha.source));
            color.setDestinationAlphaBlendFactor(factor(alpha.destination));
            color.setAlphaBlendOperation(operation(alpha.operation));
        }
        if let Some(depth) = key.raster.depth_format {
            let format = match depth {
                DepthFormat::Float32 => MTLPixelFormat::Depth32Float,
                DepthFormat::Unorm16 => MTLPixelFormat::Depth16Unorm,
                DepthFormat::Depth24Stencil8 => {
                    #[cfg(target_os = "macos")]
                    if device.isDepth24Stencil8PixelFormatSupported() {
                        MTLPixelFormat::Depth24Unorm_Stencil8
                    } else {
                        MTLPixelFormat::Depth32Float_Stencil8
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        MTLPixelFormat::Depth32Float_Stencil8
                    }
                }
            };
            descriptor.setDepthAttachmentPixelFormat(format);
            if depth.has_stencil() {
                descriptor.setStencilAttachmentPixelFormat(format);
            }
        }
        let depth_key = (key.raster.depth, key.raster.depth_write, key.raster.stencil);
        let depth = self
            .depth
            .entry(depth_key)
            .or_insert_with(|| {
                let descriptor = MTLDepthStencilDescriptor::new();
                descriptor
                    .setDepthCompareFunction(compare(key.raster.depth.unwrap_or(Compare::Always)));
                descriptor
                    .setDepthWriteEnabled(key.raster.depth_write && key.raster.depth.is_some());
                if let Some(stencil) = key.raster.stencil {
                    for (face, front) in [(stencil.front, true), (stencil.back, false)] {
                        let descriptor_face = MTLStencilDescriptor::new();
                        descriptor_face.setStencilCompareFunction(compare(face.compare));
                        descriptor_face.setStencilFailureOperation(stencil_op(face.fail));
                        descriptor_face.setDepthFailureOperation(stencil_op(face.depth_fail));
                        descriptor_face.setDepthStencilPassOperation(stencil_op(face.pass));
                        descriptor_face.setReadMask(stencil.read_mask);
                        descriptor_face.setWriteMask(stencil.write_mask);
                        if front {
                            descriptor.setFrontFaceStencil(Some(&descriptor_face));
                        } else {
                            descriptor.setBackFaceStencil(Some(&descriptor_face));
                        }
                    }
                }
                device
                    .newDepthStencilStateWithDescriptor(&descriptor)
                    .unwrap()
            })
            .clone();
        let pipeline = (
            device
                .newRenderPipelineStateWithDescriptor_error(&descriptor)
                .map_err(error)?,
            depth,
        );
        Ok(pipeline)
    }
}
impl Cache {
    pub(super) fn get(
        &mut self,
        gpu: &mut Gpu,
        compiler: &Compiler,
        key: &program::Key,
        program: &Program,
    ) -> Result<Pipeline, Error> {
        let id = hash(key);
        if let Some(pipeline) = self.pipelines.get(&id) {
            gpu.stats.pipeline_cache_hits += 1;
            return Ok(pipeline.clone());
        }
        gpu.programmable
            .warm
            .metal
            .get_or_insert_with(|| compiler.clone());
        let pipeline = gpu
            .programmable
            .warm
            .pipeline(
                recipes::Recipe::program(key, program),
                &gpu.context,
                &gpu.uniform,
                &gpu.shader,
                &gpu.layout,
                None,
                Some(program),
            )?
            .metal()?;
        if self.order.len() == MAX_PIPELINES {
            self.pipelines.remove(&self.order.pop_front().unwrap());
        }
        self.order.push_back(id);
        self.pipelines.insert(id, pipeline.clone());
        Ok(pipeline)
    }
    pub(super) fn sampler(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        mut sampler: Sampler,
    ) -> Object<dyn MTLSamplerState> {
        sampler.border_color = 0;
        self.samplers
            .entry(sampler)
            .or_insert_with(|| {
                let d = MTLSamplerDescriptor::new();
                d.setSAddressMode(address(sampler.u));
                d.setTAddressMode(address(sampler.v));
                d.setMinFilter(filter(sampler.min));
                d.setMagFilter(filter(sampler.mag));
                d.setMipFilter(if sampler.mip == Some(Filter::Linear) {
                    MTLSamplerMipFilter::Linear
                } else {
                    MTLSamplerMipFilter::Nearest
                });
                d.setLodMinClamp(sampler.first_level as f32);
                d.setLodMaxClamp(if sampler.mip.is_none() {
                    sampler.first_level as f32
                } else {
                    32.
                });
                device.newSamplerStateWithDescriptor(&d).unwrap()
            })
            .clone()
    }
}
fn compare(value: Compare) -> MTLCompareFunction {
    match value {
        Compare::Never => MTLCompareFunction::Never,
        Compare::Less => MTLCompareFunction::Less,
        Compare::Equal => MTLCompareFunction::Equal,
        Compare::LessEqual => MTLCompareFunction::LessEqual,
        Compare::Greater => MTLCompareFunction::Greater,
        Compare::NotEqual => MTLCompareFunction::NotEqual,
        Compare::GreaterEqual => MTLCompareFunction::GreaterEqual,
        Compare::Always => MTLCompareFunction::Always,
    }
}
fn stencil_op(value: StencilOp) -> MTLStencilOperation {
    match value {
        StencilOp::Keep => MTLStencilOperation::Keep,
        StencilOp::Zero => MTLStencilOperation::Zero,
        StencilOp::Replace => MTLStencilOperation::Replace,
        StencilOp::IncrementClamp => MTLStencilOperation::IncrementClamp,
        StencilOp::DecrementClamp => MTLStencilOperation::DecrementClamp,
        StencilOp::Invert => MTLStencilOperation::Invert,
        StencilOp::IncrementWrap => MTLStencilOperation::IncrementWrap,
        StencilOp::DecrementWrap => MTLStencilOperation::DecrementWrap,
    }
}
fn factor(value: Factor) -> MTLBlendFactor {
    match value {
        Factor::Zero => MTLBlendFactor::Zero,
        Factor::One => MTLBlendFactor::One,
        Factor::SourceColor => MTLBlendFactor::SourceColor,
        Factor::OneMinusSourceColor => MTLBlendFactor::OneMinusSourceColor,
        Factor::SourceAlpha => MTLBlendFactor::SourceAlpha,
        Factor::OneMinusSourceAlpha => MTLBlendFactor::OneMinusSourceAlpha,
        Factor::DestinationColor => MTLBlendFactor::DestinationColor,
        Factor::OneMinusDestinationColor => MTLBlendFactor::OneMinusDestinationColor,
        Factor::DestinationAlpha => MTLBlendFactor::DestinationAlpha,
        Factor::OneMinusDestinationAlpha => MTLBlendFactor::OneMinusDestinationAlpha,
        Factor::SourceAlphaSaturated => MTLBlendFactor::SourceAlphaSaturated,
    }
}
fn operation(value: BlendOp) -> MTLBlendOperation {
    match value {
        BlendOp::Add => MTLBlendOperation::Add,
        BlendOp::Subtract => MTLBlendOperation::Subtract,
        BlendOp::ReverseSubtract => MTLBlendOperation::ReverseSubtract,
        BlendOp::Minimum => MTLBlendOperation::Min,
        BlendOp::Maximum => MTLBlendOperation::Max,
    }
}
fn address(value: Address) -> MTLSamplerAddressMode {
    match value {
        Address::Clamp | Address::Border => MTLSamplerAddressMode::ClampToEdge,
        Address::Repeat => MTLSamplerAddressMode::Repeat,
        Address::Mirror => MTLSamplerAddressMode::MirrorRepeat,
    }
}
fn filter(value: Filter) -> MTLSamplerMinMagFilter {
    match value {
        Filter::Nearest => MTLSamplerMinMagFilter::Nearest,
        Filter::Linear => MTLSamplerMinMagFilter::Linear,
    }
}
