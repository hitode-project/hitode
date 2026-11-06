// Compare: one workgroup per producer cell. Composition already wrote the
// whole frame into `current`; `previous` is the last encoded input. A changed
// cell records its facts and verifies at most four offsets pixel by pixel.
// Nothing here writes an image.
@group(1) @binding(0) var current: texture_2d<f32>;
@group(1) @binding(1) var previous: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read_write> references: References;
@group(3) @binding(2) var<storage, read> state: array<u32>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
var<workgroup> changed: atomic<u32>;
var<workgroup> varied: atomic<u32>;
var<workgroup> rejected: array<atomic<u32>, 4>;
var<workgroup> candidates: array<u32, 4>;
var<workgroup> count: u32;

fn offset_entry(k: u32) -> vec4<i32> {
    let at = FRAME_OFFSETS + k * 4u;
    return bitcast<vec4<i32>>(vec4(frame[at], frame[at + 1u], frame[at + 2u], frame[at + 3u]));
}

// Offset index k proposed for `cell`, deduplicated by value.
fn offer(k: u32) {
    if count == 4u { return; }
    let offset = offset_entry(k).xy;
    var index = k;
    for (var j = 1u; j < 8u; j++) {
        if offset_entry(j).z != 0 && all(offset_entry(j).xy == offset) { index = j; break; }
    }
    for (var i = 0u; i < count; i++) {
        if candidates[i] == index { return; }
    }
    candidates[count] = index;
    count++;
}

fn offset_of(k: u32) -> vec2<i32> {
    if k == 0u { return vec2(i32(state[MOTION_OFFSET]), i32(state[MOTION_OFFSET + 1u])); }
    return offset_entry(k).xy;
}

@compute @workgroup_size(64)
fn compare(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let index = group.y * 65535u + group.x;
    if index >= frame[FRAME_COMPARED] { return; }
    let size = metadata.shape.size;
    let packed = frame[FRAME_DATA + frame[FRAME_LIST] + index];
    let cell = vec2(packed & 65535u, packed >> 16u);
    let pixel = cell * 8u + vec2(lane % 8u, lane / 8u);
    let inside = all(pixel < size);
    let first = bitcast<vec4<u32>>(textureLoad(current, vec2<i32>(cell * 8u), 0));
    var next = vec4(0u);
    if inside {
        next = bitcast<vec4<u32>>(textureLoad(current, vec2<i32>(pixel), 0));
        if any(next != bitcast<vec4<u32>>(textureLoad(previous, vec2<i32>(pixel), 0))) {
            atomicStore(&changed, 1u);
        }
        if any(next != first) { atomicStore(&varied, 1u); }
    }
    if workgroupUniformLoad(&changed) == 0u { return; }
    if lane == 0u && metadata.shape.translations != 0u {
        let global = state[MOTION_SHARP] != 0u && state[MOTION_EPOCH] == frame[FRAME_EPOCH];
        if global && any(offset_of(0u) != vec2(0)) {
            // The global motion takes a proposal's index when they agree.
            var k = 0u;
            for (var j = 1u; j < 8u; j++) {
                if offset_entry(j).z != 0 && all(offset_entry(j).xy == offset_of(0u)) { k = j; break; }
            }
            candidates[0] = k;
            count = 1u;
        }
        for (var i = 0u; i < frame[FRAME_PROPOSALS] && count < 4u; i++) {
            let at = i * 8u;
            let rect = vec4(frame[FRAME_DATA + at], frame[FRAME_DATA + at + 1u], frame[FRAME_DATA + at + 2u], frame[FRAME_DATA + at + 3u]);
            if all(cell * 8u < rect.xy + rect.zw) && all(cell * 8u + 8u > rect.xy) {
                offer(frame[FRAME_DATA + at + 4u]);
            }
        }
    }
    let n = workgroupUniformLoad(&count);
    if inside {
        for (var i = 0u; i < n; i++) {
            let source = clamp(vec2<i32>(pixel) - offset_of(candidates[i]), vec2(0), vec2<i32>(size) - 1);
            if any(next != bitcast<vec4<u32>>(textureLoad(previous, source, 0))) {
                atomicStore(&rejected[i], 1u);
            }
        }
    }
    workgroupBarrier();
    if lane == 0u {
        var matches = 0u;
        for (var i = 0u; i < n; i++) {
            if atomicLoad(&rejected[i]) == 0u { matches |= 1u << candidates[i]; }
        }
        let at = cell.y * metadata.shape.columns + cell.x;
        references.facts[at].stamp = frame[FRAME_EPOCH];
        references.facts[at].matches = matches;
        references.facts[at].constant = 1u - atomicLoad(&varied);
        let colour = bitcast<vec4<f32>>(first);
        references.facts[at].colour = vec2(pack2x16float(colour.xy), pack2x16float(colour.zw));
    }
}
