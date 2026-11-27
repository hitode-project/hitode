//! Schedule chunks of an indirect draw from the arguments at submission.

use super::*;
use crate::shader::wgsl::pulled::DRAW_ARGUMENTS_SLOT;

pub(super) const SLOT: u32 = 185;
const HEADER_WORDS: u32 = 6;

pub(super) fn insert(
    (gpu, compiled): (&Gpu, &OnceCell<ComputeProgram>),
    assembly: Assembly,
    input_vertices: u32,
    (arguments, strip, draw_state): (BufferRange, Option<BufferRange>, BufferWindow),
    regions: &mut Vec<ExpansionRegion>,
    programs: &mut Vec<ExpansionDispatch>,
    initialize: Option<usize>,
) -> Result<(), adv_device::gpu::Error> {
    let region = regions.len();
    let words = HEADER_WORDS + programs.len() as u32 * 3;
    regions.push(ExpansionRegion {
        bytes_per_primitive: 0,
        fixed_bytes: words * 4,
        element_stride: 4,
    });

    let schedule = compiled.get_or_init(|| {
        emit(
            assembly,
            input_vertices,
            strip.is_some(),
            region,
            programs,
            initialize,
        )
    });
    for (index, program) in programs.iter_mut().enumerate() {
        if initialize != Some(index) {
            let start = HEADER_WORDS + index as u32 * 3;
            program.indirect = Some((region, start * 4));
            program.expansion.push(scratch(SLOT, region));
        }
    }
    let mut bindings = SlotBindings::default();
    bind_draw_state(&mut bindings, draw_state);
    bind_storage(&mut bindings, DRAW_ARGUMENTS_SLOT, arguments);
    if let Some(strip) = strip {
        bind_storage(&mut bindings, super::strip::SLOT, strip);
    }
    programs.insert(0, schedule.dispatch(gpu, bindings)?);
    Ok(())
}

fn emit(
    assembly: Assembly,
    input_vertices: u32,
    strip: bool,
    region: usize,
    programs: &[ExpansionDispatch],
    initialize: Option<usize>,
) -> ComputeProgram {
    let mut source = String::new();
    parameters(&mut source);
    pulled::draw_state(&mut source, 0);
    storage(
        &mut source,
        DRAW_ARGUMENTS_SLOT,
        "dx_draw",
        "array<u32>",
        false,
    );
    storage(&mut source, SLOT, "dx_schedule", "array<u32>", true);
    let indexed = assembly.index.is_some();
    let base_vertex = if indexed { "dx_draw[word + 3u]" } else { "0u" };
    let instance_word = if indexed { 4 } else { 3 };
    let step = assembly.step;
    if strip {
        super::strip::declare(&mut source, false);
    }
    entry(&mut source, "schedule_main", 1);
    source.push_str("let word = dx_draw_state.argument_word;\n");
    if strip {
        source.push_str("let per_draw = dx_strip[0].x;\n");
    } else {
        writeln!(source,
            "let vertex_count = min(dx_draw[word], dx_draw_state.count);
             let per_draw = select(0u, 1u + (vertex_count - {input_vertices}u) / {step}u, vertex_count >= {input_vertices}u);"
        ).unwrap();
    }
    writeln!(
        source,
        "let instances = dx_draw_state.instances;
         let maximum = dx_draw_state.primitives * instances;
         let instance_count = min(dx_draw[word + 1u], instances);
         let total = min(maximum, per_draw * instance_count);
         let count = min(dx_params.y, max(total, dx_params.x) - dx_params.x);
         dx_schedule[0] = count;
         dx_schedule[1] = per_draw;
         dx_schedule[2] = dx_draw[word + 2u];
         dx_schedule[3] = {base_vertex};
         dx_schedule[4] = dx_draw[word + {instance_word}u];
         dx_schedule[5] = instance_count;",
    )
    .unwrap();

    for (index, program) in programs.iter().enumerate() {
        if initialize == Some(index) {
            continue;
        }
        let start = HEADER_WORDS + index as u32 * 3;
        let groups = match program.workgroup_size {
            Some(group) => format!(
                "(count * {}u + {}u) / {group}u",
                program.items_per_primitive,
                group - 1
            ),
            None => "u32(count != 0u)".into(),
        };
        writeln!(
            source,
            "dx_schedule[{start}u] = {groups};
            dx_schedule[{}u] = 1u; dx_schedule[{}u] = 1u;",
            start + 1,
            start + 2
        )
        .unwrap();
    }
    source.push_str("}\n");
    ComputeProgram {
        shader: OnceCell::new(),
        source,
        entry: "schedule_main",
        bindings: Vec::new(),
        counters: Vec::new(),
        expansion: vec![params(), scratch(SLOT, region)],
        workgroup_size: None,
        items_per_primitive: 1,
    }
}
