//! Guest-independent programmable drawing. Vertex storage is uploaded unchanged;
//! the GPU vertex fetcher and a small unpack expression produce shader inputs.
use super::{Image, Sampler};
use std::sync::Arc;
use wgpu::naga::{Binding, BuiltIn, TypeInner};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttributeFormat {
    Float1,
    Float2,
    Float3,
    Float4,
    Bgra8Unorm,
    Uint8x4,
    Sint16x2,
    Sint16x4,
    Unorm8x4,
    Snorm16x2,
    Snorm16x4,
    Unorm16x2,
    Unorm16x4,
    Uint10x3,
    Snorm10x3,
    Float16x2,
    Float16x4,
}
impl AttributeFormat {
    pub fn size(self) -> u32 {
        match self {
            Self::Float1
            | Self::Bgra8Unorm
            | Self::Uint8x4
            | Self::Sint16x2
            | Self::Unorm8x4
            | Self::Snorm16x2
            | Self::Unorm16x2
            | Self::Uint10x3
            | Self::Snorm10x3
            | Self::Float16x2 => 4,
            Self::Float2 | Self::Sint16x4 | Self::Snorm16x4 | Self::Unorm16x4 | Self::Float16x4 => {
                8
            }
            Self::Float3 => 12,
            Self::Float4 => 16,
        }
    }
    pub(crate) fn native(self) -> wgpu::VertexFormat {
        use wgpu::VertexFormat as F;
        match self {
            Self::Float1 => F::Float32,
            Self::Float2 => F::Float32x2,
            Self::Float3 => F::Float32x3,
            Self::Float4 => F::Float32x4,
            Self::Bgra8Unorm | Self::Unorm8x4 => F::Unorm8x4,
            Self::Uint8x4 => F::Uint8x4,
            Self::Sint16x2 => F::Sint16x2,
            Self::Sint16x4 => F::Sint16x4,
            Self::Snorm16x2 => F::Snorm16x2,
            Self::Snorm16x4 => F::Snorm16x4,
            Self::Unorm16x2 => F::Unorm16x2,
            Self::Unorm16x4 => F::Unorm16x4,
            Self::Uint10x3 | Self::Snorm10x3 => F::Uint32,
            Self::Float16x2 => F::Float16x2,
            Self::Float16x4 => F::Float16x4,
        }
    }
    pub fn wgsl_type(self) -> &'static str {
        match self {
            Self::Float1 => "f32",
            Self::Float2 | Self::Snorm16x2 | Self::Unorm16x2 | Self::Float16x2 => "vec2<f32>",
            Self::Float3 => "vec3<f32>",
            Self::Uint8x4 => "vec4<u32>",
            Self::Sint16x2 => "vec2<i32>",
            Self::Sint16x4 => "vec4<i32>",
            Self::Uint10x3 | Self::Snorm10x3 => "u32",
            _ => "vec4<f32>",
        }
    }
    pub fn unpack(self, value: &str) -> String {
        match self {
            Self::Float1 => format!("vec4<f32>({value},0.0,0.0,1.0)"),
            Self::Float2 | Self::Snorm16x2 | Self::Unorm16x2 | Self::Float16x2 => {
                format!("vec4<f32>({value},0.0,1.0)")
            }
            Self::Float3 => format!("vec4<f32>({value},1.0)"),
            Self::Bgra8Unorm => format!("{value}.bgra"),
            Self::Sint16x2 => format!("vec4<f32>(vec2<f32>({value}),0.0,1.0)"),
            Self::Sint16x4 | Self::Uint8x4 => format!("vec4<f32>({value})"),
            Self::Uint10x3 => format!(
                "vec4<f32>(vec3<f32>(vec3<u32>({value}&1023u,({value}>>10u)&1023u,({value}>>20u)&1023u)),1.0)"
            ),
            Self::Snorm10x3 => format!(
                "vec4<f32>(max(vec3<f32>(vec3<i32>(bitcast<i32>({value}<<22u)>>22,bitcast<i32>({value}<<12u)>>22,bitcast<i32>({value}<<2u)>>22))/511.0,vec3<f32>(-1.0)),1.0)"
            ),
            _ => value.to_owned(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Attribute {
    pub location: u32,
    pub offset: u32,
    pub format: AttributeFormat,
}
#[derive(Clone)]
pub struct VertexStream {
    pub bytes: Vec<u8>,
    pub stride: u32,
    pub attributes: Vec<Attribute>,
}
/// Four-vector packing keeps bool/int constants portable to uniform-buffer GL.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ConstantBlock {
    pub floats: [[f32; 4]; 256],
    pub integers: [[i32; 4]; 16],
    pub booleans: [[i32; 4]; 16],
    pub bump_matrix: [[f32; 4]; 8],
    pub bump_luminance: [[f32; 4]; 8],
}
impl Default for ConstantBlock {
    fn default() -> Self {
        bytemuck::Zeroable::zeroed()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Program {
    pub(super) id: u64,
    pub(super) writes_depth: bool,
    /// Complete validated WGSL with vertex_main and fragment_main entry points.
    pub(super) source: Arc<str>,
    /// (binding ordinal, vertex visibility); each image uses two group-1 slots.
    pub textures: Vec<bool>,
}
#[derive(Clone)]
pub struct ProgramTexture {
    pub image: Option<Image>,
    pub sampler: Sampler,
}
#[derive(Clone)]
pub struct ProgrammableDraw {
    pub program: Program,
    pub streams: Vec<VertexStream>,
    pub indices: Option<Vec<u32>>,
    pub vertex_count: u32,
    pub vertex_constants: ConstantBlock,
    pub pixel_constants: ConstantBlock,
    pub textures: Vec<ProgramTexture>,
    /// Viewport, fixed texture/alpha operations and alpha test, then reserved.
    pub parameters: [[u32; 4]; 40],
}

impl Program {
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn new(source: Arc<str>, textures: Vec<bool>) -> Self {
        static NEXT_PROGRAM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let writes_depth = wgpu::naga::front::wgsl::parse_str(&source).is_ok_and(|module| {
            let depth = Some(Binding::BuiltIn(BuiltIn::FragDepth));
            module.entry_points.iter().filter(|e| e.name == "fragment_main")
                .filter_map(|e| e.function.result.as_ref()).any(|result| {
                    result.binding == depth || matches!(&module.types[result.ty].inner,
                        TypeInner::Struct { members, .. } if members.iter().any(|m| m.binding == depth))
                })
        });
        Self {
            writes_depth,
            id: NEXT_PROGRAM.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            source,
            textures,
        }
    }
}
