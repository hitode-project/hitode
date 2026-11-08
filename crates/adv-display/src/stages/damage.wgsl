// Diagnostic history only. The content chain never reads this storage.
struct Damage {
    canvas: vec2<u32>, grid: vec2<u32>,
    unit: u32, output_unit: u32, upscaled: u32, rotation: u32,
    mark_time: u32, scale: f32, dst: vec4<f32>,
}
@group(0) @binding(1) var<uniform> damage: Damage;
struct History {
    output_pixels: atomic<u32>,
    cells: array<vec4<u32>>,
}
@group(3) @binding(4) var<storage, read_write> history: History;
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
    var h = history.cells[id.x];
    if damage.mark_time != 0u {
        if marked(producer[word], p.x) { h.x = damage.mark_time; }
        history.cells[id.x] = h;
        return;
    }
    h = max(h, vec4(1u)) - 1u;
    if marked(producer[word], p.x) { h.x = 8u; }
    if (damage.upscaled & 1u) != 0u && marked(delta[word], p.x) { h.y = 8u; }
    history.cells[id.x] = h;
}
@compute @workgroup_size(1)
fn output_cells(@builtin(workgroup_id) id: vec3<u32>) {
    let packed = cells[id.x];
    let origin = vec2(packed & 65535u, packed >> 16u) * damage.output_unit;
    let end = min(origin + damage.output_unit, damage.canvas);
    if (damage.upscaled & 4u) != 0u {
        let size = end - origin;
        atomicAdd(&history.output_pixels, size.x * size.y);
    }
    for (var y = origin.y; y < end.y; y += damage.unit) {
        for (var x = origin.x; x < end.x; x += damage.unit) {
            let index = (y / damage.unit) * damage.grid.x + x / damage.unit;
            history.cells[index].z = select(damage.mark_time, 8u, damage.mark_time == 0u);
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

// Contours stay in display pixels rather than growing with the game canvas.
// The inner blue wash is bounded by its edge, including on whole-frame updates.
fn mark_at(position: vec2<f32>) -> vec2<f32> {
    let uv = canvas_position(position);
    if any(uv < vec2(0.)) || any(uv >= vec2(1.)) { return vec2(0.); }
    let cell = vec2<u32>(uv * vec2<f32>(damage.canvas)) / damage.unit;
    let times = history.cells[cell.y * damage.grid.x + cell.x].xz;
    let age_ms = vec2<f32>(vec2(damage.mark_time) - times);
    var strength = select(
        vec2(0.),
        clamp((vec2(300.) - age_ms) / 150., vec2(0.), vec2(1.)),
        times != vec2(0u),
    );
    if (damage.upscaled & 2u) == 0u { strength.y = 0.; }
    return strength;
}
fn nearby_marks(p: vec2<f32>, step: vec2<f32>) -> vec2<f32> {
    return min(mark_at(p - step), mark_at(p + step));
}
@fragment fn contour_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = position.xy;
    let center = mark_at(p);
    if all(center == vec2(0.)) { discard; }
    let scale = max(damage.scale, 1.);
    let x = vec2(scale, 0.);
    let y = vec2(0., scale);
    var pink_alpha = 0.;
    if center.x > 0. {
        let inside = min(nearby_marks(p, x * 1.5).x, nearby_marks(p, y * 1.5).x);
        pink_alpha = select(center.x * 0.74, 0., inside > 0.);
    }
    // The wider blue contour remains visible behind a coincident solid line.
    var blue_alpha = 0.;
    if center.y > 0. {
        let edge_x = nearby_marks(p, x * 3.).y == 0.;
        let edge_y = nearby_marks(p, y * 3.).y == 0.;
        if edge_x || edge_y {
            let along = select(p.x, p.y, edge_x && !edge_y);
            let dash = fract(along / (8. * scale)) < 0.62;
            blue_alpha = center.y * select(0.1, 0.68, dash);
        }
    }
    let alpha = pink_alpha + blue_alpha * (1. - pink_alpha);
    let rgb = vec3(0.96, 0.40, 0.59) * pink_alpha
        + vec3(0.42, 0.67, 1.) * blue_alpha * (1. - pink_alpha);
    return vec4(rgb, alpha);
}
@fragment fn tint_fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = canvas_position(position.xy);
    if any(uv < vec2(0.)) || any(uv >= vec2(1.)) { discard; }
    let cell = vec2<u32>(uv * vec2<f32>(damage.canvas)) / damage.unit;
    var h = vec3<f32>(history.cells[cell.y * damage.grid.x + cell.x].xyz) / 8.;
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
