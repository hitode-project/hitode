//! Schedule chunks of an indirect draw from the arguments at submission.

use super::*;
use crate::shader::wgsl::pulled::DRAW_ARGUMENTS_SLOT;

pub(super) const SLOT: u32 = 185;
const HEADER_WORDS: u32 = 5;

#[derive(Clone, Copy)]
pub struct Indirect {
    pub first_word: u32,
    pub instances: u32,
}

pub(super) fn insert(
    gpu: &Gpu,
    assembly: Assembly,
    input_vertices: u32,
    (arguments, strip): (BufferRange, Option<BufferRange>),
    regions: &mut Vec<ExpansionRegion>,
    programs: &mut Vec<ExpansionDispatch>,
    first: usize,
) -> Result<(), adv_device::gpu::Error> {
    let Indirect {
        first_word: word,
        instances,
    } = assembly.indirect.unwrap();
    let region = regions.len();
    let words = HEADER_WORDS + (programs.len() - first) as u32 * 3;
    regions.push(ExpansionRegion {
        bytes_per_primitive: 0,
        fixed_bytes: words * 4,
        element_stride: 4,
    });

    let mut source = String::new();
    parameters(&mut source);
    storage(
        &mut source,
        DRAW_ARGUMENTS_SLOT,
        "dx_draw",
        "array<u32>",
        false,
    );
    storage(&mut source, SLOT, "dx_schedule", "array<u32>", true);
    let indexed = assembly.index.is_some();
    let base_vertex = if indexed {
        format!("dx_draw[{}u]", word + 3)
    } else {
        "0u".into()
    };
    let instance_word = word + if indexed { 4 } else { 3 };
    let Assembly { count, step, .. } = assembly;
    let maximum = assembly.primitives(input_vertices) * instances;
    if strip.is_some() {
        super::strip::declare(&mut source, false);
    }
    entry(&mut source, "schedule_main", 1);
    if strip.is_some() {
        source.push_str("let per_draw = dx_strip[0].x;\n");
    } else {
        writeln!(source,
            "let vertex_count = min(dx_draw[{word}u], {count}u);
             let per_draw = select(0u, 1u + (vertex_count - {input_vertices}u) / {step}u, vertex_count >= {input_vertices}u);"
        ).unwrap();
    }
    writeln!(
        source,
        "         let total = min({maximum}u, per_draw * min(dx_draw[{}u], {instances}u));
         let count = min(dx_params.y, max(total, dx_params.x) - dx_params.x);
         dx_schedule[0] = count;
         dx_schedule[1] = per_draw;
         dx_schedule[2] = dx_draw[{}u];
         dx_schedule[3] = {base_vertex};
         dx_schedule[4] = dx_draw[{instance_word}u];",
        word + 1,
        word + 2
    )
    .unwrap();

    for (index, program) in programs[first..].iter_mut().enumerate() {
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
        program.indirect = Some((region, start * 4));
        program.expansion.push(scratch(SLOT, region));
    }
    source.push_str("}\n");
    let schedule = ComputeProgram {
        source,
        entry: "schedule_main",
        bindings: Vec::new(),
        counters: Vec::new(),
        expansion: vec![params(), scratch(SLOT, region)],
        workgroup_size: None,
        items_per_primitive: 1,
    };
    let mut bindings = SlotBindings::default();
    bind_storage(&mut bindings, DRAW_ARGUMENTS_SLOT, arguments);
    if let Some(strip) = strip {
        bind_storage(&mut bindings, super::strip::SLOT, strip);
    }
    programs.insert(first, schedule.dispatch(gpu, bindings)?);
    Ok(())
}
