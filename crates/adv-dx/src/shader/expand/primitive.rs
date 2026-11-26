use super::*;
use crate::shader::ir::passes::variant::lanes;
use crate::shader::ir::{SignatureElement, SystemValue};

pub(super) struct Emitter<'a> {
    plan: &'a Plan<'a>,
    input_ir: &'a Module,
    input_registers: u32,
    pub output_ir: &'a Module,
    pub outputs: u32,
    geometry: Option<pulled::Pulled>,
    topology: Topology,
    max_vertices: u32,
    instances: u32,
    count_expanded: bool,
    tessellation: Option<tessellation::Output>,
}

impl<'a> Emitter<'a> {
    pub fn new(
        plan: &'a Plan<'a>,
        input_ir: &'a Module,
        input_registers: u32,
        tessellation: Option<tessellation::Output>,
    ) -> Result<Self, Error> {
        let (output_ir, outputs, geometry, topology, max_vertices, instances, variant) =
            if let Some(shader) = plan.geometry {
                if shader.ir.stage != Stage::Geometry {
                    return Err(Error::Invalid("geometry expansion stage"));
                }
                let topology = declaration(shader.ir, Opcode::DclGsOutputPrimitiveTopology)
                    .ok_or(Error::Invalid("geometry output topology"))?
                    .controls;
                let max_vertices = declaration(shader.ir, Opcode::DclMaxOutputVertexCount)
                    .and_then(|d| d.data.first())
                    .copied()
                    .filter(|&count| count != 0)
                    .ok_or(Error::Invalid("geometry output vertex limit"))?;
                let instances = declaration(shader.ir, Opcode::DclGsInstanceCount)
                    .and_then(|d| d.data.first())
                    .copied()
                    .unwrap_or(1);
                let pulled = pulled::emit(
                    shader.ir,
                    shader.variant,
                    pulled::Invocation::Geometry {
                        inputs: plan.input_vertices,
                        max_vertices,
                    },
                    shader.options,
                )?;
                (
                    shader.ir,
                    pulled.outputs,
                    Some(pulled),
                    Topology::from_geometry(decode_gs_output_primitive_topology(topology))?,
                    max_vertices,
                    instances,
                    shader.variant,
                )
            } else {
                let shader = match plan.source {
                    Source::Vertex(shader)
                    | Source::InputAssembler { shader, .. }
                    | Source::Domain { shader, .. } => shader,
                    Source::Tessellated { domain, .. } => *domain,
                };
                (
                    input_ir,
                    input_registers,
                    None,
                    plan.topology,
                    plan.topology.vertices(),
                    1,
                    shader.variant,
                )
            };
        Ok(Self {
            plan,
            input_ir,
            input_registers,
            output_ir,
            outputs,
            geometry,
            topology,
            max_vertices,
            instances,
            count_expanded: variant.count_expanded(),
            tessellation,
        })
    }

    pub fn items(&self) -> u32 {
        self.tessellation
            .map_or(1, |tessellation| tessellation.maximum)
    }

    fn count(&self) -> &'static str {
        if self.tessellation.is_some() {
            "dx_count()"
        } else {
            self.plan
                .assembly()
                .map_or("dx_params.y", |(assembly, width)| assembly.active(width))
        }
    }

    pub fn wireframe(&self) -> bool {
        self.topology == Topology::Triangle && self.raster_variant().is_some_and(Variant::wireframe)
    }

    fn raster_variant(&self) -> Option<&Variant> {
        self.plan.raster.as_ref()?.bins.first().copied()
    }

    pub fn raster_topology(&self) -> Topology {
        if self.wireframe() {
            Topology::Line
        } else {
            self.topology
        }
    }

    pub fn list_slots(&self) -> u32 {
        self.topology.fixed_slots(self.max_vertices) * self.instances
    }

    pub fn raster_slots(&self) -> u32 {
        if self.wireframe() {
            self.list_slots() * 2
        } else {
            self.list_slots()
        }
    }

    pub fn emit(&self, raster: Option<(usize, usize)>, stream: Option<usize>) -> ComputeProgram {
        let mut source = self
            .geometry
            .as_ref()
            .map(|g| g.source.clone())
            .unwrap_or_else(|| format!("alias DxOutput = array<vec4<u32>, {}>;\n", self.outputs));
        let mut expansion = vec![params()];
        let tessellated = self.tessellation;
        let domain = matches!(self.plan.source, Source::Domain { .. });
        if domain || tessellated.is_some() {
            storage(&mut source, INDEX_SLOT, "dx_indices", "array<u32>", false);
        }
        parameters(&mut source);
        if let Some((assembly, width)) = self.plan.assembly() {
            assembly.schedule(&mut source, width);
        }
        if let Some(output) = tessellated {
            expansion.extend([
                scratch(SOURCE_SLOT, output.vertices),
                scratch(INDEX_SLOT, output.indices),
            ]);
            output.declare(
                &mut source,
                &mut expansion,
                self.plan
                    .assembly()
                    .map(|(assembly, width)| assembly.active(width))
                    .unwrap(),
            );
            if self.geometry.is_some() {
                storage(
                    &mut source,
                    tessellation::POINTS,
                    "dx_points",
                    "array<vec4<u32>>",
                    false,
                );
                expansion.push(scratch(tessellation::POINTS, output.points));
            }
        } else if !domain {
            expansion.push(scratch(SOURCE_SLOT, 0));
        }
        let input_type = if self.geometry.is_some() {
            writeln!(
                source,
                "alias DxInput = array<vec4<u32>, {}>;",
                self.input_registers
            )
            .unwrap();
            "DxInput"
        } else {
            "DxOutput"
        };
        storage(
            &mut source,
            SOURCE_SLOT,
            "dx_source",
            &format!("array<{input_type}>"),
            false,
        );
        let input = if tessellated.is_some() {
            format!(
                "dx_indices[primitive * {}u + corner]",
                self.plan.input_vertices
            )
        } else if domain {
            format!(
                "dx_indices[(dx_params.x + primitive) * {}u + corner]",
                self.plan.input_vertices
            )
        } else {
            format!("primitive * {}u + corner", self.plan.input_vertices)
        };
        writeln!(source, "fn dx_input(primitive: u32, corner: u32) -> {input_type} {{ return dx_source[{input}]; }}").unwrap();
        writeln!(
            source,
            "alias DxPrimitive = array<DxOutput, {}>;",
            self.topology.vertices()
        )
        .unwrap();
        if raster.is_some() || stream.is_some() {
            writeln!(
                source,
                "alias DxExpanded = array<vec4<u32>, {}>;",
                self.outputs + 1
            )
            .unwrap();
            writeln!(
                source,
                "fn dx_record(value: DxOutput, tag: vec4<u32>) -> DxExpanded {{
                var out: DxExpanded;
                for (var r = 0u; r < {0}u; r += 1u) {{ out[r] = value[r]; }}
                out[{0}] = tag;
                return out;
            }}",
                self.outputs
            )
            .unwrap();
        }
        if let Some((vertices, args)) = raster {
            storage(
                &mut source,
                RASTER_SLOT,
                "dx_raster",
                "array<DxExpanded>",
                true,
            );
            storage(
                &mut source,
                ARGUMENTS_SLOT,
                "dx_args",
                "array<atomic<u32>>",
                true,
            );
            expansion.extend([
                scratch(RASTER_SLOT, vertices),
                scratch(ARGUMENTS_SLOT, args),
            ]);
        }
        if let Some(region) = stream {
            storage(
                &mut source,
                STREAM_SLOT,
                "dx_stream",
                "array<DxExpanded>",
                true,
            );
            expansion.push(scratch(STREAM_SLOT, region));
        }
        if self.count_expanded || self.raster_variant().is_some_and(Variant::count_clipped) {
            storage(
                &mut source,
                STATISTICS_SLOT,
                "dx_stats",
                "array<atomic<u32>>",
                true,
            );
            source.push_str(statistics::ADD_COUNTER);
        }
        if let Some(raster) = self.plan.raster.as_ref().filter(|_| raster.is_some()) {
            self.raster_functions(&mut source, raster);
        }
        self.primitive_output(&mut source, raster.is_some(), stream.is_some());

        entry(&mut source, "primitive_main", WORKGROUP);
        writeln!(
            source,
            "let primitive = id.x; if primitive >= {} {{ return; }}",
            self.count()
        )
        .unwrap();
        if raster.is_some() {
            writeln!(
                source,
                "let raster_at = primitive * {}u;",
                self.raster_slots()
            )
            .unwrap();
            let position = self
                .system(
                    SystemValue::Position,
                    self.plan.raster.as_ref().unwrap().stream,
                )
                .unwrap_or((0, 0))
                .0;
            writeln!(source, "var placeholder: DxOutput;
                placeholder[{position}] = bitcast<vec4<u32>>(vec4<f32>(2.0, 2.0, 2.0, 1.0));
                for (var j = 0u; j < {}u; j += 1u) {{
                    dx_raster[raster_at + j] = dx_record(placeholder, vec4<u32>(0xffffffffu, 0xffffffffu, 0u, 0u));
                }}", self.raster_slots()).unwrap();
        }
        if stream.is_some() {
            writeln!(
                source,
                "let stream_at = primitive * {}u;
                for (var j = 0u; j < {}u; j += 1u) {{
                    dx_stream[stream_at + j][{}].w = 0u;
                }}",
                self.list_slots(),
                self.list_slots(),
                self.outputs
            )
            .unwrap();
        }
        if let Some(geometry) = &self.geometry {
            self.geometry_entry(&mut source, geometry);
        } else {
            let arity = self.topology.vertices();
            let values = (0..arity)
                .map(|j| {
                    let corner = match self.plan.input_vertices {
                        4 if arity == 2 => j + 1,
                        6 if arity == 3 => j * 2,
                        _ => j,
                    };
                    format!("dx_input(primitive, {corner}u)")
                })
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                source,
                "dx_output(primitive, 0u, 0u, DxPrimitive({values}));"
            )
            .unwrap();
        }
        source.push_str("}\n");
        ComputeProgram {
            source,
            entry: "primitive_main",
            bindings: self
                .geometry
                .as_ref()
                .map_or_else(Vec::new, |g| g.locations.clone()),
            counters: self
                .geometry
                .as_ref()
                .map_or_else(Vec::new, |g| g.counters.clone()),
            expansion,
            workgroup_size: Some(WORKGROUP),
            items_per_primitive: self.items(),
        }
    }

    fn system(&self, value: SystemValue, stream: u32) -> Option<(u32, u8)> {
        self.output_ir
            .signatures
            .output
            .iter()
            .find(|e| e.system_value == value && e.stream == stream)
            .map(|e| (e.register, e.mask.trailing_zeros() as u8))
    }

    fn geometry_entry(&self, source: &mut String, geometry: &pulled::Pulled) {
        let inputs = geometry.inputs;
        let count = self.plan.input_vertices;
        writeln!(
            source,
            "var input: array<array<vec4<u32>, {inputs}>, {count}>;
            for (var vi = 0u; vi < {count}u; vi += 1u) {{
                let v = dx_input(primitive, vi);"
        )
        .unwrap();
        for input in &self.output_ir.signatures.input {
            if let Some(output) = self.input_ir.signatures.output.iter().find(|output| {
                output.semantic.eq_ignore_ascii_case(&input.semantic)
                    && output.semantic_index == input.semantic_index
            }) {
                for (from, to) in lanes(output).zip(lanes(input)) {
                    writeln!(
                        source,
                        "input[vi][{}][{}] = v[{}][{}];",
                        to.register, to.component, from.register, from.component
                    )
                    .unwrap();
                }
            }
        }
        let arity = self.topology.vertices();
        let per_instance = self.topology.fixed_slots(self.max_vertices);
        let primitive_id = if let Some(output) = self.tessellation {
            let (assembly, _) = self.plan.assembly().unwrap();
            let count = if assembly.indirect.is_some() {
                "dx_schedule[1]".into()
            } else {
                format!("{}u", output.count)
            };
            format!(
                "(dx_params.x + dx_points[dx_indices[primitive * {}u]].w) % {count}",
                output.arity
            )
        } else if let Source::InputAssembler { assembly, .. } = self.plan.source {
            let count = if assembly.indirect.is_some() {
                "dx_schedule[1]".into()
            } else if assembly.strip(self.plan.input_vertices) {
                "dx_strip[0].x".into()
            } else {
                format!("{}u", assembly.primitives(self.plan.input_vertices))
            };
            format!("(dx_params.x + primitive) % {count}")
        } else {
            "dx_params.x + primitive".into()
        };

        writeln!(
            source,
            "}}
            for (var instance = 0u; instance < {}u; instance += 1u) {{
                dx_run(input, vec4<u32>({primitive_id}), vec4<u32>(instance));
                var strip: array<u32, 4>;
                var old: array<vec2<u32>, 4>;
                var emitted = 0u;
                for (var at = 0u; at < dx_emitted_count; at += 1u) {{
                    let stream = dx_tags[at];
                    let n = strip[stream];
                    if n >= {}u {{",
            self.instances,
            arity - 1
        )
        .unwrap();
        match self.topology {
            Topology::Point => source.push_str("let list = DxPrimitive(dx_emitted[at]);\n"),
            Topology::Line => source
                .push_str("let list = DxPrimitive(dx_emitted[old[stream].y], dx_emitted[at]);\n"),
            Topology::Triangle => source.push_str(
                "let odd = (n & 1u) == 1u;
                let list = DxPrimitive(
                    dx_emitted[old[stream].x],
                    dx_emitted[select(old[stream].y, at, odd)],
                    dx_emitted[select(at, old[stream].y, odd)]
                );\n",
            ),
        }
        writeln!(
            source,
            "dx_output(primitive, instance * {per_instance}u + emitted, stream, list);
                    emitted += {arity}u;
                }}
                old[stream] = vec2<u32>(old[stream].y, at);
                strip[stream] = select(n + 1u, 0u, dx_end[at]);
            }}
        }}"
        )
        .unwrap();
    }

    fn raster_functions(&self, source: &mut String, raster: &Raster<'_>) {
        let arity = self.topology.vertices();
        let position = self
            .system(SystemValue::Position, raster.stream)
            .unwrap_or((0, 0))
            .0;
        writeln!(source, "fn dx_positions(list: DxPrimitive) -> array<vec4<f32>, {arity}> {{
                var position: array<vec4<f32>, {arity}>;
                for (var i = 0u; i < {arity}u; i += 1u) {{ position[i] = bitcast<vec4<f32>>(list[i][{position}]); }}
                return position;
            }}").unwrap();
        let variant = self.raster_variant().unwrap();
        if variant.count_clipped() {
            let mut planes = vec![
                "p.w <= 0.0",
                "p.x < -p.w",
                "p.x > p.w",
                "p.y < -p.w",
                "p.y > p.w",
            ];
            if raster.depth_clip {
                planes.extend(["p.z < 0.0", "p.z > p.w"]);
            }
            writeln!(
                source,
                "fn dx_clip(list: DxPrimitive) -> bool {{
                let position = dx_positions(list);"
            )
            .unwrap();
            for (index, plane) in planes.into_iter().enumerate() {
                writeln!(
                    source,
                    "var plane{index} = true;
                    for (var i = 0u; i < {arity}u; i += 1u) {{
                        let p = position[i];
                        plane{index} = plane{index} && ({plane});
                    }}
                    if plane{index} {{ return true; }}"
                )
                .unwrap();
            }
            self.distances(source, SystemValue::ClipDistance, raster.stream);
            source.push_str("return false;\n}\n");
        }
        if variant.cull_distances() {
            source.push_str("fn dx_cull(list: DxPrimitive) -> bool {\n");
            self.distances(source, SystemValue::CullDistance, raster.stream);
            source.push_str("return false;\n}\n");
        }
        if self.wireframe() && raster.cull != Cull::None {
            writeln!(
                source,
                "fn dx_face_cull(list: DxPrimitive) -> bool {{
                let p = dx_positions(list);
                let a = p[0].xy / p[0].w;
                let b = p[1].xy / p[1].w - a;
                let c = p[2].xy / p[2].w - a;
                let ccw = b.x * c.y > b.y * c.x;
                return ccw == {};
            }}",
                raster.front_ccw == (raster.cull == Cull::Front)
            )
            .unwrap();
        }
    }

    fn distances(&self, source: &mut String, system: SystemValue, stream: u32) {
        let arity = self.topology.vertices();
        let elements = self
            .output_ir
            .signatures
            .output
            .iter()
            .filter(|e| e.system_value == system && e.stream == stream);
        for SignatureElement { register, mask, .. } in elements {
            for lane in (0..4).filter(|lane| mask & (1 << lane) != 0) {
                writeln!(
                    source,
                    "{{ var outside = true;
                    for (var i = 0u; i < {arity}u; i += 1u) {{
                        outside = outside && bitcast<f32>(list[i][{register}][{lane}]) < 0.0;
                    }}
                    if outside {{ return true; }}
                }}"
                )
                .unwrap();
            }
        }
    }

    fn primitive_output(&self, source: &mut String, rasterized: bool, stream_output: bool) {
        let arity = self.topology.vertices();
        writeln!(
            source,
            "fn dx_output(primitive: u32, slot: u32, stream: u32, list: DxPrimitive) {{"
        )
        .unwrap();
        if self.geometry.is_some() && self.count_expanded {
            source.push_str("dx_add_counter(1u);\n");
        }
        if stream_output {
            writeln!(source, "for (var j = 0u; j < {arity}u; j += 1u) {{
                dx_stream[primitive * {}u + slot + j] = dx_record(list[j], vec4<u32>(0u, 0u, stream, 1u));
            }}", self.list_slots()).unwrap();
        }
        if let Some(raster) = self.plan.raster.as_ref().filter(|_| rasterized) {
            writeln!(source, "if stream != {}u {{ return; }}", raster.stream).unwrap();
            if self.count_expanded {
                source.push_str("dx_add_counter(2u);\n");
            }
            let variant = self.raster_variant().unwrap();
            if variant.cull_distances() {
                source.push_str("if dx_cull(list) { return; }\n");
            }
            if variant.count_clipped() {
                source.push_str("if !dx_clip(list) { dx_add_counter(3u); }\n");
            }
            if self.wireframe() && raster.cull != Cull::None {
                source.push_str("if dx_face_cull(list) { return; }\n");
            }
            let read_index = |value| {
                self.system(value, raster.stream).map_or_else(
                    || "0u".into(),
                    |(register, lane)| format!("list[0][{register}][{lane}]"),
                )
            };
            writeln!(
                source,
                "let viewport = {}; let layer = {};",
                read_index(SystemValue::ViewportArrayIndex),
                read_index(SystemValue::RenderTargetArrayIndex)
            )
            .unwrap();
            for (index, variant) in raster.bins.iter().enumerate() {
                let bin = variant.bin();
                let condition = if variant.binned() {
                    format!(
                        "viewport == {}u && layer == {}u",
                        bin.viewport, bin.first_layer
                    )
                } else {
                    "true".into()
                };
                writeln!(
                    source,
                    "if {condition} {{ atomicStore(&dx_args[{}u], {} * {}u); }}",
                    index * 4,
                    self.count(),
                    self.raster_slots()
                )
                .unwrap();
            }
            let slots = self.raster_slots();
            if self.wireframe() {
                writeln!(source, "let start = primitive * {slots}u + slot * 2u;
                    for (var edge = 0u; edge < 3u; edge += 1u) {{
                        dx_raster[start + edge * 2u] = dx_record(list[edge], vec4<u32>(viewport, layer, 0u, 1u));
                        dx_raster[start + edge * 2u + 1u] = dx_record(list[(edge + 1u) % 3u], vec4<u32>(viewport, layer, 0u, 1u));
                    }}").unwrap();
            } else {
                writeln!(source, "for (var j = 0u; j < {arity}u; j += 1u) {{
                    dx_raster[primitive * {slots}u + slot + j] = dx_record(list[j], vec4<u32>(viewport, layer, 0u, 1u));
                }}").unwrap();
            }
        }
        source.push_str("}\n");
    }

    pub fn stream_output(&self, stream: usize, scan: usize) -> (ComputeProgram, ComputeProgram) {
        let target_count = self.plan.stream_output.len();
        let args = |write: bool| {
            let mut source = String::new();
            parameters(&mut source);
            if let Some((assembly, width)) = self.plan.assembly() {
                assembly.schedule(&mut source, width);
            }
            storage(
                &mut source,
                STREAM_SLOT,
                "dx_stream",
                &format!("array<array<vec4<u32>, {}>>", self.outputs + 1),
                false,
            );
            storage(&mut source, SCAN_SLOT, "dx_offsets", "array<u32>", !write);
            for i in 0..target_count {
                storage(
                    &mut source,
                    TARGET_BASE + i as u32,
                    &format!("target{i}"),
                    "array<u32>",
                    write,
                );
            }
            source
        };
        let mut expansion = vec![
            params(),
            scratch(STREAM_SLOT, stream),
            scratch(SCAN_SLOT, scan),
        ];
        let mut prefix = args(false);
        for i in 0..target_count {
            storage(
                &mut prefix,
                FILLED_BASE + i as u32,
                &format!("filled{i}"),
                "array<u32>",
                true,
            );
        }
        if let Some(output) = self.tessellation {
            output.declare(
                &mut prefix,
                &mut expansion,
                self.plan
                    .assembly()
                    .map(|(assembly, width)| assembly.active(width))
                    .unwrap(),
            );
        }
        if self.plan.stream_statistics {
            storage(
                &mut prefix,
                STREAM_STATISTICS,
                "dx_so_stats",
                "array<u32>",
                true,
            );
            prefix.push_str(
                "fn dx_increment(at: u32) {
                dx_so_stats[at] += 1u;
                if dx_so_stats[at] == 0u { dx_so_stats[at + 1u] += 1u; }
            }\n",
            );
        }
        entry(&mut prefix, "prefix_main", 1);
        let arity = self.topology.vertices();
        let slots = self.list_slots();
        writeln!(
            prefix,
            "for (var at = 0u; at < {} * {slots}u; at += {arity}u) {{
            let tag = dx_stream[at][{}];",
            self.count(),
            self.outputs
        )
        .unwrap();
        for i in 0..target_count {
            writeln!(
                prefix,
                "dx_offsets[at * {target_count}u + {i}u] = 0xffffffffu;"
            )
            .unwrap();
        }
        if self.plan.stream_statistics {
            prefix.push_str("if tag.w != 0u { dx_increment(4u * tag.z + 2u); }\n");
        }
        let mut streams: Vec<_> = self
            .plan
            .stream_output
            .iter()
            .map(|target| target.stream)
            .collect();
        streams.sort_unstable();
        streams.dedup();
        for stream in streams {
            let targets: Vec<_> = self
                .plan
                .stream_output
                .iter()
                .enumerate()
                .filter(|(_, target)| target.stream == stream)
                .collect();
            let fits: Vec<_> = targets
                .iter()
                .map(|(i, target)| {
                    format!(
                        "filled{i}[0] / 4u + {}u <= arrayLength(&target{i})",
                        arity as usize * target.elements.len(),
                    )
                })
                .collect();
            writeln!(
                prefix,
                "if tag.w != 0u && tag.z == {stream}u && {} {{",
                fits.join(" && ")
            )
            .unwrap();
            if self.plan.stream_statistics {
                writeln!(prefix, "dx_increment(4u * {stream}u);").unwrap();
            }
            for (i, target) in targets {
                let stride = target.elements.len();
                writeln!(
                    prefix,
                    "dx_offsets[at * {target_count}u + {i}u] = filled{i}[0] / 4u;
                    filled{i}[0] += {}u;",
                    4 * stride * arity as usize
                )
                .unwrap();
            }
            prefix.push_str("}\n");
        }
        prefix.push_str("}\n}\n");

        let mut write = args(true);
        let mut write_expansion = expansion.clone();
        if let Some(output) = self.tessellation {
            write_expansion.pop();
            output.declare(
                &mut write,
                &mut write_expansion,
                self.plan
                    .assembly()
                    .map(|(assembly, width)| assembly.active(width))
                    .unwrap(),
            );
        }
        entry(&mut write, "stream_main", WORKGROUP);
        writeln!(
            write,
            "let primitive = id.x; if primitive >= {} {{ return; }}
            for (var j = 0u; j < {slots}u; j += {arity}u) {{
                let at = primitive * {slots}u + j;",
            self.count()
        )
        .unwrap();
        for (slot, target) in self.plan.stream_output.iter().enumerate() {
            let stride = target.elements.len();
            writeln!(
                write,
                "{{ let offset = dx_offsets[at * {target_count}u + {slot}u];
                if offset != 0xffffffffu {{
                    for (var vi = 0u; vi < {arity}u; vi += 1u) {{
                        let v = dx_stream[at + vi];"
            )
            .unwrap();
            for (i, element) in target.elements.iter().enumerate() {
                if let Some((register, lane)) = element {
                    writeln!(
                        write,
                        "target{slot}[offset + vi * {stride}u + {i}u] = v[{register}][{lane}];"
                    )
                    .unwrap();
                }
            }
            write.push_str("}\n}\n}\n");
        }
        write.push_str("}\n}\n");
        (
            ComputeProgram {
                source: prefix,
                entry: "prefix_main",
                bindings: Vec::new(),
                counters: Vec::new(),
                expansion: expansion.clone(),
                workgroup_size: None,
                items_per_primitive: 1,
            },
            ComputeProgram {
                source: write,
                entry: "stream_main",
                bindings: Vec::new(),
                counters: Vec::new(),
                expansion: write_expansion,
                workgroup_size: Some(WORKGROUP),
                items_per_primitive: self.items(),
            },
        )
    }
}

pub(super) fn initialize(bins: usize, argument_region: usize) -> ComputeProgram {
    let mut source = String::new();
    storage(&mut source, ARGUMENTS_SLOT, "dx_args", "array<u32>", true);
    entry(&mut source, "initialize_main", 1);
    writeln!(
        source,
        "for (var i = 0u; i < {bins}u; i += 1u) {{
        dx_args[i * 4u] = 0u;
        dx_args[i * 4u + 1u] = 1u;
        dx_args[i * 4u + 2u] = 0u;
        dx_args[i * 4u + 3u] = 0u;
    }}\n}}"
    )
    .unwrap();
    ComputeProgram {
        source,
        entry: "initialize_main",
        bindings: Vec::new(),
        counters: Vec::new(),
        expansion: vec![scratch(ARGUMENTS_SLOT, argument_region)],
        workgroup_size: None,
        items_per_primitive: 1,
    }
}
