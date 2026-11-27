//! Compact the primitive starts in an indexed strip before running its VS.

use super::*;
use pulled::DRAW_ARGUMENTS_SLOT;

pub(super) const SLOT: u32 = 186;

pub(super) fn declare(source: &mut String, write: bool) {
    storage(source, SLOT, "dx_strip", "array<vec2<u32>>", write);
}

pub(super) fn prepare(
    gpu: &Gpu,
    compiled: &OnceCell<ComputeProgram>,
    assembly: Assembly,
    width: u32,
    state: &DrawState,
    indices: adv_device::gpu::BufferWindow,
    arguments: Option<&BufferRange>,
) -> Result<(BufferRange, ExpansionPrelude), adv_device::gpu::Error> {
    let table = gpu.expansion_storage(u64::from(1 + state.primitives_per_instance) * 8)?;
    let program = compiled.get_or_init(|| emit(assembly, width));
    let mut slots = SlotBindings::default();
    bind_draw_state(&mut slots, state.uniform.clone());
    bind_storage(&mut slots, INDEX_SLOT, indices);
    bind_storage(&mut slots, SLOT, table.clone());
    if let Some(arguments) = arguments {
        bind_storage(&mut slots, DRAW_ARGUMENTS_SLOT, arguments.clone());
    }
    Ok((
        table,
        ExpansionPrelude {
            dispatch: program.prepare(gpu)?,
            slots,
            groups: [1, 1, 1],
        },
    ))
}

fn emit(assembly: Assembly, width: u32) -> ComputeProgram {
    let step = assembly.step;
    let index = assembly.index.unwrap();
    let mut source = String::new();
    pulled::draw_state(&mut source, 0);
    storage(&mut source, INDEX_SLOT, "dx_indices", "array<u32>", false);
    declare(&mut source, true);
    if assembly.indirect {
        storage(
            &mut source,
            DRAW_ARGUMENTS_SLOT,
            "dx_draw",
            "array<u32>",
            false,
        );
    }
    entry(&mut source, "strip_main", 1);
    if assembly.indirect {
        source.push_str(
            "let word = dx_draw_state.argument_word;
             let count = min(dx_draw[word], dx_draw_state.count);
             let first = dx_draw[word + 2u];\n",
        );
    } else {
        source.push_str("let count = dx_draw_state.count; let first = dx_draw_state.first;\n");
    }
    source.push_str(
        "var run = 0u; var written = 0u; var vertices = 0u;
        for (var p = 0u; p < count; p++) {
            let at = first + p;\n",
    );
    index.read(&mut source, "at");
    let cut = if index.wide { u32::MAX } else { 0xffff };
    writeln!(
        source,
        "
        if vertex_id == {cut}u {{
            run = 0u;
        }} else {{
            run++; vertices++;
            if run >= {width}u && (run - {width}u) % {step}u == 0u {{
                dx_strip[1u + written] = vec2<u32>(at + 1u - {width}u, (run - {width}u) / {step}u);
                written++;
            }}
        }}
    }}
    dx_strip[0] = vec2<u32>(written, vertices);
}}"
    )
    .unwrap();
    ComputeProgram {
        shader: OnceCell::new(),
        source,
        entry: "strip_main",
        bindings: Vec::new(),
        counters: Vec::new(),
        expansion: Vec::new(),
        workgroup_size: None,
        items_per_primitive: 1,
    }
}
