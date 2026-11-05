// One workgroup compares a producer cell, then verifies at most four proposals
// only when that cell is in delta. Digests select references, never matches.
struct Dimensions {
    size: vec2<u32>, argument_words: u32, argument_stride: u32,
    reach: u32, translations: u32, output_unit: u32, padding: u32,
}
struct Proposal { rectangle: vec4<u32>, offset: vec2<i32>, padding: vec2<u32> }
@group(0) @binding(0) var<uniform> dimensions: Dimensions;
@group(1) @binding(0) var scratch: texture_2d<f32>;
@group(1) @binding(1) var previous: texture_2d<f32>;
@group(1) @binding(2) var previous_output: texture_2d<f32>;
@group(3) @binding(0) var input: texture_storage_2d<rgba16float, read_write>;
@group(3) @binding(1) var<storage, read> cells: array<u32>;
@group(3) @binding(2) var<storage, read_write> delta: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read_write> gates: array<atomic<u32>>;
@group(3) @binding(4) var<storage, read_write> verified: Verification;
@group(3) @binding(5) var<storage, read> proposals: array<Proposal>;
var<workgroup> changed: atomic<u32>;
var<workgroup> compared_cell: vec2<u32>;
var<workgroup> offsets: array<vec2<i32>, 4>;
var<workgroup> matches: array<atomic<u32>, 4>;
var<workgroup> candidates: u32;
var<workgroup> kinds: array<u32, 4>;
var<workgroup> digest0: atomic<u32>;
var<workgroup> digest1: atomic<u32>;
var<workgroup> constant: atomic<u32>;
var<workgroup> colour: vec4<f32>;

fn mark(cell: vec2<u32>) {
    let stride = ((dimensions.size.x + 7u) / 8u + 31u) / 32u;
    atomicOr(&delta[cell.y * stride + cell.x / 32u], 1u << (cell.x % 32u));
    if atomicOr(&delta[stride * ((dimensions.size.y + 7u) / 8u)], 1u) == 0u {
        for (var i = 0u; i < dimensions.argument_words; i += dimensions.argument_stride) {
            atomicStore(&gates[i], (atomicLoad(&gates[i + 3u]) + 63u) / 64u);
        }
    }
}

fn offer(offset: vec2<i32>, kind: u32) {
    if all(offset == vec2(0)) || candidates == 4u { return; }
    for (var i = 0u; i < candidates; i++) {
        if all(offsets[i] == offset) { kinds[i] = max(kinds[i], kind); return; }
    }
    offsets[candidates] = offset;
    kinds[candidates] = kind;
    let source = clamp(vec2<i32>(compared_cell * 8u) - offset, vec2(0), vec2<i32>(dimensions.size) - 1);
    let first = textureLoad(previous, source, 0);
    atomicStore(&matches[candidates], u32(all(bitcast<vec4<u32>>(colour) == bitcast<vec4<u32>>(first))));
    candidates++;
}

fn update(group: vec3<u32>, lane: u32, initialize: bool) {
    if lane == 0u {
        let packed = cells[group.x];
        compared_cell = vec2(packed & 65535u, packed >> 16u);
        colour = textureLoad(scratch, vec2<i32>(compared_cell * 8u), 0);
        atomicStore(&constant, 1u);
    }
    workgroupBarrier();
    let cell = compared_cell;
    let pixel = cell * 8u + vec2(lane % 8u, lane / 8u);
    let inside = all(pixel < dimensions.size);
    var next = vec4(0.0);
    if inside {
        next = textureLoad(scratch, vec2<i32>(pixel), 0);
        var different = initialize;
        if !initialize {
            different = any(bitcast<vec4<u32>>(next) != bitcast<vec4<u32>>(textureLoad(input, vec2<i32>(pixel))));
        }
        if different {
            textureStore(input, vec2<i32>(pixel), next);
            atomicStore(&changed, 1u);
        }
    }
    workgroupBarrier();
    if atomicLoad(&changed) == 0u { return; }
    if lane == 0u { mark(cell); }
    if inside {
        let bits = bitcast<vec4<u32>>(next);
        let a = bits.x * 0x9e3779b9u ^ bits.y * 0x85ebca6bu ^ bits.z * 0xc2b2ae35u ^ bits.w;
        let b = a ^ (a >> 16u) ^ (lane * 0x27d4eb2du);
        atomicXor(&digest0, b * 0x165667b1u);
        atomicAdd(&digest1, (b ^ (b >> 13u)) * 0x9e3779b9u);
        if any(bits != bitcast<vec4<u32>>(colour)) { atomicStore(&constant, 0u); }
    }
    workgroupBarrier();
    if lane == 0u {
        if !initialize && dimensions.translations != 0u {
            if verified.sharp != 0u { offer(verified.motion, 0u); }
            for (var i = 0u; i < verified.count && candidates < 4u; i++) {
                let p = proposals[i];
                if all(cell * 8u < p.rectangle.xy + p.rectangle.zw) && all(cell * 8u + 8u > p.rectangle.xy) {
                    offer(p.offset, 1u);
                }
            }
        }
    }
    workgroupBarrier();
    if inside && lane != 0u {
        let bits = bitcast<vec4<u32>>(next);
        for (var i = 0u; i < candidates; i++) {
            if atomicLoad(&matches[i]) == 0u { continue; }
            let source = clamp(vec2<i32>(pixel) - offsets[i], vec2(0), vec2<i32>(dimensions.size) - 1);
            let old = textureLoad(previous, source, 0);
            if any(bits != bitcast<vec4<u32>>(old)) { atomicStore(&matches[i], 0u); }
        }
    }
    workgroupBarrier();
    // Source ranges can overlap across offsets. Atomic stores of their
    // original RGBA8 bytes make those scratch copies ordered and identical.
    if inside {
        let columns = (dimensions.size.x + 7u) / 8u;
        for (var i = 0u; i < candidates; i++) {
            if atomicLoad(&matches[i]) == 0u { continue; }
            let source = clamp(vec2<i32>(pixel) - offsets[i], vec2(0), vec2<i32>(dimensions.size) - 1);
            for (var y = 0u; y < 2u; y++) {
                for (var x = 0u; x < 2u; x++) {
                    let p = vec2<u32>(source) * 2u + vec2(x, y);
                    let source_cell = p / 16u;
                    let at = source_cell.y * columns + source_cell.x;
                    let local = p % 16u;
                    let value = pack4x8unorm(textureLoad(previous_output, vec2<i32>(p), 0));
                    atomicStore(&verified.cells[at].output[local.y * 16u + local.x], value);
                }
            }
        }
    }
    if lane == 0u {
        let at = cell.y * ((dimensions.size.x + 7u) / 8u) + cell.x;
        verified.cells[at].data = vec4(verified.epoch, atomicLoad(&digest0), atomicLoad(&digest1), atomicLoad(&constant));
        verified.cells[at].colour = vec2(pack2x16float(colour.xy), pack2x16float(colour.zw));
        atomicStore(&verified.cells[at].slot, 0xffffffffu);
        atomicStore(&verified.cells[at].constant_epoch, 0u);
        for (var i = 0u; i < 4u; i++) {
            var term = vec4<i32>(0);
            if i < candidates { term = vec4(offsets[i], i32(atomicLoad(&matches[i])), i32(kinds[i])); }
            verified.cells[at].terms[i] = term;
        }
    }
}

@compute @workgroup_size(64)
fn compare(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    update(group, lane, false);
}
@compute @workgroup_size(64)
fn initialize(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    update(group, lane, true);
}
