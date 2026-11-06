// Compare: one workgroup per source cell near the change. Composition already
// wrote the whole frame into `current`; `previous` is the last encoded input.
// A flagged cell, which the producer may have changed, stamps itself with the
// frame's epoch when it did. A cell checks a hypothesis only when a cell
// sharing an output field with it voted for it, and records those its pixels
// match (HOST-VIEW-13). Nothing is cleared per frame.
@group(1) @binding(0) var current: texture_2d<f32>;
@group(1) @binding(1) var previous: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read_write> facts: array<Fact>;
@group(3) @binding(2) var<storage, read_write> state: array<u32>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var<storage, read> anchors: array<u32>;
var<workgroup> changed: atomic<u32>;
var<workgroup> evidence: atomic<u32>;
var<workgroup> evident: u32;
var<workgroup> rejected: atomic<u32>;
var<workgroup> proposed: atomic<u32>;
var<workgroup> keys: array<u32, VOTES>;
var<workgroup> counts: array<u32, VOTES>;
var<workgroup> chosen: array<u32, HYPOTHESES>;
var<workgroup> kept: u32;

// This frame's hypotheses: zero, the offsets that reused a cell last frame,
// then the most-voted offsets with two votes or more. One lane per vote slot.
fn choose(lane: u32) -> u32 {
    let shape = metadata.shape;
    let last = shape.hypotheses + ((frame[FRAME_EPOCH] + 1u) % 2u) * HYPOTHESIS_WORDS;
    if lane == 0u {
        chosen[0] = 0u;
        var n = 1u;
        let won = state[last + 1u];
        for (var i = 1u; i < min(state[last], HYPOTHESES); i++) {
            if (won & (1u << i)) != 0u {
                chosen[n] = state[last + 2u + i];
                n++;
            }
        }
        kept = n;
    }
    let n = workgroupUniformLoad(&kept);
    let key = state[shape.votes + lane];
    var votes = state[shape.votes + VOTES + lane];
    for (var i = 0u; i < n; i++) {
        if chosen[i] == key { votes = 0u; }
    }
    // A single vote is most likely a hash collision.
    if key == 0u || votes < 2u { votes = 0u; }
    keys[lane] = key;
    counts[lane] = votes;
    workgroupBarrier();
    if votes != 0u {
        var rank = 0u;
        for (var t = 0u; t < VOTES; t++) {
            if counts[t] > votes || (counts[t] == votes && keys[t] < key) { rank++; }
        }
        if rank < min(PROPOSALS, HYPOTHESES - n) {
            chosen[n + rank] = key;
            atomicAdd(&proposed, 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u { kept = n + atomicLoad(&proposed); }
    return workgroupUniformLoad(&kept);
}

// One workgroup per frame writes the hypotheses where compare, bookkeeping,
// copies and the next frame read them.
@compute @workgroup_size(64)
fn hypotheses(@builtin(local_invocation_index) lane: u32) {
    let n = choose(lane);
    if lane == 0u {
        let block = metadata.shape.hypotheses + (frame[FRAME_EPOCH] % 2u) * HYPOTHESIS_WORDS;
        state[block] = n;
        state[block + 1u] = 0u;
        for (var i = 0u; i < n; i++) { state[block + 2u + i] = chosen[i]; }
    }
}

@compute @workgroup_size(64)
fn compare(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let index = group.y * 65535u + group.x;
    if index >= frame[FRAME_COMPARED] { return; }
    let shape = metadata.shape;
    let epoch = frame[FRAME_EPOCH];
    let block = shape.hypotheses + (epoch % 2u) * HYPOTHESIS_WORDS;
    let n = state[block];
    if lane < n { chosen[lane] = state[block + 2u + lane]; }
    workgroupBarrier();
    let entry = listed(FRAME_LIST, index);
    let whole = frame[FRAME_LIST] == WHOLE && frame[FRAME_EXACT] == 0u;
    let flagged = whole || (entry & COMPARE_FLAG) != 0u;
    let cell = vec2(entry & 65535u, (entry >> 16u) & 0x7fffu);
    let pixel = vec2<i32>(cell * 8u + vec2(lane % 8u, lane / 8u));
    let size = vec2<i32>(shape.size);
    let inside = all(pixel < size);
    var next = vec4(0u);
    if flagged && inside {
        next = bitcast<vec4<u32>>(textureLoad(current, pixel, 0));
        if any(next != bitcast<vec4<u32>>(textureLoad(previous, pixel, 0))) { atomicStore(&changed, 1u); }
    }
    // Alongside, the hypotheses voted within the cells sharing an output
    // field with this one.
    let r = i32(shape.neighbourhood);
    let side = u32(2 * r + 1);
    for (var i = lane; n > 1u && i < side * side; i += 64u) {
        let near = vec2<i32>(cell) + vec2(i32(i % side), i32(i / side)) - r;
        if any(near < vec2(0)) || near.x >= i32(shape.columns) || near.y >= i32(shape.rows) { continue; }
        let pending = PENDING * shape.columns * shape.rows + (u32(near.y) * shape.columns + u32(near.x)) * PENDING_WORDS;
        let voted = anchors[pending + 3u];
        if voted == 0u || anchors[pending] != epoch { continue; }
        for (var h = 1u; h < n; h++) {
            if chosen[h] == voted { atomicOr(&evidence, 1u << h); }
        }
    }
    workgroupBarrier();
    if lane == 0u { evident = atomicLoad(&evidence); }
    let checked = workgroupUniformLoad(&evident);
    if checked != 0u && inside {
        if !flagged { next = bitcast<vec4<u32>>(textureLoad(current, pixel, 0)); }
        for (var i = 1u; i < n; i++) {
            if (checked & (1u << i)) == 0u { continue; }
            let source = pixel - hypothesis(chosen[i]);
            if any(source < vec2(0)) || any(source >= size)
                || any(next != bitcast<vec4<u32>>(textureLoad(previous, source, 0))) {
                atomicOr(&rejected, 1u << i);
            }
        }
    }
    workgroupBarrier();
    if lane == 0u {
        let at = cell.y * shape.columns + cell.x;
        if atomicLoad(&changed) != 0u { facts[at].changed = epoch; }
        if checked != 0u {
            facts[at].compared = epoch;
            facts[at].matches = checked & ~atomicLoad(&rejected);
        }
    }
}
