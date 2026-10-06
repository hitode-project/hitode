struct Parameters {
    color_op: u32,
    alpha_op: u32,
    alpha_compare: u32,
    alpha_reference: f32,
    address_flags: u32,
    border_rgba: u32,
    first_level: u32,
    filter_flags: u32,
}
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> parameters: Parameters;

struct Fragment {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
}
@vertex fn vertex_main(@location(0) position: vec4<f32>,
    @location(1) color: vec4<f32>, @location(2) uv: vec2<f32>) -> Fragment {
    var result: Fragment;
    result.position = position;
    result.color = color;
    result.uv = uv;
    return result;
}
fn combine(op: u32, color: vec4<f32>, texel: vec4<f32>) -> vec4<f32> {
    switch op {
        case 0u: { return color; }
        case 1u: { return texel; }
        case 2u: { return color * texel; }
        case 3u: { return clamp(color + texel, vec4(0.0), vec4(1.0)); }
        default: { return clamp(color - texel, vec4(0.0), vec4(1.0)); }
    }
}
fn compare_alpha(alpha: f32) -> bool {
    let reference = parameters.alpha_reference;
    switch parameters.alpha_compare {
        case 0u: { return false; }
        case 1u: { return alpha < reference; }
        case 2u: { return alpha == reference; }
        case 3u: { return alpha <= reference; }
        case 4u: { return alpha > reference; }
        case 5u: { return alpha != reference; }
        case 6u: { return alpha >= reference; }
        default: { return true; }
    }
}
@fragment fn fragment_main(input: Fragment) -> @location(0) vec4<f32> {
    var texel = textureSample(image, image_sampler, input.uv);
    if (parameters.address_flags & 3u) != 0u {
        let dimensions = vec2<f32>(textureDimensions(image, 0));
        let dx = dpdx(input.uv) * dimensions;
        let dy = dpdy(input.uv) * dimensions;
        let lod = 0.5 * log2(max(max(dot(dx, dx), dot(dy, dy)), 1e-16));
        texel = adv_border_sample(image, input.uv, lod, parameters.address_flags,
            parameters.border_rgba, parameters.first_level, parameters.filter_flags);
    }
    if (parameters.alpha_op & 0x80000000u) != 0u { texel.a = 1.0; }
    let color = combine(parameters.color_op, input.color, texel).rgb;
    let alpha = combine(parameters.alpha_op & 0x7fffffffu, input.color, texel).a;
    if !compare_alpha(alpha) { discard; }
    return vec4(color, alpha);
}
