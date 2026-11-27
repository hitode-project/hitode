//! Hull execution, fixed-function tessellation, and the pulled domain stage
//! share the expansion chunk's scratch, with a compact point and index list.

use super::*;
use crate::shader::ir::{SignatureElement, SystemValue, passes::variant::lanes};

const TESS_PATCHES: u32 = 205;
const HULL_OUTPUT: u32 = 206;
pub(super) const POINTS: u32 = 207;
pub(super) const RANGES: u32 = 208;

#[derive(Clone, Copy)]
pub struct Tessellation {
    pub input_points: u32,
    output_points: u32,
    domain: u32,
    partitioning: u32,
    output: u32,
    max_points: u32,
    max_indices: u32,
    pub topology: Topology,
}

impl Tessellation {
    pub fn new(hull: &Module, domain: &Module) -> Result<Self, Error> {
        let invalid = Error::Invalid("tessellation declarations");
        let controls = |module, opcode| {
            declaration(module, opcode)
                .map(|declaration| declaration.controls)
                .ok_or(invalid)
        };
        let input_points =
            decode_input_control_point_count(controls(hull, Opcode::DclInputControlPointCount)?);
        let output_points =
            decode_output_control_point_count(controls(hull, Opcode::DclOutputControlPointCount)?);
        let hull_domain = decode_tess_domain(controls(hull, Opcode::DclTessDomain)?);
        if output_points
            != decode_input_control_point_count(controls(
                domain,
                Opcode::DclInputControlPointCount,
            )?)
            || hull_domain != decode_tess_domain(controls(domain, Opcode::DclTessDomain)?)
        {
            return Err(invalid);
        }
        let (domain, max_points, max_indices) = match hull_domain {
            D3D11_SB_TESSELLATOR_DOMAIN_TRI => (0, 3169, 18432),
            D3D11_SB_TESSELLATOR_DOMAIN_QUAD => (1, 4225, 24576),
            D3D11_SB_TESSELLATOR_DOMAIN_ISOLINE => (2, 4160, 8192),
            _ => return Err(invalid),
        };
        let partitioning =
            match decode_tess_partitioning(controls(hull, Opcode::DclTessPartitioning)?) {
                D3D11_SB_TESSELLATOR_PARTITIONING_INTEGER
                | D3D11_SB_TESSELLATOR_PARTITIONING_POW2 => 0,
                D3D11_SB_TESSELLATOR_PARTITIONING_FRACTIONAL_ODD => 1,
                D3D11_SB_TESSELLATOR_PARTITIONING_FRACTIONAL_EVEN => 2,
                _ => return Err(invalid),
            };
        let (output, topology, max_indices) =
            match decode_tess_output_primitive(controls(hull, Opcode::DclTessOutputPrimitive)?) {
                D3D11_SB_TESSELLATOR_OUTPUT_POINT => (0, Topology::Point, max_points),
                D3D11_SB_TESSELLATOR_OUTPUT_LINE if domain == 2 => (1, Topology::Line, max_indices),
                D3D11_SB_TESSELLATOR_OUTPUT_TRIANGLE_CW if domain != 2 => {
                    (2, Topology::Triangle, max_indices)
                }
                D3D11_SB_TESSELLATOR_OUTPUT_TRIANGLE_CCW if domain != 2 => {
                    (3, Topology::Triangle, max_indices)
                }
                _ => return Err(invalid),
            };
        Ok(Self {
            input_points,
            output_points,
            domain,
            partitioning,
            output,
            max_points,
            max_indices,
            topology,
        })
    }

    pub(super) fn emit(
        self,
        vertex: Shader<'_>,
        hull: Shader<'_>,
        domain: Shader<'_>,
        fetches: &[pulled::VertexFetch],
        assembly: Assembly,
        regions: &mut Vec<ExpansionRegion>,
    ) -> Result<(ComputeProgram, u32, Programs), Error> {
        let mut region = |items: u32, stride| {
            let index = regions.len();
            regions.push(ExpansionRegion {
                bytes_per_primitive: items * stride,
                fixed_bytes: 0,
                element_stride: stride,
            });
            index
        };
        let vs = pulled::fetched(vertex.ir, vertex.variant, vertex.options, fetches)?;
        let vs_region = region(self.input_points, vs.outputs * 16);
        let hs = pulled::hull(
            hull.ir,
            &super::super::analysis::analyze(vertex.ir),
            self.input_points,
            self.output_points,
            hull.options,
        )?;
        let hs_patch = registers(&hull.ir.signatures.patch_constant);
        let hs_region = region(1, (self.output_points * hs.outputs + hs_patch) * 16);
        let patches = region(1, 48);
        let ranges = region(1, 16);
        let points = region(self.max_points, 16);
        let indices = region(self.max_indices, 4);

        let mut vs_code = vs.source;
        parameters(&mut vs_code);
        storage(
            &mut vs_code,
            SOURCE_SLOT,
            "dx_source",
            "array<DxOutput>",
            true,
        );
        assembly.emit(&mut vs_code, self.input_points);
        vs_code.push_str("}\n");
        let vertex = program(
            vs_code,
            "source_main",
            vs.locations,
            vs.counters,
            vec![params(), scratch(SOURCE_SLOT, vs_region)],
            1,
        );

        let mut hs_code = hs.source;
        parameters(&mut hs_code);
        assembly.schedule(&mut hs_code, self.input_points);
        hs_code.push_str("struct TessPatch { edges: vec4<u32>, inside: vec2<u32>, domain: u32, partitioning: u32, output: u32, }\n");
        storage(
            &mut hs_code,
            SOURCE_SLOT,
            "dx_source",
            "array<DxVsOutput>",
            false,
        );
        storage(
            &mut hs_code,
            HULL_OUTPUT,
            "dx_hull_output",
            "array<DxHullOutput>",
            true,
        );
        storage(
            &mut hs_code,
            TESS_PATCHES,
            "dx_tess_patches",
            "array<TessPatch>",
            true,
        );
        entry(&mut hs_code, "hull_main", WORKGROUP);
        let count = assembly.primitives(self.input_points);
        let per_draw = if assembly.indirect.is_some() {
            "dx_schedule[1]".into()
        } else {
            format!("{count}u")
        };
        let active = assembly.active(self.input_points);
        writeln!(
            hs_code,
            "let primitive = id.x; if primitive >= {active} {{ return; }}\nvar vs: array<DxVsOutput, {0}>;
            for (var p = 0u; p < {0}u; p++) {{ vs[p] = dx_source[primitive * {0}u + p]; }}
            let result = dx_hull(vs, (dx_params.x + primitive) % {per_draw});
            dx_hull_output[primitive] = result;
            var factors: TessPatch;
            factors.domain = {1}u;
            factors.partitioning = {2}u;
            factors.output = {3}u;",
            self.input_points, self.domain, self.partitioning, self.output
        )
        .unwrap();
        for element in &hull.ir.signatures.patch_constant {
            let member = match element.system_value {
                SystemValue::TessFactor => "edges",
                SystemValue::InsideTessFactor => "inside",
                _ => continue,
            };
            for (index, lane) in lanes(element).enumerate() {
                let component = element.semantic_index + index as u32;
                writeln!(
                    hs_code,
                    "factors.{member}[{component}] = result.values[{}][{}];",
                    lane.register, lane.component
                )
                .unwrap();
            }
        }
        hs_code.push_str("dx_tess_patches[primitive] = factors;\n}\n");
        let hull_program = program(
            hs_code,
            "hull_main",
            hs.locations,
            hs.counters,
            vec![
                params(),
                scratch(SOURCE_SLOT, vs_region),
                scratch(HULL_OUTPUT, hs_region),
                scratch(TESS_PATCHES, patches),
            ],
            1,
        );

        let fixed = [
            (
                "tess_count",
                Some(WORKGROUP),
                vec![scratch(0, patches), scratch(1, ranges)],
            ),
            ("tess_scan", None, vec![scratch(1, ranges)]),
            (
                "tess_emit",
                Some(WORKGROUP),
                vec![
                    scratch(0, patches),
                    scratch(1, ranges),
                    scratch(2, points),
                    scratch(3, indices),
                ],
            ),
        ]
        .map(|(entry, workgroup_size, expansion)| ComputeProgram {
            source: {
                let mut source = include_str!("../tessellator/compute.wgsl").to_owned();
                parameters(&mut source);
                assembly.schedule(&mut source, self.input_points);
                writeln!(source, "fn tess_active() -> u32 {{ return {active}; }}").unwrap();
                source
            },
            entry,
            bindings: Vec::new(),
            counters: Vec::new(),
            expansion: expansion.into_iter().chain([params()]).collect(),
            workgroup_size,
            items_per_primitive: 1,
        });

        let ds_inputs = registers(&domain.ir.signatures.input);
        let ds_patch = registers(&domain.ir.signatures.patch_constant);
        let ds = pulled::emit(
            domain.ir,
            domain.variant,
            pulled::Invocation::Domain {
                points: self.output_points,
                control: ds_inputs,
                patch: ds_patch,
            },
            domain.options,
        )?;
        let vertices = region(self.max_points, ds.outputs * 16);
        let mut ds_code = ds.source;
        parameters(&mut ds_code);
        assembly.schedule(&mut ds_code, self.input_points);
        writeln!(
            ds_code,
            "alias DxControl = array<array<vec4<u32>, {}>, {}>;
            struct DxHullOutput {{ control: DxControl, values: array<vec4<u32>, {hs_patch}>, }}",
            hs.outputs, self.output_points
        )
        .unwrap();
        storage(
            &mut ds_code,
            HULL_OUTPUT,
            "dx_hull_output",
            "array<DxHullOutput>",
            false,
        );
        storage(&mut ds_code, POINTS, "dx_points", "array<vec4<u32>>", false);
        storage(&mut ds_code, RANGES, "dx_ranges", "array<vec4<u32>>", false);
        storage(
            &mut ds_code,
            SOURCE_SLOT,
            "dx_source",
            "array<DxOutput>",
            true,
        );
        if assembly.count_statistics {
            storage(
                &mut ds_code,
                STATISTICS_SLOT,
                "dx_stats",
                "array<atomic<u32>>",
                true,
            );
            ds_code.push_str(statistics::ADD_COUNTER);
        }
        entry(&mut ds_code, "domain_main", WORKGROUP);
        let statistics = if assembly.count_statistics {
            "if id.x == 0u { dx_add_count(5u, last.z + last.x); }"
        } else {
            ""
        };
        writeln!(
            ds_code,
            "if {active} == 0u {{ return; }}
            let last = dx_ranges[{active} - 1u];
            {statistics}
            if id.x >= last.z + last.x {{ return; }}
            let point = dx_points[id.x];
            let result = dx_hull_output[point.w];
            var cp: array<array<vec4<u32>, {ds_inputs}>, {}>;
            var values: array<vec4<u32>, {ds_patch}>;
            for (var n = 0u; n < {}u; n++) {{",
            self.output_points, self.output_points
        )
        .unwrap();
        link(
            &mut ds_code,
            &hull.ir.signatures.output,
            &domain.ir.signatures.input,
            "result.control[n]",
            "cp[n]",
        );
        ds_code.push_str("}\n");
        link(
            &mut ds_code,
            &hull.ir.signatures.patch_constant,
            &domain.ir.signatures.patch_constant,
            "result.values",
            "values",
        );
        writeln!(ds_code, "dx_source[id.x] = dx_run(vec4<u32>(point.xyz, 0u), vec4<u32>((dx_params.x + point.w) % {per_draw}), cp, values);\n}}").unwrap();
        let domain_program = program(
            ds_code,
            "domain_main",
            ds.locations,
            ds.counters,
            vec![
                params(),
                scratch(HULL_OUTPUT, hs_region),
                scratch(POINTS, points),
                scratch(RANGES, ranges),
                scratch(SOURCE_SLOT, vertices),
            ],
            self.max_points,
        );
        let output = Output {
            vertices,
            indices,
            points,
            ranges,
            arity: self.topology.vertices(),
            count,
            maximum: self.max_indices / self.topology.vertices(),
        };
        Ok((
            domain_program,
            ds.outputs,
            Programs {
                vertex,
                hull: hull_program,
                fixed,
                output,
            },
        ))
    }
}

fn registers(elements: &[SignatureElement]) -> u32 {
    elements
        .iter()
        .map(|element| element.register)
        .max()
        .unwrap_or(0)
        + 1
}

fn link(
    source: &mut String,
    producer: &[SignatureElement],
    consumer: &[SignatureElement],
    from: &str,
    to: &str,
) {
    for input in consumer {
        if let Some(output) = producer.iter().find(|output| {
            output.semantic.eq_ignore_ascii_case(&input.semantic)
                && output.semantic_index == input.semantic_index
        }) {
            for (out, input) in lanes(output).zip(lanes(input)) {
                writeln!(
                    source,
                    "{to}[{}][{}] = {from}[{}][{}];",
                    input.register, input.component, out.register, out.component
                )
                .unwrap();
            }
        }
    }
}

fn program(
    source: String,
    entry: &'static str,
    bindings: Vec<BindingLocation>,
    counters: Vec<(u32, u32)>,
    expansion: Vec<ExpansionBinding>,
    items_per_primitive: u32,
) -> ComputeProgram {
    ComputeProgram {
        source,
        entry,
        bindings,
        counters,
        expansion,
        workgroup_size: Some(WORKGROUP),
        items_per_primitive,
    }
}

pub(super) struct Programs {
    pub vertex: ComputeProgram,
    pub hull: ComputeProgram,
    pub(super) fixed: [ComputeProgram; 3],
    pub output: Output,
}

#[derive(Clone, Copy)]
pub(super) struct Output {
    pub vertices: usize,
    pub indices: usize,
    pub points: usize,
    pub ranges: usize,
    pub arity: u32,
    pub count: u32,
    pub maximum: u32,
}

impl Output {
    pub fn declare(self, source: &mut String, expansion: &mut Vec<ExpansionBinding>, active: &str) {
        storage(source, RANGES, "dx_ranges", "array<vec4<u32>>", false);
        writeln!(source, "fn dx_count() -> u32 {{ if {active} == 0u {{ return 0u; }} let last = dx_ranges[{active} - 1u]; let total = (last.w + last.y) / {}u; return min(dx_params.z, max(total, dx_params.w) - dx_params.w); }}", self.arity).unwrap();
        expansion.push(scratch(RANGES, self.ranges));
    }
}
