use super::*;
use objc2_foundation::{NSArray, NSProcessInfo, NSURL};
use std::hash::{Hash, Hasher};

#[derive(Clone, PartialEq, Eq, Hash)]
struct ColorKey {
    format: MTLPixelFormat,
    blend: Option<(BlendComponent, BlendComponent)>,
    write_mask: u8,
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct RenderKey {
    vertex: Entry,
    fragment: Option<Entry>,
    colors: Vec<ColorKey>,
    depth: MTLPixelFormat,
    stencil: MTLPixelFormat,
    samples: u32,
    alpha_to_coverage: bool,
}
impl RenderKey {
    fn new(desc: &RenderPipelineDesc) -> Self {
        let depth = desc
            .depth_stencil
            .and_then(|d| caps::pixel_format(d.format))
            .unwrap_or(MTLPixelFormat::Invalid);
        Self {
            vertex: desc.vertex.clone(),
            fragment: desc.fragment.clone(),
            colors: desc
                .colors
                .iter()
                .map(|c| ColorKey {
                    format: caps::pixel_format(c.format).unwrap_or(MTLPixelFormat::Invalid),
                    blend: c.blend,
                    write_mask: c.write_mask,
                })
                .collect(),
            depth,
            stencil: if depth == MTLPixelFormat::Depth32Float_Stencil8 {
                depth
            } else {
                MTLPixelFormat::Invalid
            },
            samples: desc.samples,
            alpha_to_coverage: desc.alpha_to_coverage,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct DepthKey {
    compare: Compare,
    write: bool,
    stencil: Option<(StencilFace, StencilFace)>,
    read_mask: u8,
    write_mask: u8,
}
impl DepthKey {
    fn new(desc: Option<DepthStencil>) -> Self {
        desc.map_or(
            Self {
                compare: Compare::Always,
                write: false,
                stencil: None,
                read_mask: 0,
                write_mask: 0,
            },
            |d| Self {
                compare: d.depth_compare,
                write: d.depth_write,
                stencil: d.stencil,
                read_mask: if d.stencil.is_some() {
                    d.stencil_read_mask
                } else {
                    0
                },
                write_mask: if d.stencil.is_some() {
                    d.stencil_write_mask
                } else {
                    0
                },
            },
        )
    }
}
#[derive(Default)]
pub(super) struct Pipelines {
    render: HashMap<RenderKey, Object<dyn MTLRenderPipelineState>>,
    depth: HashMap<DepthKey, Object<dyn MTLDepthStencilState>>,
    compute: HashMap<Entry, Object<dyn MTLComputePipelineState>>,
}
#[derive(Clone)]
pub(crate) struct RenderPipeline(pub(super) Arc<RenderData>);
pub(super) struct RenderData {
    pub raw: Object<dyn MTLRenderPipelineState>,
    pub depth: Object<dyn MTLDepthStencilState>,
    pub raster: Raster,
    pub topology: MTLPrimitiveType,
    /// Vertex, then fragment.
    pub used: [shader::Used; 2],
    pub sizes: [Option<Box<[Option<u8>]>>; 2],
}
/// Encoder raster state a pipeline implies; passes emit only what changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Raster {
    pub cull: MTLCullMode,
    pub winding: MTLWinding,
    pub fill: MTLTriangleFillMode,
    pub clip: MTLDepthClipMode,
    pub depth_bias: bool,
}
impl Raster {
    /// A new render encoder's state.
    pub const DEFAULT: Self = Self {
        cull: MTLCullMode::None,
        winding: MTLWinding::Clockwise,
        fill: MTLTriangleFillMode::Fill,
        clip: MTLDepthClipMode::Clip,
        depth_bias: false,
    };
    fn new(p: Primitive, depth_bias: bool) -> Self {
        Self {
            cull: match p.cull {
                Cull::None => MTLCullMode::None,
                Cull::Front => MTLCullMode::Front,
                Cull::Back => MTLCullMode::Back,
            },
            winding: if p.front_ccw {
                MTLWinding::CounterClockwise
            } else {
                MTLWinding::Clockwise
            },
            fill: if p.wireframe {
                MTLTriangleFillMode::Lines
            } else {
                MTLTriangleFillMode::Fill
            },
            clip: if p.unclipped_depth {
                MTLDepthClipMode::Clamp
            } else {
                MTLDepthClipMode::Clip
            },
            depth_bias,
        }
    }
}
#[derive(Clone)]
pub(crate) struct ComputePipeline(pub(super) Arc<ComputeData>);
pub(super) struct ComputeData {
    pub raw: Object<dyn MTLComputePipelineState>,
    pub workgroup: MTLSize,
    pub used: shader::Used,
    pub sizes: Option<Box<[Option<u8>]>>,
}

impl Device {
    pub(crate) fn pipeline_identity(&self, desc: &RenderPipelineDesc) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        RenderKey::new(desc).hash(&mut hash);
        hash.finish()
    }
    pub(crate) fn create_render_pipeline(
        &self,
        desc: &RenderPipelineDesc,
        cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::RenderPipeline> {
        self.check()?;
        objc2::rc::autoreleasepool(|_| {
            let vertex = desc.vertex.shader.inner.function(&desc.vertex)?;
            let fragment = desc
                .fragment
                .as_ref()
                .map(|e| e.shader.inner.function(e))
                .transpose()?;
            if vertex.stage != Stage::Vertex || fragment.is_some_and(|f| f.stage != Stage::Fragment)
            {
                return Err(Error::Compile("render pipeline: wrong shader stage".into()));
            }
            let used = [
                self.used(&desc.bindings, Stages::VERTEX, vertex)?,
                fragment.map_or(Ok(shader::Used::default()), |f| {
                    self.used(&desc.bindings, Stages::FRAGMENT, f)
                })?,
            ];
            let key = RenderKey::new(desc);
            let d = MTLRenderPipelineDescriptor::new();
            d.setLabel(Some(&NSString::from_str(desc.label)));
            d.setVertexFunction(Some(&vertex.raw));
            d.setFragmentFunction(fragment.map(|f| &*f.raw));
            d.setRasterSampleCount(key.samples as usize);
            d.setAlphaToCoverageEnabled(key.alpha_to_coverage);
            d.setDepthAttachmentPixelFormat(key.depth);
            d.setStencilAttachmentPixelFormat(key.stencil);
            for (i, c) in key.colors.iter().enumerate() {
                let color = unsafe { d.colorAttachments().objectAtIndexedSubscript(i) };
                color.setPixelFormat(c.format);
                let mask = c.write_mask;
                color.setWriteMask(MTLColorWriteMask(
                    ((mask & 1) << 3 | (mask & 2) << 1 | (mask & 4) >> 1 | (mask & 8) >> 3)
                        as usize,
                ));
                if let Some((rgb, alpha)) = c.blend {
                    color.setBlendingEnabled(true);
                    color.setSourceRGBBlendFactor(factor(rgb.src));
                    color.setDestinationRGBBlendFactor(factor(rgb.dst));
                    color.setRgbBlendOperation(operation(rgb.op));
                    color.setSourceAlphaBlendFactor(factor(alpha.src));
                    color.setDestinationAlphaBlendFactor(factor(alpha.dst));
                    color.setAlphaBlendOperation(operation(alpha.op));
                }
            }
            // Archive access and deduplication are synchronized across compile callers.
            let mut archive = cache.map(|c| c.inner.0.lock().unwrap());
            let archive = archive.as_mut().and_then(|a| a.as_mut());
            if let Some(a) = &archive {
                d.setBinaryArchives(Some(&NSArray::from_slice(&[&*a.raw])));
                a.raw
                    .addRenderPipelineFunctionsWithDescriptor_error(&d)
                    .map_err(|e| shader::compile_error("render archive", e))?;
            }
            let mut pipelines = self.0.pipelines.lock().unwrap();
            let raw = if let Some(raw) = pipelines.render.get(&key) {
                raw.clone()
            } else {
                let raw = self
                    .0
                    .raw
                    .newRenderPipelineStateWithDescriptor_error(&d)
                    .map_err(|e| shader::compile_error("render pipeline", e))?;
                pipelines.render.insert(key, raw.clone());
                raw
            };
            let depth_key = DepthKey::new(desc.depth_stencil);
            let depth = if let Some(raw) = pipelines.depth.get(&depth_key) {
                raw.clone()
            } else {
                let d = MTLDepthStencilDescriptor::new();
                d.setDepthCompareFunction(resource::compare(depth_key.compare));
                d.setDepthWriteEnabled(depth_key.write);
                if let Some((front, back)) = depth_key.stencil {
                    for (front_face, face) in [(true, front), (false, back)] {
                        let s = MTLStencilDescriptor::new();
                        s.setStencilCompareFunction(resource::compare(face.compare));
                        s.setStencilFailureOperation(stencil_op(face.fail));
                        s.setDepthFailureOperation(stencil_op(face.depth_fail));
                        s.setDepthStencilPassOperation(stencil_op(face.pass));
                        s.setReadMask(u32::from(depth_key.read_mask));
                        s.setWriteMask(u32::from(depth_key.write_mask));
                        if front_face {
                            d.setFrontFaceStencil(Some(&s));
                        } else {
                            d.setBackFaceStencil(Some(&s));
                        }
                    }
                }
                let raw = self
                    .0
                    .raw
                    .newDepthStencilStateWithDescriptor(&d)
                    .ok_or(Error::OutOfMemory)?;
                pipelines.depth.insert(depth_key, raw.clone());
                raw
            };
            Ok(crate::RenderPipeline {
                inner: RenderPipeline(Arc::new(RenderData {
                    raw,
                    depth,
                    raster: Raster::new(
                        desc.primitive,
                        desc.depth_stencil.is_some_and(|d| d.depth_bias),
                    ),
                    topology: match desc.primitive.topology {
                        Topology::Points => MTLPrimitiveType::Point,
                        Topology::Lines => MTLPrimitiveType::Line,
                        Topology::LineStrip => MTLPrimitiveType::LineStrip,
                        Topology::Triangles => MTLPrimitiveType::Triangle,
                        Topology::TriangleStrip => MTLPrimitiveType::TriangleStrip,
                    },
                    used,
                    sizes: [vertex.sizes.clone(), fragment.and_then(|f| f.sizes.clone())],
                })),
            })
        })
    }
    pub(crate) fn create_compute_pipeline(
        &self,
        desc: &ComputePipelineDesc,
        cache: Option<&crate::pipeline::PipelineCache>,
    ) -> Result<crate::ComputePipeline> {
        self.check()?;
        objc2::rc::autoreleasepool(|_| {
            let entry = desc.entry.shader.inner.function(&desc.entry)?;
            if entry.stage != Stage::Compute {
                return Err(Error::Compile(
                    "compute pipeline: wrong shader stage".into(),
                ));
            }
            let used = self.used(&desc.bindings, Stages::COMPUTE, entry)?;
            let d = MTLComputePipelineDescriptor::new();
            d.setLabel(Some(&NSString::from_str(desc.label)));
            d.setComputeFunction(Some(&entry.raw));
            let mut archive = cache.map(|c| c.inner.0.lock().unwrap());
            let archive = archive.as_mut().and_then(|a| a.as_mut());
            if let Some(a) = &archive {
                d.setBinaryArchives(Some(&NSArray::from_slice(&[&*a.raw])));
                a.raw
                    .addComputePipelineFunctionsWithDescriptor_error(&d)
                    .map_err(|e| shader::compile_error("compute archive", e))?;
            }
            let mut pipelines = self.0.pipelines.lock().unwrap();
            let raw = if let Some(raw) = pipelines.compute.get(&desc.entry) {
                raw.clone()
            } else {
                let raw = self
                    .0
                    .raw
                    .newComputePipelineStateWithDescriptor_options_reflection_error(
                        &d,
                        MTLPipelineOption::empty(),
                        None,
                    )
                    .map_err(|e| shader::compile_error("compute pipeline", e))?;
                pipelines.compute.insert(desc.entry.clone(), raw.clone());
                raw
            };
            let [width, height, depth] = entry.workgroup.map(|n| n as usize);
            Ok(crate::ComputePipeline {
                inner: ComputePipeline(Arc::new(ComputeData {
                    raw,
                    workgroup: MTLSize {
                        width,
                        height,
                        depth,
                    },
                    used,
                    sizes: entry.sizes.clone(),
                })),
            })
        })
    }
    /// Native indices one stage reads; each must be declared in `bindings`.
    fn used(
        &self,
        bindings: &[Binding],
        stage: Stages,
        function: &shader::Function,
    ) -> Result<shader::Used> {
        let mut declared = shader::Used::default();
        for b in bindings.iter().filter(|b| b.stages.contains(stage)) {
            let (group, buffer) = shader::binding_group(b.kind);
            declared.add(group, b.slot, buffer, self.caps().limits.max_sampled_images)?;
        }
        let used = function.used.unwrap_or(declared);
        if used.samplers as u16 & (used.samplers >> 16) as u16 != 0 {
            return Err(Error::Invalid(format!(
                "{stage:?} sampler slots collide modulo 16"
            )));
        }
        if !declared.contains(used) {
            return Err(Error::Invalid(format!(
                "{stage:?} reads a binding the pipeline does not declare"
            )));
        }
        Ok(used)
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
        Factor::Src => MTLBlendFactor::SourceColor,
        Factor::OneMinusSrc => MTLBlendFactor::OneMinusSourceColor,
        Factor::SrcAlpha => MTLBlendFactor::SourceAlpha,
        Factor::OneMinusSrcAlpha => MTLBlendFactor::OneMinusSourceAlpha,
        Factor::Dst => MTLBlendFactor::DestinationColor,
        Factor::OneMinusDst => MTLBlendFactor::OneMinusDestinationColor,
        Factor::DstAlpha => MTLBlendFactor::DestinationAlpha,
        Factor::OneMinusDstAlpha => MTLBlendFactor::OneMinusDestinationAlpha,
        Factor::SrcAlphaSaturated => MTLBlendFactor::SourceAlphaSaturated,
        Factor::Constant => MTLBlendFactor::BlendColor,
        Factor::OneMinusConstant => MTLBlendFactor::OneMinusBlendColor,
    }
}
fn operation(value: BlendOp) -> MTLBlendOperation {
    match value {
        BlendOp::Add => MTLBlendOperation::Add,
        BlendOp::Subtract => MTLBlendOperation::Subtract,
        BlendOp::ReverseSubtract => MTLBlendOperation::ReverseSubtract,
        BlendOp::Min => MTLBlendOperation::Min,
        BlendOp::Max => MTLBlendOperation::Max,
    }
}

#[derive(Clone)]
pub(crate) struct PipelineCache(Arc<Mutex<Option<Archive>>>);
struct Archive {
    raw: Object<dyn MTLBinaryArchive>,
    header: Vec<u8>,
}
const MAGIC: &[u8; 8] = b"ADVMTL\0\x01";
fn header(device: &ProtocolObject<dyn MTLDevice>) -> Vec<u8> {
    let name = device.name().to_string();
    let os = NSProcessInfo::processInfo()
        .operatingSystemVersionString()
        .to_string();
    let mut data = MAGIC.to_vec();
    for value in [&name, &os] {
        data.extend_from_slice(&(value.len() as u32).to_le_bytes());
        data.extend_from_slice(value.as_bytes());
    }
    data
}
fn payload<'a>(data: &'a [u8], header: &[u8]) -> Option<&'a [u8]> {
    let tail = data.strip_prefix(header)?;
    let length = u64::from_le_bytes(tail.get(..8)?.try_into().ok()?);
    let bytes = tail.get(8..)?;
    (length == bytes.len() as u64).then_some(bytes)
}
impl PipelineCache {
    pub(crate) fn load(data: &[u8]) -> crate::pipeline::PipelineCache {
        let archive = objc2::rc::autoreleasepool(|_| {
            if !objc2::available!(macos = 11.0, ios = 14.0) {
                return None;
            }
            let device = MTLCreateSystemDefaultDevice()?;
            let header = header(&device);
            let desc = MTLBinaryArchiveDescriptor::new();
            let file = payload(data, &header).and_then(|bytes| {
                let file = TemporaryFile::new().ok()?;
                std::fs::write(&file.0, bytes).ok()?;
                desc.setUrl(Some(&file.url()));
                Some(file)
            });
            let raw = device
                .newBinaryArchiveWithDescriptor_error(&desc)
                .or_else(|_| {
                    // A malformed driver payload is an empty cache, like a bad header.
                    device.newBinaryArchiveWithDescriptor_error(&MTLBinaryArchiveDescriptor::new())
                })
                .ok()?;
            drop(file);
            Some(Archive { raw, header })
        });
        crate::pipeline::PipelineCache {
            inner: Self(Arc::new(Mutex::new(archive))),
        }
    }
    pub(crate) fn serialize(&self) -> Result<Vec<u8>> {
        let archive = self.0.lock().unwrap();
        let Some(archive) = &*archive else {
            return Ok(Vec::new());
        };
        let file = TemporaryFile::new()?;
        archive
            .raw
            .serializeToURL_error(&file.url())
            .map_err(|e| shader::compile_error("Metal archive", e))?;
        let bytes = std::fs::read(&file.0).map_err(io_error)?;
        let mut data = archive.header.clone();
        data.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        data.extend_from_slice(&bytes);
        Ok(data)
    }
}
pub(super) fn io_error(e: std::io::Error) -> Error {
    Error::Compile(format!("Metal file I/O: {}", e.kind()))
}
// Metal accepts file URLs for libraries and archives. Each operation owns and
// removes its temporary file; no caller cache location is retained here.
pub(super) struct TemporaryFile(pub std::path::PathBuf);
impl TemporaryFile {
    pub fn new() -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "adv-gpu-metal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error)?;
        Ok(Self(path))
    }
    pub fn url(&self) -> Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(self.0.to_str().unwrap()))
    }
}
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
// Archive mutation is protected by its mutex.
unsafe impl Send for Archive {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_payload_requires_the_exact_header_and_length() {
        let header = b"device and OS";
        let mut data = header.to_vec();
        data.extend_from_slice(&3u64.to_le_bytes());
        data.extend_from_slice(b"abc");
        assert_eq!(payload(&data, header), Some(&b"abc"[..]));
        assert!(payload(&data, b"another device").is_none());
        for end in 0..data.len() {
            assert!(payload(&data[..end], header).is_none());
        }
        data.push(0);
        assert!(payload(&data, header).is_none());
    }
}
