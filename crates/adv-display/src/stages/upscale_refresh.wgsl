// Refresh: one workgroup per listed last-pass cell of the output being
// written (HOST-VIEW-13). The other output holds the previous frame: a
// rebuilt cell copies it at an offset, a cell written last frame and not now
// copies it in place, and a cached colour fills its 2x2 pattern.
@group(1) @binding(0) var previous_output: texture_2d<f32>;
@group(1) @binding(1) var constant_output: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> entries: array<vec2<u32>>;
@group(3) @binding(1) var<storage, read> frame: array<u32>;
@group(3) @binding(2) var<storage, read> state: array<u32>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var output: texture_storage_2d<rgba8unorm, write>;

fn offset_entry(k: u32) -> vec4<i32> {
    let at = FRAME_OFFSETS + k * 4u;
    return bitcast<vec4<i32>>(vec4(frame[at], frame[at + 1u], frame[at + 2u], frame[at + 3u]));
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let entry = entries[group.x];
    let cell = vec2(entry.x & 65535u, entry.x >> 16u);
    let op = entry.y;
    let side = metadata.shape.last_unit * 2u;
    let limit = metadata.shape.size * 2u;
    var offset = vec2(0);
    if op >= REFRESH_REBUILD && op < REFRESH_CONSTANT {
        let k = op - REFRESH_REBUILD;
        if k == 0u {
            offset = vec2(i32(state[MOTION_OFFSET]), i32(state[MOTION_OFFSET + 1u]));
        } else {
            offset = offset_entry(k).xy;
        }
    }
    let slot = op & ~REFRESH_CONSTANT;
    let base = vec2(slot % 64u, slot / 64u) * 2u;
    for (var i = lane; i < side * side; i += 64u) {
        let pixel = cell * side + vec2(i % side, i / side);
        if any(pixel >= limit) { continue; }
        var value: vec4<f32>;
        if op >= REFRESH_CONSTANT {
            value = textureLoad(constant_output, vec2<i32>(base + (pixel & vec2(1u))), 0);
        } else {
            value = textureLoad(previous_output, vec2<i32>(pixel) - offset * 2, 0);
        }
        textureStore(output, vec2<i32>(pixel), value);
    }
}
