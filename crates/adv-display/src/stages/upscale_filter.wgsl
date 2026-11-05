struct Parameters { size: vec2<u32>, unit: u32, reach: u32 }
struct Arguments { x: atomic<u32>, y: u32, z: u32, count: u32 }
@group(0) @binding(0) var<uniform> parameters: Parameters;
@group(3) @binding(0) var<storage, read> cells: array<u32>;
@group(3) @binding(1) var<storage, read> delta: array<u32>;
@group(3) @binding(2) var<storage, read_write> compacted: array<u32>;
@group(3) @binding(3) var<storage, read_write> arguments: Arguments;
var<workgroup> count: atomic<u32>;
var<workgroup> base: u32;

fn meets(packed: u32) -> bool {
    let cell = vec2(packed & 65535u, packed >> 16u);
    let origin = cell * parameters.unit;
    let lo = (max(origin, vec2(parameters.reach)) - parameters.reach) / 8u;
    let hi = (min(origin + parameters.unit + parameters.reach, parameters.size) + 7u) / 8u;
    let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
    for (var y = lo.y; y < hi.y; y++) {
        for (var word = lo.x / 32u; word <= (hi.x - 1u) / 32u; word++) {
            let first = select(0u, lo.x % 32u, word == lo.x / 32u);
            let last = select(31u, (hi.x - 1u) % 32u, word == (hi.x - 1u) / 32u);
            let mask = (0xffffffffu << first) & (0xffffffffu >> (31u - last));
            if (delta[y * stride + word] & mask) != 0u { return true; }
        }
    }
    return false;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    var packed = 0u;
    var keep = false;
    var local = 0u;
    if id.x < arguments.count {
        packed = cells[id.x];
        keep = meets(packed);
        if keep { local = atomicAdd(&count, 1u); }
    }
    workgroupBarrier();
    if lane == 0u {
        let kept = atomicLoad(&count);
        if kept != 0u { base = atomicAdd(&arguments.x, kept); }
    }
    workgroupBarrier();
    if keep { compacted[base + local] = packed; }
}
