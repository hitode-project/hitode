//! Compute expansion of Direct3D vertex-processing stages. The plan refers to
//! admitted registers and assembled primitives and has no guest state.

mod primitive;
pub mod statistics;
#[cfg(test)]
mod tests;

use super::analysis::Reflection;
use super::dxbc::fields::*;
use super::ir::{Error, Module, Opcode, Stage, Statement, Vector};
use super::tessellator::{self, Domain, Partitioning};
use super::variant::Variant;
use super::wgsl::{BindingLocation, STATISTICS_SLOT, pulled};
use adv_device::gpu::{
    BufferRange, Expand, ExpansionBinding, ExpansionDispatch, ExpansionDraw, ExpansionPrelude,
    ExpansionRegion, Gpu, ModernDraw, ModernShader, QuerySlot, SlotBindings, StorageBinding,
};
use std::fmt::Write;
use std::sync::Arc;

const WORKGROUP: u32 = 64;
const PARAMETERS: u32 = 32;
const INPUT_SLOT: u32 = 161;
const INDEX_SLOT: u32 = 162;
const SOURCE_SLOT: u32 = 163;
pub const RASTER_SLOT: u32 = 164;
const ARGUMENTS_SLOT: u32 = 165;
const STREAM_SLOT: u32 = 166;
const SCAN_SLOT: u32 = 167;
const FILLED_SLOT: u32 = 168;
const TARGET_BASE: u32 = 169;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Topology {
    Point,
    Line,
    Triangle,
}

impl Topology {
    pub fn vertices(self) -> u32 {
        match self {
            Self::Point => 1,
            Self::Line => 2,
            Self::Triangle => 3,
        }
    }

    pub fn fixed_slots(self, max_vertices: u32) -> u32 {
        self.vertices() * max_vertices.saturating_sub(self.vertices() - 1)
    }

    fn from_geometry(controls: u32) -> Result<Self, Error> {
        match controls {
            D3D10_SB_PRIMITIVE_TOPOLOGY_POINTLIST => Ok(Self::Point),
            D3D10_SB_PRIMITIVE_TOPOLOGY_LINESTRIP => Ok(Self::Line),
            D3D10_SB_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP => Ok(Self::Triangle),
            _ => Err(Error::Invalid("geometry output topology")),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Shader<'a> {
    pub ir: &'a Module,
    pub variant: &'a Variant,
}

pub enum Source<'a> {
    Vertex(Shader<'a>),
    Domain {
        shader: Shader<'a>,
        control_points: u32,
        control_registers: u32,
        patch_registers: u32,
    },
}

impl Source<'_> {
    /// Assemble unique domain invocations followed by the fixed-function
    /// tessellator's point, line or triangle list.
    pub fn domain_inputs(
        domain: Domain,
        partitioning: Partitioning,
        primitive_id: u32,
        control: &[Vector],
        patch: &[Vector],
    ) -> (Vec<Vector>, Vec<u32>) {
        let mesh = tessellator::tessellate(domain, partitioning);
        let mut vertices =
            Vec::with_capacity(mesh.points.len() * (2 + control.len() + patch.len()));
        for point in mesh.points {
            let [u, v, w] = point.map(f32::to_bits);
            vertices.push([u, v, w, 0]);
            vertices.push([primitive_id; 4]);
            vertices.extend_from_slice(control);
            vertices.extend_from_slice(patch);
        }
        (vertices, mesh.indices)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cull {
    None,
    Front,
    Back,
}

pub struct Raster<'a> {
    /// One selected variant per declared viewport and attachment slice.
    pub bins: &'a [&'a Variant],
    pub fragment: Option<&'a Reflection>,
    pub stream: u32,
    pub cull: Cull,
    pub front_ccw: bool,
    pub depth_clip: bool,
}

pub struct StreamOutput {
    pub stream: u32,
    /// Register and lane at each 32-bit offset in the SO target's stride.
    /// A gap advances the cursor and leaves the target bytes untouched.
    pub elements: Vec<Option<(u32, u8)>>,
}

pub struct Plan<'a> {
    pub source: Source<'a>,
    pub topology: Topology,
    /// The assembled vertices per input primitive, including adjacency.
    pub input_vertices: u32,
    pub geometry: Option<Shader<'a>>,
    pub raster: Option<Raster<'a>>,
    /// Each declaration here has a bound target.
    pub stream_output: &'a [StreamOutput],
}

pub struct ComputeProgram {
    pub source: String,
    pub entry: &'static str,
    /// The guest register layout before modern pipeline slot compaction.
    pub bindings: Vec<BindingLocation>,
    expansion: Vec<ExpansionBinding>,
    workgroup_size: Option<u32>,
}

pub struct Programs {
    pub source: ComputeProgram,
    pub primitive: ComputeProgram,
    pub raster: Vec<String>,
    pub topology: Topology,
    initialize: Option<ComputeProgram>,
    scan: Option<ComputeProgram>,
    write: Option<ComputeProgram>,
    regions: Vec<ExpansionRegion>,
    raster_region: Option<usize>,
    argument_region: Option<usize>,
    storage_varyings: bool,
    wireframe: bool,
    registers: u32,
    raster_stream: u32,
    domain: Option<(u32, u32)>,
}

pub struct Inputs {
    pub primitives: u32,
    /// Packed source input registers; system values occupy their declared lanes.
    pub vertices: BufferRange,
    /// One u32 source record index per input primitive vertex.
    pub indices: BufferRange,
    pub vertex: SlotBindings,
    pub geometry: SlotBindings,
    pub statistics: Option<BufferRange>,
    /// Targets in declaration order, then their u32 filled byte sizes.
    pub stream_output: Option<(Vec<BufferRange>, BufferRange)>,
}

pub struct BinDraw {
    pub draw: ModernDraw,
    pub fragment: SlotBindings,
    pub statistics: Option<QuerySlot>,
}

impl Programs {
    pub fn vertex_shader(&self, bin: usize) -> Result<Arc<ModernShader>, adv_device::gpu::Error> {
        let source = &self.raster[bin];
        ModernShader::wgsl(
            xxhash_rust::xxh3::xxh3_64(source.as_bytes()),
            source,
            "vertex_main",
            adv_device::gpu::ShaderStage::Vertex,
        )
        .map(Arc::new)
    }

    pub fn fragment_shader(
        &self,
        module: &Module,
        variant: &Variant,
        producer: &Reflection,
        shader_identity: u64,
    ) -> Result<Arc<ModernShader>, adv_device::gpu::Error> {
        let mut producer = producer.clone();
        producer
            .signatures
            .output
            .retain(|element| element.stream == self.raster_stream);
        let translation = super::wgsl::emit_expanded_fragment(
            module,
            variant,
            &producer,
            self.topology.vertices(),
            self.registers,
        )
        .map_err(|error| adv_device::gpu::Error::Invalid(error.to_string()))?;
        ModernShader::wgsl(
            variant.pipeline_identity(shader_identity),
            &translation.source,
            translation.entry_point,
            adv_device::gpu::ShaderStage::Fragment,
        )
        .map(Arc::new)
    }

    pub fn command(
        &self,
        gpu: &Gpu,
        inputs: Inputs,
        bins: Vec<BinDraw>,
    ) -> Result<Expand, adv_device::gpu::Error> {
        let Inputs {
            primitives,
            vertices,
            indices,
            mut vertex,
            mut geometry,
            statistics,
            stream_output,
        } = inputs;
        let prelude = if let Some((input_stride, output_stride)) = self.domain {
            let count =
                ((vertices.range.end - vertices.range.start) / u64::from(input_stride)) as u32;
            let outputs = gpu.expansion_storage(u64::from(count) * u64::from(output_stride))?;
            bind_storage(&mut vertex, INPUT_SLOT, vertices);
            bind_storage(&mut vertex, SOURCE_SLOT, outputs.clone());
            bind_storage(&mut geometry, SOURCE_SLOT, outputs);
            bind_storage(&mut geometry, INDEX_SLOT, indices);
            let groups = count.div_ceil(WORKGROUP);
            Some(ExpansionPrelude {
                dispatch: self.source.prepare(gpu)?,
                slots: std::mem::take(&mut vertex),
                groups: [groups.min(65_535), groups.div_ceil(65_535), 1],
            })
        } else {
            bind_storage(&mut vertex, INPUT_SLOT, vertices);
            bind_storage(&mut vertex, INDEX_SLOT, indices);
            None
        };
        if let Some(statistics) = &statistics {
            bind_storage(&mut geometry, STATISTICS_SLOT, statistics.clone());
        }
        let mut programs = Vec::new();
        if let Some(initialize) = &self.initialize {
            programs.push(initialize.dispatch(gpu, SlotBindings::default())?);
        }
        if prelude.is_none() {
            programs.push(self.source.dispatch(gpu, vertex)?);
        }
        programs.push(self.primitive.dispatch(gpu, geometry)?);
        let mut stream = SlotBindings::default();
        if let Some((targets, filled)) = stream_output {
            bind_storage(&mut stream, FILLED_SLOT, filled);
            for (slot, target) in targets.into_iter().enumerate() {
                bind_storage(&mut stream, TARGET_BASE + slot as u32, target);
            }
        }
        if let Some(scan) = &self.scan {
            programs.push(scan.dispatch(gpu, stream.clone())?);
        }
        if let Some(write) = &self.write {
            programs.push(write.dispatch(gpu, stream)?);
        }
        let bins = bins
            .into_iter()
            .zip(&self.raster)
            .enumerate()
            .map(|(i, (mut bin, _))| {
                bin.draw.primitive.topology = match self.topology {
                    Topology::Point => adv_device::gpu::ModernTopology::Points,
                    Topology::Line => adv_device::gpu::ModernTopology::Lines,
                    Topology::Triangle => adv_device::gpu::ModernTopology::Triangles,
                };
                bin.draw.index = None;
                if self.wireframe {
                    bin.draw.primitive.wireframe = false;
                }
                if let Some(statistics) = &statistics {
                    bind_storage(&mut bin.fragment, STATISTICS_SLOT, statistics.clone());
                }
                ExpansionDraw {
                    draw: bin.draw,
                    vertex: SlotBindings::default(),
                    fragment: bin.fragment,
                    vertex_expansion: vec![scratch(RASTER_SLOT, self.raster_region.unwrap())],
                    fragment_expansion: if self.storage_varyings {
                        vec![scratch(RASTER_SLOT, self.raster_region.unwrap())]
                    } else {
                        Vec::new()
                    },
                    indirect: (self.argument_region.unwrap(), i as u32 * 16),
                    statistics: bin.statistics,
                }
            })
            .collect();
        Ok(Expand {
            primitives,
            prelude,
            regions: self.regions.clone(),
            programs,
            bins,
        })
    }
}

impl ComputeProgram {
    fn dispatch(
        &self,
        gpu: &Gpu,
        slots: SlotBindings,
    ) -> Result<ExpansionDispatch, adv_device::gpu::Error> {
        Ok(ExpansionDispatch {
            dispatch: self.prepare(gpu)?,
            slots,
            expansion: self.expansion.clone(),
            workgroup_size: self.workgroup_size,
        })
    }

    fn prepare(&self, gpu: &Gpu) -> Result<adv_device::gpu::Dispatch, adv_device::gpu::Error> {
        let shader = ModernShader::wgsl(
            xxhash_rust::xxh3::xxh3_64(self.source.as_bytes()),
            &self.source,
            self.entry,
            adv_device::gpu::ShaderStage::Compute,
        )?;
        gpu.prepare_dispatch(Arc::new(shader))
    }
}

fn bind_storage(slots: &mut SlotBindings, register: u32, buffer: BufferRange) {
    if slots.storage.len() <= register as usize {
        slots.storage.resize_with(register as usize + 1, || None);
    }
    slots.storage[register as usize] = Some(StorageBinding::Buffer(buffer));
}

fn scratch(slot: u32, region: usize) -> ExpansionBinding {
    ExpansionBinding::Scratch { slot, region }
}

fn params() -> ExpansionBinding {
    ExpansionBinding::Parameters { slot: PARAMETERS }
}

fn parameters(source: &mut String) {
    writeln!(
        source,
        "@group(0) @binding({PARAMETERS}) var<uniform> dx_params: vec4<u32>;"
    )
    .unwrap();
}

fn storage(source: &mut String, slot: u32, name: &str, ty: &str, writable: bool) {
    let access = if writable { "read_write" } else { "read" };
    writeln!(
        source,
        "@group(3) @binding({slot}) var<storage, {access}> {name}: {ty};"
    )
    .unwrap();
}

fn entry(source: &mut String, name: &str, group: u32) {
    writeln!(source, "@compute @workgroup_size({group}) fn {name}(@builtin(global_invocation_id) id: vec3<u32>) {{").unwrap();
}

const PRIMITIVE_INDEX: &str = "let primitive = id.x; if primitive >= dx_params.y { return; }";

impl Plan<'_> {
    pub fn emit(&self) -> Result<Programs, Error> {
        let (source, input_ir, input_registers) = self.emit_source()?;
        let domain = match self.source {
            Source::Vertex(_) => None,
            Source::Domain {
                control_points,
                control_registers,
                patch_registers,
                ..
            } => Some((
                (2 + control_points * control_registers + patch_registers) * 16,
                input_registers * 16,
            )),
        };
        let mut regions = Vec::new();
        if domain.is_none() {
            regions.push(ExpansionRegion {
                bytes_per_primitive: self.input_vertices * input_registers * 16,
                fixed_bytes: 0,
                element_stride: input_registers * 16,
            });
        }
        let program = primitive::Emitter::new(self, input_ir, input_registers)?;
        let topology = program.raster_topology();
        let (initialize, raster_region, argument_region, raster) = match &self.raster {
            Some(raster) if program.raster_slots() != 0 => {
                let vertex_region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: program.raster_slots() * (program.outputs + 1) * 16,
                    fixed_bytes: 0,
                    element_stride: (program.outputs + 1) * 16,
                });
                let argument_region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: 0,
                    fixed_bytes: raster.bins.len() as u32 * 16,
                    element_stride: 4,
                });
                let initialize = primitive::initialize(raster.bins.len(), argument_region);
                let shaders = raster
                    .bins
                    .iter()
                    .map(|variant| {
                        pulled::raster(
                            program.output_ir,
                            variant,
                            raster.fragment,
                            program.outputs,
                            topology.vertices(),
                            raster.stream,
                        )
                    })
                    .collect::<Result<_, _>>()?;
                (
                    Some(initialize),
                    Some(vertex_region),
                    Some(argument_region),
                    shaders,
                )
            }
            _ => (None, None, None, Vec::new()),
        };
        let stream_region =
            (!self.stream_output.is_empty() && program.list_slots() != 0).then(|| {
                let region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: program.list_slots() * (program.outputs + 1) * 16,
                    fixed_bytes: 0,
                    element_stride: (program.outputs + 1) * 16,
                });
                region
            });
        let scan_region = stream_region.map(|_| {
            let region = regions.len();
            regions.push(ExpansionRegion {
                bytes_per_primitive: 4 * self.stream_output.len() as u32 * program.list_slots(),
                fixed_bytes: 0,
                element_stride: 4,
            });
            region
        });
        let primitive = program.emit(raster_region.zip(argument_region), stream_region);
        let (scan, write) =
            stream_region
                .zip(scan_region)
                .map_or((None, None), |(stream, scan)| {
                    let (p, w) = program.stream_output(stream, scan);
                    (Some(p), Some(w))
                });
        Ok(Programs {
            source,
            primitive,
            topology,
            raster,
            initialize,
            scan,
            write,
            regions,
            raster_region,
            argument_region,
            storage_varyings: self.raster.as_ref().is_some_and(|r| {
                r.bins
                    .first()
                    .is_some_and(|variant| variant.storage_varyings())
            }),
            wireframe: program.wireframe(),
            registers: program.outputs,
            raster_stream: self.raster.as_ref().map_or(0, |raster| raster.stream),
            domain,
        })
    }

    fn emit_source(&self) -> Result<(ComputeProgram, &Module, u32), Error> {
        use pulled::Invocation;
        let (shader, invocation) = match self.source {
            Source::Vertex(shader) => (shader, Invocation::Vertex),
            Source::Domain {
                shader,
                control_points,
                control_registers,
                patch_registers,
            } => (
                shader,
                Invocation::Domain {
                    points: control_points,
                    control: control_registers,
                    patch: patch_registers,
                },
            ),
        };
        if !matches!(
            (shader.ir.stage, &invocation),
            (Stage::Vertex, Invocation::Vertex) | (Stage::Domain, Invocation::Domain { .. })
        ) {
            return Err(Error::Invalid("expansion source stage"));
        }
        let pulled = pulled::emit(shader.ir, shader.variant, invocation)?;
        let mut source = pulled.source;
        storage(
            &mut source,
            INPUT_SLOT,
            "dx_inputs",
            "array<vec4<u32>>",
            false,
        );
        storage(
            &mut source,
            SOURCE_SLOT,
            "dx_source",
            "array<DxOutput>",
            true,
        );
        let expansion = if matches!(invocation, Invocation::Vertex) {
            parameters(&mut source);
            storage(&mut source, INDEX_SLOT, "dx_indices", "array<u32>", false);
            entry(&mut source, "source_main", WORKGROUP);
            writeln!(
                source,
                "{PRIMITIVE_INDEX}\nfor (var j = 0u; j < {}u; j += 1u) {{",
                self.input_vertices
            )
            .unwrap();
            writeln!(
                source,
                "let index = dx_indices[(dx_params.x + primitive) * {}u + j];",
                self.input_vertices
            )
            .unwrap();
            vec![params(), scratch(SOURCE_SLOT, 0)]
        } else {
            entry(&mut source, "source_main", WORKGROUP);
            writeln!(source, "let index = id.x + id.y * {}u;", 65_535 * WORKGROUP).unwrap();
            Vec::new()
        };
        match invocation {
            Invocation::Vertex => {
                writeln!(source, "var input: array<vec4<u32>, {}>;", pulled.inputs).unwrap();
                writeln!(source, "for (var r = 0u; r < {}u; r += 1u) {{ input[r] = dx_inputs[index * {}u + r]; }}", pulled.inputs, pulled.inputs).unwrap();
                writeln!(
                    source,
                    "dx_source[primitive * {}u + j] = dx_run(input);\n}}",
                    self.input_vertices
                )
                .unwrap();
            }
            Invocation::Domain {
                points,
                control,
                patch,
            } => {
                let stride = 2 + points * control + patch;
                writeln!(
                    source,
                    "let at = index * {stride}u; if at >= arrayLength(&dx_inputs) {{ return; }}"
                )
                .unwrap();
                writeln!(
                    source,
                    "var cp: array<array<vec4<u32>, {control}>, {points}>;"
                )
                .unwrap();
                writeln!(
                    source,
                    "for (var p = 0u; p < {points}u; p += 1u) {{
                    for (var r = 0u; r < {control}u; r += 1u) {{
                        cp[p][r] = dx_inputs[at + 2u + p * {control}u + r];
                    }}
                }}"
                )
                .unwrap();
                writeln!(
                    source,
                    "var patch_values: array<vec4<u32>, {}>;",
                    patch.max(1)
                )
                .unwrap();
                writeln!(source, "for (var r = 0u; r < {patch}u; r += 1u) {{ patch_values[r] = dx_inputs[at + {}u + r]; }}", 2 + points * control).unwrap();
                writeln!(
                    source,
                    "dx_source[index] = dx_run(dx_inputs[at], dx_inputs[at + 1u], cp, patch_values);"
                )
                .unwrap();
            }
            Invocation::Geometry { .. } => unreachable!(),
        }
        source.push_str("}\n");
        Ok((
            ComputeProgram {
                source,
                entry: "source_main",
                bindings: pulled.locations,
                expansion,
                workgroup_size: Some(WORKGROUP),
            },
            shader.ir,
            pulled.outputs,
        ))
    }
}

fn declaration(module: &Module, opcode: Opcode) -> Option<&super::ir::Declaration> {
    module.body.iter().find_map(|statement| match statement {
        Statement::Declare(decl) if decl.opcode == opcode => Some(decl),
        _ => None,
    })
}
