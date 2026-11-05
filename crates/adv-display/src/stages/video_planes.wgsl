@group(1) @binding(0) var luma: texture_2d<f32>;
@group(1) @binding(1) var chroma: texture_2d<f32>;
@group(2) @binding(0) var filtering: sampler;
// Gather the bilinear footprint and interpolate in float before the color
// matrix, without quantizing filtered YUV back to the 8-bit plane format.
fn interpolate(values: vec4<f32>, weight: vec2<f32>) -> f32 {
    return mix(mix(values.w, values.z, weight.x), mix(values.x, values.y, weight.x), weight.y);
}
fn sample_video(uv: vec2<f32>) -> vec4<f32> {
    let y_size = vec2<f32>(textureDimensions(luma));
    let y_at = uv * y_size - vec2(0.5);
    let y_coord = (floor(y_at) + vec2(1.0)) / y_size;
    let y = (interpolate(textureGather(0, luma, filtering, y_coord), fract(y_at)) - p.range.y) * p.range.x;
    let uv_size = vec2<f32>(textureDimensions(chroma));
    let uv_at = uv * uv_size - vec2(0.5);
    let uv_coord = (floor(uv_at) + vec2(1.0)) / uv_size;
    let cbcr = (vec2(
        interpolate(textureGather(0, chroma, filtering, uv_coord), fract(uv_at)),
        interpolate(textureGather(1, chroma, filtering, uv_coord), fract(uv_at))
    ) - vec2(p.range.w)) * p.range.z;
    let yuv = vec3(y, cbcr);
    return vec4(dot(p.r.xyz, yuv), dot(p.g.xyz, yuv), dot(p.b.xyz, yuv), 1.);
}
