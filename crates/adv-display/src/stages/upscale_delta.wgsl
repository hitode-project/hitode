// One workgroup compares one producer cell. The last bitmap word gates all
// filters once, so an unchanged composition launches no filter workgroups.
struct Dimensions { size: vec2<u32>, argument_words: u32, argument_stride: u32 }
@group(0) @binding(0) var<uniform> dimensions: Dimensions;
@group(1) @binding(0) var scratch: texture_2d<f32>;
@group(3) @binding(0) var input: texture_storage_2d<rgba16float, read_write>;
@group(3) @binding(1) var<storage, read> cells: array<u32>;
@group(3) @binding(2) var<storage, read_write> delta: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read_write> gates: array<atomic<u32>>;
var<workgroup> changed: atomic<u32>;
var<workgroup> compared_cell: vec2<u32>;

fn mark(cell: vec2<u32>) {
    let stride = ((dimensions.size.x + 7u) / 8u + 31u) / 32u;
    atomicOr(&delta[cell.y * stride + cell.x / 32u], 1u << (cell.x % 32u));
    if atomicOr(&delta[stride * ((dimensions.size.y + 7u) / 8u)], 1u) == 0u {
        for (var i = 0u; i < dimensions.argument_words; i += dimensions.argument_stride) {
            atomicStore(&gates[i], (atomicLoad(&gates[i + 3u]) + 63u) / 64u);
        }
    }
}

fn update(group: vec3<u32>, lane: u32, initialize: bool) {
    if lane == 0u {
        let packed = cells[group.x];
        compared_cell = vec2(packed & 65535u, packed >> 16u);
    }
    workgroupBarrier();
    let cell = compared_cell;
    let pixel = cell * 8u + vec2(lane % 8u, lane / 8u);
    if all(pixel < dimensions.size) {
        let next = textureLoad(scratch, vec2<i32>(pixel), 0);
        var different = initialize;
        if !initialize {
            let previous = textureLoad(input, vec2<i32>(pixel));
            different = any(bitcast<vec4<u32>>(next) != bitcast<vec4<u32>>(previous));
        }
        if different {
            textureStore(input, vec2<i32>(pixel), next);
            atomicStore(&changed, 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u && atomicLoad(&changed) != 0u { mark(cell); }
}

@compute @workgroup_size(64)
fn compare(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    update(group, lane, false);
}

// The first composition has no previous input or valid tensors.
@compute @workgroup_size(64)
fn initialize(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    update(group, lane, true);
}
