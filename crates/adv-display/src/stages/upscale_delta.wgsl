// Compare: one workgroup per producer cell. Composition already wrote the
// whole frame into `current`; `previous` is the last encoded input. A changed
// cell stamps itself with the frame's epoch, so nothing is cleared per frame.
@group(1) @binding(0) var current: texture_2d<f32>;
@group(1) @binding(1) var previous: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read_write> stamps: array<u32>;
@group(3) @binding(2) var<storage, read> metadata: Metadata;
var<workgroup> changed: atomic<u32>;

@compute @workgroup_size(64)
fn compare(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let index = group.y * 65535u + group.x;
    if index >= frame[FRAME_COMPARED] { return; }
    let packed = frame[FRAME_DATA + frame[FRAME_LIST] + index];
    let cell = vec2(packed & 65535u, packed >> 16u);
    let pixel = vec2<i32>(cell * 8u + vec2(lane % 8u, lane / 8u));
    if all(vec2<u32>(pixel) < metadata.shape.size) {
        let next = bitcast<vec4<u32>>(textureLoad(current, pixel, 0));
        if any(next != bitcast<vec4<u32>>(textureLoad(previous, pixel, 0))) {
            atomicStore(&changed, 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u && atomicLoad(&changed) != 0u {
        stamps[cell.y * metadata.shape.columns + cell.x] = frame[FRAME_EPOCH];
    }
}
