// Copies: one workgroup per output cell reused at an offset (HOST-VIEW-13).
// Gather reads the previous output at twice the offset into scratch, at the
// cell's own place, before anything writes the output; scatter then writes
// the cell back. Copied and computed cells are disjoint.
@group(1) @binding(0) var source: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> entries: array<u32>;
@group(3) @binding(1) var<storage, read> frame: array<u32>;
@group(3) @binding(2) var<storage, read> state: array<u32>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var destination: texture_storage_2d<rgba8unorm, write>;

fn copy(group: u32, lane: u32, gather: bool) {
    let shape = metadata.shape;
    let entry = entries[group];
    let cell = vec2(entry & 65535u, (entry >> 16u) & COPY_ROWS);
    var offset = vec2(0);
    if gather {
        let hypotheses = shape.hypotheses + (frame[FRAME_EPOCH] % 2u) * HYPOTHESIS_WORDS;
        offset = 2 * hypothesis(state[hypotheses + 2u + (entry >> COPY_SHIFT)]);
    }
    let side = metadata.grids[shape.passes - 1u].unit * 2u;
    let limit = shape.size * 2u;
    for (var i = lane; i < side * side; i += 64u) {
        let pixel = cell * side + vec2(i % side, i / side);
        if any(pixel >= limit) { continue; }
        textureStore(destination, vec2<i32>(pixel), textureLoad(source, vec2<i32>(pixel) - offset, 0));
    }
}

@compute @workgroup_size(64)
fn gather(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    copy(group.x, lane, true);
}

@compute @workgroup_size(64)
fn scatter(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    copy(group.x, lane, false);
}
