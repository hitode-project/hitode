struct ShaderConstants {
    f: array<vec4<f32>, 256>,
    i: array<vec4<i32>, 16>,
    b: array<vec4<i32>, 16>,
    bump_matrix: array<vec4<f32>, 8>,
    bump_luminance: array<vec4<f32>, 8>,
}
struct DrawParameters { values: array<vec4<u32>, 40> }
@group(0) @binding(0) var<uniform> vs_constants:ShaderConstants;
@group(0) @binding(2) var<uniform> draw_parameters:DrawParameters;
@group(1) @binding(0) var source: texture_2d<f32>;

@vertex fn vertex_main(@builtin(vertex_index) n: u32) -> @builtin(position) vec4<f32> {
    let vertices = array(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    return vec4(vertices[n], 0., 1.);
}

fn sample_axis(x: u32, axis: vec4<u32>, dst: u32) -> u32 {
    let a = axis.y;
    let b = axis.z;
    var pixel = x;
    switch axis.x {
        case 1u: {
            pixel = (max(1u, (2u * x + 1u) * a) - 1u) / b;
        }
        case 2u: {
            // The 24-bit fraction is multiplied in two 12-bit parts.
            let midpoint = b / 2u + (a & 1u) * 0x800000u;
            let low = (b & 0xfffu) * x + (midpoint & 0xfffu);
            let high = (b >> 12u) * x + (midpoint >> 12u) + (low >> 12u);
            pixel = a * x + a / 2u + (high >> 12u);
        }
        case 3u: {
            pixel = a / 2u + x * a + (dst / 2u + x * b) / dst;
        }
        default: {}
    }
    return select(pixel, axis.w - 1u - pixel, axis.w != 0u);
}

fn expand5(value: u32) -> u32 {
    return value << 3u | value >> 2u;
}

fn convert(raw: u32, mode: u32) -> u32 {
    var rgb: vec3<u32>;
    switch (mode & 15u) {
        case 1u: { rgb = (vec3(raw) >> vec3(16u, 8u, 0u)) & vec3(255u); }
        case 2u: { rgb = vec3(expand5((raw >> 10u) & 31u), expand5((raw >> 5u) & 31u), expand5(raw & 31u)); }
        case 3u: {
            let g = (raw >> 5u) & 63u;
            rgb = vec3(expand5(raw >> 11u), g << 2u | g >> 4u, expand5(raw & 31u));
        }
        case 4u: { rgb = vec3(raw); }
        default: { return raw; }
    }
    switch (mode >> 4u) {
        case 2u: { return (rgb.x >> 3u) << 10u | (rgb.y >> 3u) << 5u | (rgb.z >> 3u); }
        case 3u: { return (rgb.x >> 3u) << 11u | (rgb.y >> 2u) << 5u | (rgb.z >> 3u); }
        default: { return rgb.x << 16u | rgb.y << 8u | rgb.z; }
    }
}

@fragment fn fragment_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let origins = draw_parameters.values[0];
    let rule = draw_parameters.values[3];
    let xy = vec2<u32>(position.xy) - origins.zw;
    let coord = origins.xy + vec2(sample_axis(xy.x, draw_parameters.values[1], rule.x), sample_axis(xy.y, draw_parameters.values[2], rule.y));
    let texel = textureLoad(source, vec2<i32>(coord), 0);
    let src = draw_parameters.values[5];
    let raw = select(packed_raw(texel), packed_encode(texel, packed_storage_masks(src)), src.x != 0u);
    let key = draw_parameters.values[4];
    if key.x != 0u && (raw & rule.z) == key.y {
        discard;
    }
    let value = convert(raw, rule.w);
    let dst = draw_parameters.values[6];
    if dst.x != 0u { return packed_decode(value, packed_storage_masks(dst)); }
    return vec4<f32>((vec4<u32>(value) >> vec4(16u, 8u, 0u, 24u)) & vec4(255u)) / 255.;
}
