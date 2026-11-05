// Diagnostic history only. The content chain never reads this storage.
struct Damage {
    canvas: vec2<u32>, grid: vec2<u32>,
    unit: u32, output_unit: u32, upscaled: u32, rotation: u32,
    view: vec4<f32>, dst: vec4<f32>,
}
@group(0) @binding(1) var<uniform> damage: Damage;
@group(3) @binding(4) var<storage, read_write> history: array<vec4<u32>>;
@group(3) @binding(5) var<storage, read> producer: array<u32>;
@group(3) @binding(6) var<storage, read> delta: array<u32>;
@group(3) @binding(7) var<storage, read> cells: array<u32>;

fn marked(bits: u32, x: u32) -> bool { return (bits & (1u << (x % 32u))) != 0u; }
@compute @workgroup_size(64)
fn age(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= damage.grid.x * damage.grid.y { return; }
    let p = vec2(id.x % damage.grid.x, id.x / damage.grid.x) * damage.unit / 8u;
    let stride = ((damage.canvas.x + 7u) / 8u + 31u) / 32u;
    let word = p.y * stride + p.x / 32u;
    var h = history[id.x];
    h = max(h, vec4(1u)) - 1u;
    if marked(producer[word], p.x) { h.x = 8u; }
    if (damage.upscaled & 1u) != 0u && marked(delta[word], p.x) { h.y = 8u; }
    history[id.x] = h;
}
@compute @workgroup_size(1)
fn output_cells(@builtin(workgroup_id) id: vec3<u32>) {
    let packed = cells[id.x];
    let origin = vec2(packed & 65535u, packed >> 16u) * damage.output_unit;
    for (var y = origin.y; y < min(origin.y + damage.output_unit, damage.canvas.y); y += damage.unit) {
        for (var x = origin.x; x < min(origin.x + damage.output_unit, damage.canvas.x); x += damage.unit) {
            let index = (y / damage.unit) * damage.grid.x + x / damage.unit;
            history[index].z = 8u;
        }
    }
}
@vertex fn tint_vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 3>(vec2(-1., -1.), vec2(3., -1.), vec2(-1., 3.));
    return vec4(corners[vertex], 0., 1.);
}
fn canvas_position(position: vec2<f32>) -> vec2<f32> {
    let uv = (position - damage.dst.xy) / damage.dst.zw;
    switch damage.rotation {
        case 1u: { return vec2(uv.y, 1. - uv.x); }
        case 2u: { return 1. - uv; }
        case 3u: { return vec2(1. - uv.y, uv.x); }
        default: { return uv; }
    }
}
@fragment fn tint_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = canvas_position(position.xy);
    if any(uv < vec2(0.)) || any(uv >= vec2(1.)) { discard; }
    let cell = vec2<u32>(uv * vec2<f32>(damage.canvas)) / damage.unit;
    var h = vec3<f32>(history[cell.y * damage.grid.x + cell.x].xyz) / 8.;
    if (damage.upscaled & 2u) == 0u { h.y = 0.; h.z = 0.; }
    // Spatial stripes keep all three quantities visible when they overlap.
    let stripe = (u32(position.x) + u32(position.y)) % 6u / 2u;
    let strengths = array<f32, 3>(h.x, h.y, h.z);
    let colors = array<vec3<f32>, 3>(vec3(1., 0.2, 0.45), vec3(0.1, 1., 0.65), vec3(0.3, 0.55, 1.));
    let marked_count = u32(h.x > 0.) + u32(h.y > 0.) + u32(h.z > 0.);
    var selected = stripe;
    if marked_count == 1u || strengths[selected] == 0. {
        selected = select(select(2u, 1u, h.y > 0.), 0u, h.x > 0.);
    }
    let alpha = strengths[selected] * 0.5;
    return vec4(colors[selected] * alpha, alpha);
}
