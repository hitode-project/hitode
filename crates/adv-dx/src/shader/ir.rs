//! Register IR shared by the shader back ends. Registers hold raw bits; each
//! instruction specifies how to interpret those bits.

pub mod interpreter;
pub(crate) mod opcode;
pub mod passes;
mod verify;

pub use opcode::Opcode;
pub use verify::verify;

pub type Vector = [u32; 4];

/// D3D11_COMMONSHADER_FLOWCONTROL_NESTING_LIMIT also applies to SM4.
pub(crate) const MAX_CONTROL_FLOW_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
}

impl Stage {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Pixel => "ps",
            Self::Vertex => "vs",
            Self::Geometry => "gs",
            Self::Hull => "hs",
            Self::Domain => "ds",
            Self::Compute => "cs",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    Sm40,
    Sm41,
    Sm50,
    Level91,
    Level93,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    Bits,
    Float,
    Int,
    Uint,
    Double,
    Binding,
    Opaque,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Register {
    Temp,
    Input,
    Output,
    IndexableTemp,
    Sampler,
    Resource,
    ConstantBuffer,
    ImmediateConstantBuffer,
    Label,
    InputPrimitiveId,
    OutputDepth,
    Null,
    Rasterizer,
    OutputCoverageMask,
    Stream,
    FunctionBody,
    FunctionTable,
    Interface,
    FunctionInput,
    FunctionOutput,
    OutputControlPointId,
    InputForkInstanceId,
    InputJoinInstanceId,
    InputControlPoint,
    OutputControlPoint,
    InputPatchConstant,
    InputDomainPoint,
    ThisPointer,
    UnorderedAccessView,
    ThreadGroupSharedMemory,
    InputThreadId,
    InputThreadGroupId,
    InputThreadIdInGroup,
    InputCoverageMask,
    InputThreadIdInGroupFlattened,
    InputGsInstanceId,
    OutputDepthGreaterEqual,
    OutputDepthLessEqual,
    CycleCounter,
    OutputStencilRef,
    InnerCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Index {
    Direct(u64),
    Relative { base: u64, register: Box<Operand> },
}

impl Index {
    pub fn direct(&self) -> Option<u32> {
        match self {
            Self::Direct(value) => u32::try_from(*value).ok(),
            Self::Relative { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Components {
    None,
    Scalar,
    Mask(u8),
    Swizzle([u8; 4]),
    Select(u8),
}

impl Components {
    pub fn mask(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Scalar => 1,
            Self::Mask(mask) => mask,
            Self::Swizzle(lanes) => lanes.into_iter().fold(0, |mask, lane| mask | 1 << lane),
            Self::Select(lane) => 1 << lane,
        }
    }

    pub fn swizzle(self) -> [u8; 4] {
        match self {
            Self::Scalar => [0; 4],
            Self::Select(lane) => [lane; 4],
            Self::Swizzle(lanes) => lanes,
            Self::Mask(_) | Self::None => [0, 1, 2, 3],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Modifier {
    #[default]
    None,
    Negate,
    Absolute,
    AbsoluteNegate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Precision {
    #[default]
    Default,
    Float16,
    Float2_8,
    Int16,
    Uint16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperandValue {
    Register {
        register: Register,
        indices: Vec<Index>,
    },
    /// The second index is a byte address, with a stride of 16 for relative indices.
    Constant {
        slot: Index,
        byte_offset: Index,
    },
    Immediate(Vector),
    Immediate64([u64; 4]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operand {
    pub value: OperandValue,
    pub components: Components,
    pub modifier: Modifier,
    pub precision: Precision,
    pub non_uniform: bool,
}

impl Operand {
    pub fn register(&self) -> Option<(Register, &[Index])> {
        match &self.value {
            OperandValue::Register { register, indices } => Some((*register, indices)),
            OperandValue::Constant { .. }
            | OperandValue::Immediate(_)
            | OperandValue::Immediate64(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceDimension {
    Buffer,
    Texture1D,
    Texture2D,
    Texture2DMs,
    Texture3D,
    TextureCube,
    Texture1DArray,
    Texture2DArray,
    Texture2DMsArray,
    TextureCubeArray,
    RawBuffer,
    StructuredBuffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceType {
    Unorm,
    Snorm,
    Int,
    Uint,
    Float,
    Mixed,
    Double,
    Continued,
    Unused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplerMode {
    Default,
    Comparison,
    Mono,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    ConstantBuffer {
        bytes: u32,
        dynamic_indexing: bool,
    },
    Sampler(SamplerMode),
    Typed {
        dimension: ResourceDimension,
        samples: u32,
        returns: [ResourceType; 4],
    },
    Raw {
        bytes: u32,
    },
    Structured {
        stride: u32,
        elements: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub register: Register,
    pub slot: u32,
    pub kind: BindingKind,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemValue {
    None,
    Position,
    ClipDistance,
    CullDistance,
    RenderTargetArrayIndex,
    ViewportArrayIndex,
    VertexId,
    PrimitiveId,
    InstanceId,
    FrontFace,
    SampleIndex,
    TessFactor,
    InsideTessFactor,
    Target,
    Depth,
    Coverage,
    DepthGreaterEqual,
    DepthLessEqual,
    StencilRef,
    InnerCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureElement {
    pub semantic: String,
    pub semantic_index: u32,
    pub system_value: SystemValue,
    pub value_type: ValueType,
    pub register: u32,
    pub mask: u8,
    pub used_mask: u8,
    pub stream: u32,
    pub min_precision: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signatures {
    pub input: Vec<SignatureElement>,
    pub output: Vec<SignatureElement>,
    pub patch_constant: Vec<SignatureElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Undefined,
    Flat,
    Perspective,
    PerspectiveCentroid,
    Linear,
    LinearCentroid,
    PerspectiveSample,
    LinearSample,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub opcode: Opcode,
    pub operand: Option<Operand>,
    pub controls: u32,
    pub data: Vec<u32>,
    pub binding: Option<Binding>,
    pub interpolation: Option<Interpolation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceOperands {
    pub offset: Option<[i32; 3]>,
    pub dimension: Option<(ResourceDimension, u32)>,
    pub returns: Option<[ResourceType; 4]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub opcode: Opcode,
    pub destinations: Vec<Operand>,
    pub sources: Vec<Operand>,
    pub saturate: bool,
    pub test_nonzero: bool,
    pub precise: u8,
    pub controls: u32,
    pub resource: ResourceOperands,
    pub interface_function: Option<u32>,
    /// Semantic pass inserts sign-preserving flushing on these FP32 values.
    pub flush_inputs: bool,
    pub flush_outputs: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Statement {
    Declare(Declaration),
    Execute(Instruction),
    If {
        test: Instruction,
        accept: Block,
        reject: Option<Block>,
    },
    Loop(Block),
    Switch {
        selector: Operand,
        cases: Vec<Case>,
    },
    ImmediateConstants(Vec<Vector>),
    CustomData {
        class: u32,
        words: Vec<u32>,
    },
}

pub type Block = Vec<Statement>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub value: Option<u32>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub stage: Stage,
    pub model: Model,
    pub signatures: Signatures,
    pub body: Block,
    pub feature_flags: u64,
    pub statistics: Vec<u32>,
    pub reflection: Option<ReflectionData>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionData {
    pub compiler: String,
    pub version: u32,
    pub flags: u32,
    pub buffers: Vec<BufferDescription>,
    pub resources: Vec<ResourceDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferDescription {
    pub name: String,
    pub bytes: u32,
    pub flags: u32,
    pub kind: u32,
    pub variables: Vec<VariableDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableDescription {
    pub name: String,
    pub byte_offset: u32,
    pub bytes: u32,
    pub flags: u32,
    pub ty: VariableType,
    pub default: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableType {
    pub class: u16,
    pub scalar_type: u16,
    pub rows: u16,
    pub columns: u16,
    pub elements: u16,
    pub name: Option<String>,
    pub members: Vec<(String, u32, Self)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDescription {
    pub name: String,
    pub kind: u32,
    pub value_type: u32,
    pub dimension: u32,
    pub samples: u32,
    pub slot: u32,
    pub count: u32,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    DoublePrecision,
    MissingStorageFormat(u32),
    UnsupportedStage(Stage),
    UnsupportedOpcode(Opcode),
    UnsupportedRegister(Register),
    MissingInput(Register, u32),
    ExecutionLimit,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}
