//! Compute expansion of Direct3D vertex-processing stages. The plan refers to
//! admitted registers and assembled primitives and has no guest state.

mod primitive;
mod schedule;
mod strip;
pub use schedule::Indirect;
pub mod statistics;
mod tessellation;
pub use tessellation::Tessellation;
#[cfg(test)]
mod tests;

use super::analysis::Reflection;
use super::dxbc::fields::*;
use super::ir::{Error, Module, Opcode, Stage, Statement, Vector};
use super::tessellator::{self, Domain, Partitioning};
use super::variant::Variant;
use super::wgsl::{BindingLocation, Options, STATISTICS_SLOT, Translation, pulled};
use adv_device::gpu::{
    BufferRange, Expand, ExpansionBinding, ExpansionDispatch, ExpansionDraw, ExpansionPrelude,
    ExpansionRegion, ExpansionWindow, Gpu, ModernDraw, ModernShader, QuerySlot, SlotBindings,
    StorageBinding,
};
use std::fmt::Write;
use std::sync::Arc;

const WORKGROUP: u32 = 64;
const PARAMETERS: u32 = 32;
const PREDICATE_SLOT: u32 = 184;
const INPUT_SLOT: u32 = 161;
const INDEX_SLOT: u32 = 162;
const SOURCE_SLOT: u32 = 163;
pub const RASTER_SLOT: u32 = 164;
const ARGUMENTS_SLOT: u32 = 165;
const STREAM_SLOT: u32 = 166;
const SCAN_SLOT: u32 = 167;
const FILLED_BASE: u32 = 176;
const TARGET_BASE: u32 = 169;
const STREAM_STATISTICS: u32 = 180;

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
    pub options: Options<'a>,
}

pub enum Source<'a> {
    Vertex(Shader<'a>),
    InputAssembler {
        shader: Shader<'a>,
        fetches: &'a [pulled::VertexFetch],
        assembly: Assembly,
    },
    Tessellated {
        vertex: Shader<'a>,
        hull: &'a Shader<'a>,
        domain: &'a Shader<'a>,
        fetches: &'a [pulled::VertexFetch],
        assembly: Assembly,
        layout: Tessellation,
    },
    Domain {
        shader: Shader<'a>,
        control_points: u32,
        control_registers: u32,
        patch_registers: u32,
    },
}

/// A direct IA draw is expanded in API primitive order.
#[derive(Clone, Copy)]
pub struct Assembly {
    pub count_statistics: bool,
    pub predicate: Option<bool>,
    pub first: u32,
    pub count: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
    pub index: Option<IndexInput>,
    pub step: u32,
    pub flip_winding: bool,
    pub indirect: Option<Indirect>,
}

#[derive(Clone, Copy)]
pub struct IndexInput {
    pub offset: u32,
    pub wide: bool,
}

impl IndexInput {
    fn read(self, source: &mut String, at: &str) {
        let Self { offset, wide } = self;
        if wide {
            writeln!(source, "let vertex_id = dx_indices[{offset}u / 4u + {at}];").unwrap();
        } else {
            writeln!(source, "let word = {offset}u / 2u + {at}; let vertex_id = (dx_indices[word / 2u] >> (16u * (word & 1u))) & 65535u;").unwrap();
        }
    }
}

impl Assembly {
    pub fn primitives(self, input_vertices: u32) -> u32 {
        if self.count < input_vertices {
            0
        } else {
            1 + (self.count - input_vertices) / self.step
        }
    }

    fn strip(self, input_vertices: u32) -> bool {
        self.index.is_some() && self.step < input_vertices
    }

    fn active(self, input_vertices: u32) -> &'static str {
        match (
            self.predicate.is_some(),
            self.indirect.is_some(),
            self.strip(input_vertices),
        ) {
            (true, _, _) => "dx_predicated_count()",
            (_, true, _) => "dx_schedule[0]",
            (_, _, true) => "dx_strip_active()",
            _ => "dx_params.y",
        }
    }

    fn schedule(self, source: &mut String, input_vertices: u32) {
        if self.indirect.is_some() {
            storage(source, schedule::SLOT, "dx_schedule", "array<u32>", false);
        }
        if self.strip(input_vertices) {
            strip::declare(source, false);
            if self.indirect.is_none() {
                let maximum = self.primitives(input_vertices);
                writeln!(
                    source,
                    "fn dx_strip_active() -> u32 {{
                    let total = dx_strip[0].x * (dx_params.z / {maximum}u);
                    return min(dx_params.y, max(total, dx_params.x) - dx_params.x);
                }}"
                )
                .unwrap();
            }
        }
        if let Some(value) = self.predicate {
            storage(source, PREDICATE_SLOT, "dx_predicate", "array<u32>", false);
            let count = Self {
                predicate: None,
                ..self
            }
            .active(input_vertices);
            writeln!(source, "fn dx_predicated_count() -> u32 {{ return select(0u, {count}, (dx_predicate[0] != 0u) == {value}); }}").unwrap();
        }
    }

    fn emit(self, source: &mut String, input_vertices: u32) {
        let Self {
            first,
            base_vertex,
            first_instance,
            index,
            step,
            flip_winding,
            ..
        } = self;
        let count = self.primitives(input_vertices);
        if index.is_some() {
            storage(source, INDEX_SLOT, "dx_indices", "array<u32>", false);
        }
        self.schedule(source, input_vertices);
        entry(source, "source_main", WORKGROUP);
        let active = self.active(input_vertices);
        writeln!(
            source,
            "let primitive = id.x;
            if primitive >= {active} {{ return; }}
            let item = dx_params.x + primitive;"
        )
        .unwrap();
        if self.indirect.is_some() {
            source.push_str("let first = dx_schedule[2]; let base_vertex = dx_schedule[3]; let first_instance = dx_schedule[4]; let per_draw = dx_schedule[1];\n");
        } else {
            let per_draw = if self.strip(input_vertices) {
                "dx_strip[0].x".into()
            } else {
                format!("{count}u")
            };
            writeln!(source, "let first = {first}u; let base_vertex = {}u; let first_instance = {first_instance}u; let per_draw = {per_draw};", base_vertex as u32).unwrap();
        }
        writeln!(
            source,
            "let instance = item / per_draw + first_instance;
            let per_instance = item % per_draw;
            for (var j = 0u; j < {input_vertices}u; j += 1u) {{"
        )
        .unwrap();
        let strip = self.strip(input_vertices);
        if strip {
            source.push_str("let strip_item = dx_strip[1u + per_instance];\n");
        }
        if input_vertices == 6 && flip_winding {
            if strip {
                source.push_str(
                    "let base = strip_item.x;
                    let in_strip = strip_item.y;
                    let has_next = per_instance + 1u < per_draw
                        && dx_strip[2u + per_instance].y == in_strip + 1u;\n",
                );
            } else {
                source.push_str(
                    "let base = first + 2u * per_instance;
                    let in_strip = per_instance;
                    let has_next = per_instance + 1u < per_draw;\n",
                );
            }
            // The previous and next triangles supply the two shared-edge neighbors.
            source.push_str("let adjacent_before = select(base + 1u, base - 2u, in_strip != 0u);
                let adjacent_after = select(base + 5u, base + 6u, has_next);
                let corners = array<u32, 6>(base, adjacent_before, base + 2u, adjacent_after, base + 4u, base + 3u);
                let at = corners[select(j, (6u - j) % 6u, (in_strip & 1u) != 0u)];\n");
        } else {
            if flip_winding {
                writeln!(
                    source,
                    "let corner = select(j, (3u - j) % 3u, ({} & 1u) != 0u);",
                    if strip {
                        "strip_item.y"
                    } else {
                        "per_instance"
                    }
                )
                .unwrap();
            } else {
                source.push_str("let corner = j;\n");
            }
            if strip {
                source.push_str("let at = strip_item.x + corner;\n");
            } else {
                writeln!(source, "let at = first + per_instance * {step}u + corner;").unwrap();
            }
        }
        if let Some(index) = index {
            index.read(source, "at");
        } else {
            source.push_str("let vertex_id = at - first;\n");
        }
        let start = if index.is_some() {
            "base_vertex"
        } else {
            "first"
        };
        writeln!(
            source,
            "dx_source[primitive * {input_vertices}u + j] = dx_fetch(vertex_id + {start}, instance, {start}, first_instance);
            }}"
        ).unwrap();
    }
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

#[derive(Clone)]
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
    /// Mask of declared streams to count when SO statistics are active.
    pub stream_statistics: u32,
}

pub struct ComputeProgram {
    pub source: String,
    pub entry: &'static str,
    /// The guest register layout before modern pipeline slot compaction.
    pub bindings: Vec<BindingLocation>,
    pub counters: Vec<(u32, u32)>,
    expansion: Vec<ExpansionBinding>,
    workgroup_size: Option<u32>,
    items_per_primitive: u32,
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
    assembled: bool,
    tessellation: Option<tessellation::Programs>,
    window_regions: std::ops::Range<usize>,
    assembly: Option<(Assembly, u32)>,
}

pub struct Inputs {
    pub primitives: u32,
    /// Packed source input registers; system values occupy their declared lanes.
    pub vertices: Option<BufferRange>,
    /// One u32 source record index per input primitive vertex.
    pub indices: Option<BufferRange>,
    pub vertex: SlotBindings,
    pub geometry: SlotBindings,
    pub hull: SlotBindings,
    pub domain: SlotBindings,
    pub indirect: Option<BufferRange>,
    pub stream_statistics: Option<BufferRange>,
    pub statistics: Option<BufferRange>,
    pub predicate: Option<BufferRange>,
    /// Targets and their filled byte sizes, in declaration order.
    pub stream_output: Vec<(BufferRange, BufferRange)>,
}

pub struct BinDraw {
    pub draw: ModernDraw,
    pub fragment: SlotBindings,
    pub statistics: Option<QuerySlot>,
}

impl Programs {
    pub fn tessellation_stages(&self) -> Option<(&ComputeProgram, &ComputeProgram)> {
        self.tessellation
            .as_ref()
            .map(|programs| (&programs.vertex, &programs.hull))
    }

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
        options: Options<'_>,
    ) -> Result<Arc<ModernShader>, adv_device::gpu::Error> {
        let translation = self
            .fragment_code(module, variant, producer, options)
            .map_err(|error| adv_device::gpu::Error::Invalid(error.to_string()))?;
        ModernShader::wgsl(
            variant.pipeline_identity(shader_identity),
            &translation.source,
            translation.entry_point,
            adv_device::gpu::ShaderStage::Fragment,
        )
        .map(Arc::new)
    }

    pub fn fragment_code(
        &self,
        module: &Module,
        variant: &Variant,
        producer: &Reflection,
        options: Options<'_>,
    ) -> Result<Translation, Error> {
        let mut producer = producer.clone();
        producer
            .signatures
            .output
            .retain(|element| element.stream == self.raster_stream);
        super::wgsl::emit_expanded_fragment(
            module,
            variant,
            &producer,
            self.topology.vertices(),
            self.registers,
            options,
        )
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
            mut hull,
            mut domain,
            indirect,
            stream_statistics,
            statistics,
            predicate,
            stream_output,
        } = inputs;
        let mut strip_buffer = None;
        let prelude = if let Some((input_stride, output_stride)) = self.domain {
            let vertices = vertices.unwrap();
            let count =
                ((vertices.range.end - vertices.range.start) / u64::from(input_stride)) as u32;
            let outputs = gpu.expansion_storage(u64::from(count) * u64::from(output_stride))?;
            bind_storage(&mut vertex, INPUT_SLOT, vertices);
            bind_storage(&mut vertex, SOURCE_SLOT, outputs.clone());
            bind_storage(&mut geometry, SOURCE_SLOT, outputs);
            bind_storage(&mut geometry, INDEX_SLOT, indices.unwrap());
            let groups = count.div_ceil(WORKGROUP);
            Some(ExpansionPrelude {
                dispatch: self.source.prepare(gpu)?,
                slots: std::mem::take(&mut vertex),
                groups: [groups.min(65_535), groups.div_ceil(65_535), 1],
            })
        } else {
            if self.assembled {
                bind_storage(&mut vertex, INPUT_SLOT, vertices.unwrap());
            }
            let strip = self
                .assembly
                .filter(|(assembly, width)| assembly.strip(*width));
            let prelude = if let Some((assembly, width)) = strip {
                let (buffer, prelude) = strip::prepare(
                    gpu,
                    assembly,
                    width,
                    indices.as_ref().unwrap().clone(),
                    indirect.as_ref(),
                )?;
                bind_storage(&mut vertex, strip::SLOT, buffer.clone());
                bind_storage(&mut geometry, strip::SLOT, buffer.clone());
                strip_buffer = Some(buffer);
                Some(prelude)
            } else {
                None
            };
            if let Some(indices) = indices {
                bind_storage(&mut vertex, INDEX_SLOT, indices);
            }
            prelude
        };
        if let Some(statistics) = &statistics {
            bind_storage(&mut geometry, STATISTICS_SLOT, statistics.clone());
            bind_storage(&mut domain, STATISTICS_SLOT, statistics.clone());
        }
        let mut programs = Vec::new();
        if let Some(tessellation) = &self.tessellation {
            programs.push(tessellation.vertex.dispatch(gpu, vertex)?);
            programs.push(tessellation.hull.dispatch(gpu, std::mem::take(&mut hull))?);
            for pass in &tessellation.fixed {
                programs.push(pass.dispatch(gpu, SlotBindings::default())?);
            }
            programs.push(self.source.dispatch(gpu, std::mem::take(&mut domain))?);
        } else if self.domain.is_none() {
            programs.push(self.source.dispatch(gpu, vertex)?);
        }
        let first_output = programs.len();
        if let Some(initialize) = &self.initialize {
            programs.push(initialize.dispatch(gpu, SlotBindings::default())?);
        }
        programs.push(self.primitive.dispatch(gpu, geometry)?);
        let mut stream = SlotBindings::default();
        for (slot, (target, filled)) in stream_output.into_iter().enumerate() {
            bind_storage(&mut stream, TARGET_BASE + slot as u32, target);
            bind_storage(&mut stream, FILLED_BASE + slot as u32, filled);
        }
        if let Some(buffer) = &strip_buffer {
            bind_storage(&mut stream, strip::SLOT, buffer.clone());
        }
        if let Some(statistics) = stream_statistics {
            bind_storage(&mut stream, STREAM_STATISTICS, statistics);
        }
        if let Some(scan) = &self.scan {
            programs.push(scan.dispatch(gpu, stream.clone())?);
        }
        if let Some(write) = &self.write {
            programs.push(write.dispatch(gpu, stream)?);
        }
        if let Some(predicate) = predicate {
            for program in &mut programs {
                bind_storage(&mut program.slots, PREDICATE_SLOT, predicate.clone());
            }
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
        let mut regions = self.regions.clone();
        let mut first_output = first_output;
        if let Some((assembly, input_vertices)) = self
            .assembly
            .filter(|(assembly, _)| assembly.indirect.is_some())
        {
            schedule::insert(
                gpu,
                assembly,
                input_vertices,
                (indirect.unwrap(), strip_buffer),
                &mut regions,
                &mut programs,
                self.initialize.as_ref().map(|_| first_output),
            )?;
            first_output += 1;
        }
        Ok(Expand {
            primitives,
            prelude,
            regions,
            programs,
            bins,
            window: self.tessellation.as_ref().map(|programs| ExpansionWindow {
                first_program: first_output,
                regions: self.window_regions.clone(),
                maximum: programs.output.maximum,
            }),
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
            items_per_primitive: self.items_per_primitive,
            indirect: None,
        })
    }

    fn prepare(&self, gpu: &Gpu) -> Result<adv_device::gpu::Dispatch, adv_device::gpu::Error> {
        let mut identity = xxhash_rust::xxh3::Xxh3::new();
        identity.update(self.source.as_bytes());
        identity.update(self.entry.as_bytes());
        let shader = ModernShader::wgsl(
            identity.digest(),
            &self.source,
            self.entry,
            adv_device::gpu::ShaderStage::Compute,
        )?;
        gpu.prepare_dispatch(Arc::new(shader))
    }
}

/// Convert a stream-output cursor to native draw arguments on the GPU.
pub fn draw_auto(
    gpu: &Gpu,
    cursor: BufferRange,
    offset: u32,
    stride: u32,
) -> Result<(adv_device::gpu::Dispatch, SlotBindings, BufferRange), adv_device::gpu::Error> {
    use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
    let invalid = |error: adv_gpu::Error| adv_device::gpu::Error::Invalid(error.to_string());
    let params = gpu
        .device()
        .create_buffer(&BufferDesc {
            size: 16,
            usage: BufferUsage::UNIFORM,
            memory: Memory::Upload,
            label: "stream-output input offset and stride",
        })
        .map_err(invalid)?;
    // This parameter range belongs to the draw being prepared.
    unsafe { &mut *params.mapped().unwrap().as_ptr() }
        .copy_from_slice(bytemuck::bytes_of(&[offset, stride, 0, 0]));
    let shader = Arc::new(ModernShader::wgsl(
        0x757f_047b_d84e_069c,
        "@group(0) @binding(0) var<uniform> input: vec4<u32>;
        @group(3) @binding(0) var<storage, read> filled: array<u32>;
        @group(3) @binding(1) var<storage, read_write> args: vec4<u32>;
        @compute @workgroup_size(1) fn main() {
            let count = (max(filled[0], input.x) - input.x) / input.y;
            args = vec4<u32>(count, 1u, 0u, 0u);
        }",
        "main",
        adv_device::gpu::ShaderStage::Compute,
    )?);
    let arguments = BufferRange {
        buffer: gpu
            .device()
            .create_buffer(&BufferDesc {
                size: 16,
                usage: BufferUsage::STORAGE | BufferUsage::INDIRECT,
                memory: Memory::Device,
                label: "stream-output draw arguments",
            })
            .map_err(invalid)?,
        range: 0..16,
    };
    let mut bindings = SlotBindings {
        uniforms: vec![Some(
            BufferRange {
                buffer: params,
                range: 0..16,
            }
            .into(),
        )],
        ..Default::default()
    };
    bind_storage(&mut bindings, 0, cursor);
    bind_storage(&mut bindings, 1, arguments.clone());
    Ok((gpu.prepare_dispatch(shader)?, bindings, arguments))
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
    fn assembly(&self) -> Option<(Assembly, u32)> {
        match self.source {
            Source::InputAssembler { assembly, .. } => Some((assembly, self.input_vertices)),
            Source::Tessellated {
                assembly, layout, ..
            } => Some((assembly, layout.input_points)),
            _ => None,
        }
    }

    pub fn emit(&self) -> Result<Programs, Error> {
        let mut regions = Vec::new();
        let (source, input_ir, input_registers, tessellation) = match self.source {
            Source::Tessellated {
                vertex,
                hull,
                domain,
                fetches,
                assembly,
                layout,
            } => {
                let (source, outputs, tessellation) =
                    layout.emit(vertex, *hull, *domain, fetches, assembly, &mut regions)?;
                (source, domain.ir, outputs, Some(tessellation))
            }
            _ => {
                let (source, ir, outputs) = self.emit_source()?;
                (source, ir, outputs, None)
            }
        };
        let domain = match self.source {
            Source::Vertex(_) | Source::InputAssembler { .. } | Source::Tessellated { .. } => None,
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
        if domain.is_none() && tessellation.is_none() {
            regions.push(ExpansionRegion {
                bytes_per_primitive: self.input_vertices * input_registers * 16,
                fixed_bytes: 0,
                element_stride: input_registers * 16,
            });
        }
        let program = primitive::Emitter::new(
            self,
            input_ir,
            input_registers,
            tessellation.as_ref().map(|programs| programs.output),
        )?;
        let window_start = regions.len();
        let topology = program.raster_topology();
        let (initialize, raster_region, argument_region, raster) = match &self.raster {
            Some(raster) if program.raster_slots() != 0 => {
                let vertex_region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: program.raster_slots()
                        * (program.outputs + 1)
                        * 16
                        * program.items(),
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
        let stream_region = (program.list_slots() != 0
            && (!self.stream_output.is_empty() || self.stream_statistics != 0))
            .then(|| {
                let region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: program.list_slots()
                        * (program.outputs + 1)
                        * 16
                        * program.items(),
                    fixed_bytes: 0,
                    element_stride: (program.outputs + 1) * 16,
                });
                region
            });
        let scan_region = stream_region
            .filter(|_| !self.stream_output.is_empty())
            .map(|_| {
                let region = regions.len();
                regions.push(ExpansionRegion {
                    bytes_per_primitive: 4
                        * self.stream_output.len() as u32
                        * program.list_slots()
                        * program.items(),
                    fixed_bytes: 0,
                    element_stride: 4,
                });
                region
            });
        let primitive = program.emit(raster_region.zip(argument_region), stream_region);
        let (scan, write) = stream_region.map_or((None, None), |stream| {
            let (prefix, write) = program.stream_output(stream, scan_region);
            (Some(prefix), write)
        });
        let window_end = regions.len();
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
            assembled: matches!(self.source, Source::Vertex(_)),
            assembly: self.assembly(),
            tessellation,
            window_regions: window_start..window_end,
        })
    }

    fn emit_source(&self) -> Result<(ComputeProgram, &Module, u32), Error> {
        use pulled::Invocation;
        if let Source::InputAssembler {
            shader,
            fetches,
            assembly,
        } = self.source
        {
            let pulled = pulled::fetched(shader.ir, shader.variant, shader.options, fetches)?;
            let mut source = pulled.source;
            parameters(&mut source);
            storage(
                &mut source,
                SOURCE_SLOT,
                "dx_source",
                "array<DxOutput>",
                true,
            );
            assembly.emit(&mut source, self.input_vertices);
            source.push_str("}\n");
            return Ok((
                ComputeProgram {
                    source,
                    entry: "source_main",
                    bindings: pulled.locations,
                    counters: pulled.counters,
                    expansion: vec![params(), scratch(SOURCE_SLOT, 0)],
                    workgroup_size: Some(WORKGROUP),
                    items_per_primitive: 1,
                },
                shader.ir,
                pulled.outputs,
            ));
        }
        let (shader, invocation) = match self.source {
            Source::InputAssembler { .. } | Source::Tessellated { .. } => unreachable!(),
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
        let pulled = pulled::emit(shader.ir, shader.variant, invocation, shader.options)?;
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
                counters: pulled.counters,
                expansion,
                workgroup_size: Some(WORKGROUP),
                items_per_primitive: 1,
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
