//! Bounded Direct3D 9 shader bytecode frontend and WGSL library translator.
//!
//! The translator is written here instead of linking vkd3d-shader or Wine. SM1-3
//! bytecode is regular, and its semantic details need differential tests either
//! way; vkd3d-shader is an LGPL C library that would need dynamic linking, an
//! Android cross-build and a SPIR-V to naga hop. Both remain semantic references.
//! naga validates the WGSL and stays on the same version as wgpu.
//!
//! No game bytecode is embedded here. The token layout follows Microsoft's
//! Instruction/Source/Destination Parameter Token and Relative Addressing specs.
//! Translation deliberately fails for semantics requiring unavailable host state.
mod cache;
pub mod interpreter;
mod parser;
#[cfg(test)]
mod tests;
mod translate;

pub const MAX_SHADER_TOKENS: usize = 65_536;
pub const MAX_EXECUTION_STEPS: usize = 65_536;
pub const MAX_FLOW_DEPTH: usize = 32;
pub const MAX_TRANSLATION_BYTES: usize = 4 * 1024 * 1024;
pub const TRANSLATOR_ABI: &str = "adv-d3d9-wgsl-6";
/// Read a guest token stream without mistaking immediate or comment data for END.
/// The callback's index is in DWORDs, and is always below MAX_SHADER_TOKENS.
pub fn read_bytecode<E: std::fmt::Display>(
    mut read_word: impl FnMut(usize) -> Result<u32, E>,
) -> Result<Vec<u32>, Error> {
    let header = read_word(0).map_err(|e| Error::new(0, e.to_string()))?;
    let version = Shader::parse(&[header, 0xffff])?.version;
    let mut words = vec![header];
    loop {
        let offset = words.len();
        if offset >= MAX_SHADER_TOKENS {
            return Err(Error::new(offset, "shader token limit exceeded"));
        }
        let token = read_word(offset).map_err(|e| Error::new(offset, e.to_string()))?;
        let length =
            instruction_length(version, token).map_err(|e| Error::new(offset, e.message))?;
        if offset
            .checked_add(length)
            .is_none_or(|n| n > MAX_SHADER_TOKENS)
        {
            return Err(Error::new(offset, "shader token limit exceeded"));
        }
        words.push(token);
        for index in offset + 1..offset + length {
            words.push(read_word(index).map_err(|e| Error::new(index, e.to_string()))?);
        }
        if token as u16 == 0xffff {
            return Ok(words);
        }
    }
}
/// Length used by bounded guest-memory readers. Full operand validation is done by parse.
pub fn instruction_length(version: Version, token: u32) -> Result<usize, Error> {
    match token as u16 {
        0xffff => Ok(1),
        0xfffe => Ok(1 + ((token >> 16) & 0x7fff) as usize),
        code => {
            let op = Opcode::decode(code).ok_or_else(|| Error::new(0, "unknown opcode"))?;
            if version.major >= 2 {
                Ok(1 + ((token >> 24) & 15) as usize)
            } else {
                let (d, s, i) = op.arity(version);
                Ok(1 + d + s + i)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Pixel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: String,
}
impl Error {
    pub(crate) fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "shader token {}: {}", self.offset, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegisterKind {
    Temp,
    Input,
    Float,
    Address,
    RasterOutput,
    ColorOutputLegacy,
    Output,
    Int,
    ColorOutput,
    DepthOutput,
    Sampler,
    Bool,
    Loop,
    Misc,
    Label,
    Predicate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Register {
    pub kind: RegisterKind,
    pub index: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relative {
    pub register: Register,
    pub component: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceModifier {
    None,
    Negate,
    Bias,
    BiasNegate,
    Sign,
    SignNegate,
    Complement,
    X2,
    X2Negate,
    DivideZ,
    DivideW,
    Abs,
    AbsNegate,
    Not,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub register: Register,
    pub swizzle: [u8; 4],
    pub modifier: SourceModifier,
    pub relative: Option<Relative>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub register: Register,
    pub mask: u8,
    pub saturate: bool,
    pub partial_precision: bool,
    pub centroid: bool,
    pub shift: i8,
    pub relative: Option<Relative>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SamplerDimension {
    D2,
    Cube,
    D3,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamplerBinding {
    pub index: u8,
    pub dimension: SamplerDimension,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub usage: u8,
    pub index: u8,
    pub register: Register,
    pub mask: u8,
    pub centroid: bool,
    pub sampler: Option<SamplerDimension>,
}

macro_rules! opcodes {
    ($($name:ident = $value:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum Opcode { $($name = $value),* }
        impl Opcode { pub(crate) fn decode(value:u16)->Option<Self> { match value { $($value=>Some(Self::$name),)* _=>None } } }
    }
}
opcodes! { Nop=0, Mov=1, Add=2, Sub=3, Mad=4, Mul=5, Rcp=6, Rsq=7, Dp3=8, Dp4=9, Min=10, Max=11, Slt=12, Sge=13, Exp=14, Log=15, Lit=16, Dst=17, Lrp=18, Frc=19, M4x4=20, M4x3=21, M3x4=22, M3x3=23, M3x2=24, Call=25, CallNz=26, Loop=27, Ret=28, EndLoop=29, Label=30, Dcl=31, Pow=32, Crs=33, Sgn=34, Abs=35, Nrm=36, SinCos=37, Rep=38, EndRep=39, If=40, Ifc=41, Else=42, EndIf=43, Break=44, Breakc=45, Mova=46, Defb=47, Defi=48, Texcoord=64, Texkill=65, Tex=66, Texbem=67, Texbeml=68, Texreg2ar=69, Texreg2gb=70, Texm3x2pad=71, Texm3x2tex=72, Texm3x3pad=73, Texm3x3tex=74, Texm3x3spec=76, Texm3x3vspec=77, Expp=78, Logp=79, Cnd=80, Def=81, Texreg2rgb=82, Texdp3tex=83, Texm3x2depth=84, Texdp3=85, Texm3x3=86, Texdepth=87, Cmp=88, Bem=89, Dp2add=90, Dsx=91, Dsy=92, Texldd=93, Setp=94, Texldl=95, Breakp=96, Phase=0xfffd }
impl Opcode {
    pub(crate) fn arity(self, version: Version) -> (usize, usize, usize) {
        use Opcode::*;
        match self {
            Nop | Ret | EndLoop | EndRep | Else | EndIf | Break | Phase => (0, 0, 0),
            Dcl => (1, 0, 1),
            Def => (1, 0, 4),
            Defi => (1, 0, 4),
            Defb => (1, 0, 1),
            Call | Label | Rep | If | Breakp => (0, 1, 0),
            CallNz | Loop | Ifc | Breakc => (0, 2, 0),
            Texkill | Texdepth => (1, 0, 0),
            Texcoord | Tex => {
                if version.major == 1 {
                    (1, usize::from(version.minor == 4), 0)
                } else if self == Tex {
                    (1, 2, 0)
                } else {
                    (1, 1, 0)
                }
            }
            Texldd => (1, 4, 0),
            Texldl => (1, 2, 0),
            Mad | Lrp | Cnd | Cmp | Dp2add => (1, 3, 0),
            Sgn | SinCos if version.major < 3 => (1, 3, 0),
            Mov | Rcp | Rsq | Exp | Log | Lit | Frc | Abs | Nrm | Mova | Expp | Logp | Dsx
            | Dsy | SinCos | Sgn => (1, 1, 0),
            Texm3x3spec => (1, 2, 0),
            Texbem | Texbeml | Texreg2ar | Texreg2gb | Texm3x2pad | Texm3x2tex | Texm3x3pad
            | Texm3x3tex | Texm3x3vspec | Texreg2rgb | Texdp3tex | Texm3x2depth | Texdp3
            | Texm3x3 => (1, 1, 0),
            _ => (1, 2, 0),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub offset: usize,
    pub opcode: Opcode,
    pub controls: u8,
    pub coissue: bool,
    pub destination: Option<Destination>,
    pub sources: Vec<Source>,
    pub predicate: Option<Source>,
    pub immediate: Vec<u32>,
}
#[derive(Clone, Debug)]
pub struct Shader {
    pub stage: Stage,
    pub version: Version,
    pub instructions: Vec<Instruction>,
    pub inputs: Vec<Declaration>,
    pub outputs: Vec<Declaration>,
    pub(crate) tokens: Vec<u32>,
    pub(crate) flow: Vec<Flow>,
}
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Flow {
    pub target: usize,
    pub end: usize,
}
#[derive(Clone, Debug)]
pub struct Translation {
    pub source: String,
    pub samplers: Vec<SamplerBinding>,
    pub writes_depth: bool,
    pub writes_point_size: bool,
}
impl Translation {
    /// Verify library code with declarations and stage entrypoint supplied as a test wrapper.
    /// The renderer additionally validates its real bindings and vertex linkage.
    pub fn validate(&self, stage: Stage) -> Result<(), Error> {
        let limit = if stage == Stage::Vertex { 4 } else { 16 };
        if self.samplers.iter().any(|s| s.index >= limit) {
            return Err(Error::new(0, "sampler index exceeds stage limit"));
        }
        let p = if stage == Stage::Vertex { "vs" } else { "ps" };
        let mut source = String::from(
            "diagnostic(off, derivative_uniformity);\nstruct ShaderInputs {v:array<vec4<f32>,16>,t:array<vec4<f32>,8>,position:vec4<f32>,front_face:bool};\nstruct ShaderOutputs {position:vec4<f32>,colors:array<vec4<f32>,2>,texcoords:array<vec4<f32>,8>,depth:f32,point_size:f32,fog:f32,generic:array<vec4<f32>,12>};\nstruct ShaderConstants {f:array<vec4<f32>,256>,i:array<vec4<i32>,16>,b:array<vec4<i32>,16>,bump_matrix:array<vec4<f32>,8>,bump_luminance:array<vec4<f32>,8>};\nvar<private> vs_constants:ShaderConstants; var<private> ps_constants:ShaderConstants;\n",
        );
        for s in &self.samplers {
            source.push_str(&format!("fn {p}_sample_{}(coord:vec4<f32>,mode:u32)->vec4<f32> {{return coord;}}\nfn {p}_sample_grad_{}(coord:vec4<f32>,dx:vec4<f32>,dy:vec4<f32>)->vec4<f32> {{return coord;}}\n", s.index, s.index));
        }
        source.push_str(&self.source);
        source.push_str(if stage == Stage::Vertex { "@vertex fn validation_main()->@builtin(position) vec4<f32> { var input:ShaderInputs; return vs_run(input).position; }" } else { "@fragment fn validation_main()->@location(0) vec4<f32> { var input:ShaderInputs; return ps_run(input).colors[0]; }" });
        Shader::validate_wgsl(&source)
    }
}
impl Shader {
    pub fn parse(tokens: &[u32]) -> Result<Self, Error> {
        parser::parse(tokens)
    }
    pub fn bytecode(&self) -> &[u32] {
        &self.tokens
    }
    pub fn translate(&self) -> Result<Translation, Error> {
        translate::translate(self)
    }
    /// Opt-in local cache; stores translated WGSL only, never original game bytecode.
    /// Cache I/O failures are non-fatal and fall back to fresh translation.
    pub fn translate_cached(&self, directory: &std::path::Path) -> Result<Translation, Error> {
        cache::translate_cached(self, directory)
    }
    pub fn disassemble(&self) -> String {
        parser::disassemble(self)
    }
    /// Validate a composed wrapper (including bindings and a stage entrypoint).
    pub fn validate_wgsl(source: &str) -> Result<(), Error> {
        let module = naga::front::wgsl::parse_str(source)
            .map_err(|e| Error::new(0, e.emit_to_string(source)))?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .map_err(|e| Error::new(0, e.to_string()))?;
        Ok(())
    }
}
