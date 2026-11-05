struct Dimensions {
    size: vec2<u32>, argument_words: u32, argument_stride: u32,
    reach: u32, translations: u32, output_unit: u32, padding: u32,
}
struct Header { epoch: u32, count: u32, offset: vec2<i32>, sharp: u32 }
@group(0) @binding(0) var<uniform> dimensions: Dimensions;
@group(1) @binding(0) var composed: texture_2d<f32>;
@group(1) @binding(1) var previous: texture_2d<f32>;
@group(3) @binding(0) var<storage, read_write> data: array<u32>;
@group(3) @binding(1) var<storage, read_write> header: Header;
var<workgroup> sums: array<vec4<f32>, 64>;
var<workgroup> products: array<f32, 64>;

fn bitmap_words() -> u32 {
    return ((dimensions.size.x + 7u) / 8u + 31u) / 32u * ((dimensions.size.y + 7u) / 8u);
}
fn profile(axis: u32, old: bool, line: u32) -> u32 {
    return bitmap_words() + select(0u, dimensions.size.x, axis == 1u) * 2u + line * 2u + u32(old);
}
fn score(axis: u32, index: u32) -> u32 {
    return bitmap_words() + (dimensions.size.x + dimensions.size.y) * 2u + select(0u, dimensions.size.x * 2u, axis == 1u) + index;
}

@compute @workgroup_size(64)
fn project(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let axis = group.y;
    let length = dimensions.size[axis];
    let across = dimensions.size[1u - axis];
    if group.x >= length { return; }
    var values = vec2(0.0);
    for (var i = lane; i < across; i += 64u) {
        var pixel = vec2(group.x, i);
        if axis == 1u { pixel = pixel.yx; }
        let old = textureLoad(previous, vec2<i32>(pixel), 0).xyz;
        let cell = pixel / 8u;
        let stride = ((dimensions.size.x + 7u) / 8u + 31u) / 32u;
        var next = old;
        if (data[cell.y * stride + cell.x / 32u] & (1u << (cell.x % 32u))) != 0u {
            next = textureLoad(composed, vec2<i32>(pixel), 0).xyz;
        }
        values += vec2(dot(next, vec3(0.299, 0.587, 0.114)), dot(old, vec3(0.299, 0.587, 0.114)));
    }
    sums[lane] = vec4(values, 0.0, 0.0);
    workgroupBarrier();
    for (var step = 32u; step > 0u; step /= 2u) {
        if lane < step { sums[lane] += sums[lane + step]; }
        workgroupBarrier();
    }
    if lane == 0u {
        data[profile(axis, false, group.x)] = bitcast<u32>(sums[0].x / f32(across));
        data[profile(axis, true, group.x)] = bitcast<u32>(sums[0].y / f32(across));
    }
}

@compute @workgroup_size(64)
fn correlate(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let axis = group.y;
    let length = dimensions.size[axis];
    if group.x >= length * 2u - 1u { return; }
    let offset = i32(group.x) - i32(length) + 1;
    let first = u32(max(offset, 0));
    let end = u32(min(i32(length) + offset, i32(length)));
    let count = end - first;
    var value = vec4(0.0);
    var product = 0.0;
    for (var i = first + lane; i < end; i += 64u) {
        let a = bitcast<f32>(data[profile(axis, false, i)]);
        let b = bitcast<f32>(data[profile(axis, true, u32(i32(i) - offset))]);
        value += vec4(a, b, a * a, b * b);
        product += a * b;
    }
    sums[lane] = value;
    products[lane] = product;
    workgroupBarrier();
    for (var step = 32u; step > 0u; step /= 2u) {
        if lane < step {
            sums[lane] += sums[lane + step];
            products[lane] += products[lane + step];
        }
        workgroupBarrier();
    }
    if lane == 0u {
        let s = sums[0];
        let variance = (f32(count) * s.z - s.x * s.x) * (f32(count) * s.w - s.y * s.y);
        var correlation = -1.0;
        if count >= (length + 1u) / 2u && variance > 0.0 {
            correlation = (f32(count) * products[0] - s.x * s.y) / sqrt(variance);
        }
        data[score(axis, group.x)] = bitcast<u32>(correlation);
    }
}

@compute @workgroup_size(1)
fn choose() {
    var offset = vec2<i32>(0);
    var sharp = true;
    for (var axis = 0u; axis < 2u; axis++) {
        let length = dimensions.size[axis];
        var best = -1.0;
        var second = -1.0;
        for (var i = 0u; i < length * 2u - 1u; i++) {
            let correlation = bitcast<f32>(data[score(axis, i)]);
            if correlation > best {
                second = best;
                best = correlation;
                offset[axis] = i32(i) - i32(length) + 1;
            } else {
                second = max(second, correlation);
            }
        }
        sharp = sharp && best > 0.95 && best - second > 0.05;
    }
    header.offset = offset;
    header.sharp = u32(sharp);
}
