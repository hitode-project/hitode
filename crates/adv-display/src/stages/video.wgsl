struct Params {
    canvas: vec4<f32>, dst: vec4<f32>, uv: vec4<f32>, bg: vec4<f32>,
    color: vec4<f32>, range: vec4<f32>, r: vec4<f32>, g: vec4<f32>, b: vec4<f32>,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(3) @binding(2) var<storage, read> tiles: array<vec4<f32>>;

@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.));
    let pos = tiles[tile].xy + corners[vertex] * tiles[tile].zw;
    return vec4(pos / p.canvas.xy * vec2(2., -2.) + vec2(-1., 1.), 0., 1.);
}
fn inside(pos: vec2<f32>, r: vec4<f32>) -> bool { return all(pos >= r.xy) && all(pos < r.xy + r.zw); }
fn video_color(pos: vec4<f32>) -> vec4<f32> {
    if inside(pos.xy, p.dst) {
        let uv = p.uv.xy + (pos.xy - p.dst.xy) / p.dst.zw * p.uv.zw;
        return sample_video(uv);
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
