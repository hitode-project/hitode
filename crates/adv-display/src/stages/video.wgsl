struct Params {
    canvas: vec4<f32>, dst: vec4<f32>, uv: vec4<f32>, bg: vec4<f32>,
    color: vec4<f32>, range: vec4<f32>, r: vec4<f32>, g: vec4<f32>, b: vec4<f32>,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(3) @binding(2) var<storage, read> tiles: array<vec4<f32>>;
@group(1) @binding(0) var luma: texture_2d<f32>;
@group(1) @binding(1) var chroma: texture_2d<f32>;
@group(2) @binding(0) var filtering: sampler;
@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
    let pos = tiles[tile].xy + corners[vertex] * tiles[tile].zw;
    return vec4(pos / p.canvas.xy * vec2(2., -2.) + vec2(-1., 1.), 0., 1.);
}
fn inside(pos: vec2<f32>, r: vec4<f32>) -> bool { return all(pos >= r.xy) && all(pos < r.xy + r.zw); }
// Gather the bilinear footprint and interpolate in float before the color
// matrix, without quantizing filtered YUV back to the 8-bit plane format.
fn interpolate(values: vec4<f32>, weight: vec2<f32>) -> f32 {
    return mix(mix(values.w, values.z, weight.x), mix(values.x, values.y, weight.x), weight.y);
}
fn video_color(pos: vec4<f32>) -> vec4<f32> {
    if inside(pos.xy, p.dst) {
        let uv = p.uv.xy + (pos.xy - p.dst.xy) / p.dst.zw * p.uv.zw;
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
    return p.color;
}

@fragment fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if inside(pos.xy, p.dst) || inside(pos.xy, p.bg) { return video_color(pos); }
    discard;
}
@vertex fn vs_software(@builtin(vertex_index) vertex: u32, @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    return software_vertex(vertex, tile);
}
@fragment fn fs_software(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if inside(pos.xy, p.dst) || inside(pos.xy, p.bg) { return video_color(pos); }
    return software_pixel(u32(pos.x), u32(pos.y));
}
