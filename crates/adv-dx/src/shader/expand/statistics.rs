//! D3D11 pipeline statistics combine recording counts and raster or expansion
//! counts. Query boundaries and per-draw slots are chosen by the recording.

use super::{STATISTICS_SLOT, Topology};
use crate::shader::tessellator::Mesh;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PipelineStatistics {
    pub ia_vertices: u64,
    pub ia_primitives: u64,
    pub vs_invocations: u64,
    pub gs_invocations: u64,
    pub gs_primitives: u64,
    pub c_invocations: u64,
    pub c_primitives: u64,
    pub ps_invocations: u64,
    pub hs_invocations: u64,
    pub ds_invocations: u64,
    pub cs_invocations: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rasterization {
    Enabled,
    NoRasterizedStream,
    NullPixelWithoutDepthStencil,
}

impl Rasterization {
    pub fn select(rasterized_stream: bool, pixel_shader: bool, depth_or_stencil: bool) -> Self {
        if !rasterized_stream {
            Self::NoRasterizedStream
        } else if !pixel_shader && !depth_or_stencil {
            Self::NullPixelWithoutDepthStencil
        } else {
            Self::Enabled
        }
    }

    pub fn enabled(self) -> bool {
        self == Self::Enabled
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Assembly {
    /// Vertices presented to the IA, including vertices of incomplete primitives.
    pub vertices: u32,
    /// Complete primitives after strip cuts and adjacency assembly.
    pub primitives: u32,
    pub instances: u32,
}

/// The paired low and high words of each counter are contiguous in storage.
/// The first four counters are written by the pixel and expansion programs.
pub const GPU_WORDS: usize = 16;
pub const GPU_COUNTERS: [usize; GPU_WORDS / 2] = [7, 4, 5, 6, 3, 9, 0, 1];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssemblyTopology {
    List(u32),
    Strip(u32, u32),
}

impl AssemblyTopology {
    pub fn primitives(self, vertices: u32) -> u32 {
        match self {
            Self::List(width) => vertices / width,
            Self::Strip(width, step) => vertices.checked_sub(width).map_or(0, |n| 1 + n / step),
        }
    }
}

impl PipelineStatistics {
    pub fn record_draw(
        &mut self,
        ia: Assembly,
        gs_instances: Option<u32>,
        tessellated: Option<(u64, u64)>,
        rasterization: Rasterization,
    ) {
        let vertices = u64::from(ia.vertices) * u64::from(ia.instances);
        let primitives = u64::from(ia.primitives) * u64::from(ia.instances);
        self.ia_vertices += vertices;
        self.ia_primitives += primitives;
        self.vs_invocations += vertices;
        let clipper_inputs = if let Some((points, primitives_out)) = tessellated {
            self.hs_invocations += primitives;
            self.ds_invocations += points;
            primitives_out
        } else {
            primitives
        };
        if let Some(gs_instances) = gs_instances {
            self.gs_invocations += clipper_inputs * u64::from(gs_instances);
        }
        if gs_instances.is_none() && rasterization.enabled() {
            self.c_invocations += clipper_inputs;
        }
    }

    pub fn record_dispatch(&mut self, groups: [u32; 3], threads: [u32; 3]) {
        self.cs_invocations += groups
            .into_iter()
            .chain(threads)
            .map(u64::from)
            .product::<u64>();
    }

    pub fn tessellated(meshes: &[Mesh], topology: Topology) -> (u64, u64) {
        meshes.iter().fold((0, 0), |(points, primitives), mesh| {
            (
                points + mesh.points.len() as u64,
                primitives + mesh.indices.len() as u64 / u64::from(topology.vertices()),
            )
        })
    }

    /// Accumulate the expansion's output and, without a native query, raster counts.
    pub fn record_gpu(&mut self, words: [u32; GPU_WORDS], geometry: bool) {
        let count =
            |index: usize| u64::from(words[index * 2]) | u64::from(words[index * 2 + 1]) << 32;
        self.ps_invocations += count(0);
        self.gs_primitives += count(1);
        if geometry {
            self.c_invocations += count(2);
        }
        self.c_primitives += count(3);
    }

    pub fn record_indirect(&mut self, words: [u32; INDIRECT_WORDS]) {
        let count =
            |index: usize| u64::from(words[index * 2]) | u64::from(words[index * 2 + 1]) << 32;
        self.ia_vertices += count(0);
        self.ia_primitives += count(1);
        self.vs_invocations += count(2);
        self.gs_invocations += count(3);
        self.c_invocations += count(5);
        self.hs_invocations += count(8);
        self.cs_invocations += count(10);
    }

    pub fn record_native(&mut self, [clipping, fragments]: [u64; 2], pixel_shader: bool) {
        self.c_primitives += clipping;
        if pixel_shader {
            self.ps_invocations += fragments;
        }
    }
}

pub(super) const ADD_COUNTER: &str = "
fn dx_add_counter(counter: u32) {
    dx_add_count(counter, 1u);
}
fn dx_add_count(counter: u32, count: u32) {
    let index = counter * 2u;
    let previous = atomicAdd(&dx_stats[index], count);
    if previous + count < previous {
        atomicAdd(&dx_stats[index + 1u], 1u);
    }
}
";

/// Indirect counts occupy a separate recording buffer in D3D counter order.
pub const INDIRECT_WORDS: usize = 22;
pub const INDIRECT_ARGUMENTS_SLOT: u32 = 159;

pub enum Indirect {
    Draw {
        topology: AssemblyTopology,
        gs_instances: u32,
        hull: bool,
        rasterization: Rasterization,
    },
    Dispatch {
        threads_per_group: u32,
    },
}

impl Indirect {
    /// Emit a single-thread helper for the argument layout shared by direct
    /// and indexed indirect draws. Strip-cut counts come from the assembly.
    pub fn compute(&self) -> String {
        self.compute_at(0)
    }

    pub fn compute_at(&self, offset: u64) -> String {
        let body = match *self {
            Self::Draw {
                topology,
                gs_instances,
                hull,
                rasterization,
            } => {
                let primitives = match topology {
                    AssemblyTopology::List(width) => format!("vertices / {width}u"),
                    AssemblyTopology::Strip(width, step) => {
                        format!(
                            "select(0u, 1u + (vertices - {width}u) / {step}u, vertices >= {width}u)"
                        )
                    }
                };
                let mut body = format!(
                    "let vertices = dx_arguments[0]; let instances = dx_arguments[1];
                    let primitives = {primitives};
                    dx_add_product(0u, vertices, instances);
                    dx_add_product(1u, primitives, instances);
                    dx_add_product(2u, vertices, instances);"
                );
                if hull {
                    body.push_str("dx_add_product(8u, primitives, instances);");
                } else if gs_instances != 0 {
                    body.push_str(&format!(
                        "dx_add_product(3u, primitives, instances * {gs_instances}u);"
                    ));
                } else if rasterization.enabled() {
                    body.push_str("dx_add_product(5u, primitives, instances);");
                }
                body
            }
            Self::Dispatch { threads_per_group } => format!(
                "dx_add_product(10u, dx_arguments[0] * dx_arguments[1], dx_arguments[2] * {threads_per_group}u);"
            ),
        };
        let counters = STATISTICS_SLOT;
        format!(
            "
@group(3) @binding({INDIRECT_ARGUMENTS_SLOT}) var<storage, read> dx_argument_words: array<u32>;
@group(3) @binding({counters}) var<storage, read_write> dx_stats: array<atomic<u32>>;
fn dx_add_product(counter: u32, a: u32, b: u32) {{
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let ll = a0 * b0;
    let hl = a1 * b0 + (ll >> 16u);
    let lh = a0 * b1 + (hl & 0xffffu);
    let low = ((lh & 0xffffu) << 16u) | (ll & 0xffffu);
    let high = a1 * b1 + (hl >> 16u) + (lh >> 16u);
    let at = counter * 2u;
    let prior = atomicAdd(&dx_stats[at], low);
    atomicAdd(&dx_stats[at + 1u], high + select(0u, 1u, prior + low < prior));
}}
@compute @workgroup_size(1) fn indirect_statistics() {{
    let dx_arguments = vec3<u32>(dx_argument_words[{offset}u], dx_argument_words[{offset}u + 1u], dx_argument_words[{offset}u + 2u]);
    {body}
}}
"
        )
    }
}
