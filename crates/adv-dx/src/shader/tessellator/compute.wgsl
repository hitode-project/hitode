// Fixed-function patch tessellation. Port of CHWTessellator under the
// Microsoft MIT notice in tessellator.rs. All coordinates are 16.16 fixed
// point until they are written to the domain shader's input.
const one = 65536u;
const half_fixed = 32768u;
const epsilon = 1.0 / 65536.0;

struct TessPatch {
    edges: vec4<f32>,
    inside: vec2<f32>,
    domain: u32, // triangle, quad, isoline
    partitioning: u32, // integer/pow2, fractional odd, fractional even
    output: u32, // points, lines, clockwise, counterclockwise
}

struct TessRange {
    points: u32,
    indices: u32,
    first_point: u32,
    first_index: u32,
}

@group(3) @binding(0) var<storage, read> patches: array<TessPatch>;
@group(3) @binding(1) var<storage, read_write> ranges: array<TessRange>;
@group(3) @binding(2) var<storage, read_write> points: array<vec4<u32>>;
@group(3) @binding(3) var<storage, read_write> indices: array<u32>;

struct TessFactor {
    fixed: u32,
    odd: bool,
    half_points: u32,
    fraction: u32,
    split: u32,
    floor_reciprocal: u32,
    ceil_reciprocal: u32,
}

fn reciprocal(n: u32) -> u32 {
    return (one + n / 2u) / n;
}

fn factor(value: f32, partitioning: u32, lower: f32, inside: bool) -> TessFactor {
    let upper = select(64.0, 63.0, partitioning == 1u);
    let clamped = min(select(lower, value, value >= lower), upper);
    var fixed: u32;
    var odd: bool;
    if partitioning == 0u {
        let rounded = u32(ceil(clamped));
        fixed = rounded * one;
        odd = (rounded & 1u) != 0u && !(inside && rounded == 1u);
    } else {
        fixed = u32(round(clamped * f32(one)));
        odd = partitioning == 1u;
    }
    var h = (fixed + 1u) / 2u;
    if odd || h == half_fixed {
        h += half_fixed;
    }
    let lo = h / one;
    let hi = (h + one - 1u) / one;
    var split = 0u;
    if hi == lo {
        split = hi + 1u;
    } else if !odd || lo != 1u {
        let n = lo - u32(odd);
        split = 2u * (n ^ (1u << firstLeadingBit(n))) + 1u;
    }
    return TessFactor(fixed, odd, hi, h % one, split,
        reciprocal(2u * lo - u32(odd)), reciprocal(2u * hi - u32(odd)));
}

fn segments(f: TessFactor) -> u32 {
    return 2u * f.half_points - u32(f.odd);
}

fn location(f: TessFactor, i: u32) -> u32 {
    let flip = i >= f.half_points;
    let p = select(i, segments(f) - i, flip);
    if p == f.half_points {
        return half_fixed;
    }
    let on_floor = (p - u32(p > f.split)) * f.floor_reciprocal;
    let on_ceil = p * f.ceil_reciprocal;
    let at = (on_floor * (one - f.fraction) + on_ceil * f.fraction + half_fixed) >> 16u;
    return select(at, one - at, flip);
}

struct Factors {
    outside: array<TessFactor, 4>,
    inside: array<TessFactor, 2>,
    boundary: u32,
    unit: bool,
    culled: bool,
}

fn factors(domain_patch: TessPatch) -> Factors {
    var result: Factors;
    let domain = domain_patch.domain;
    let partitioning = domain_patch.partitioning;
    let lower = select(1.0, 2.0, partitioning == 2u);
    result.unit = true;
    if domain == 2u {
        result.culled = !(domain_patch.edges.x > 0.0 && domain_patch.edges.y > 0.0);
        result.outside[0] = factor(domain_patch.edges.x, 0u, 1.0, false);
        result.outside[1] = factor(domain_patch.edges.y, partitioning, lower, false);
        return result;
    }
    let edge_count = select(3u, 4u, domain == 1u);
    let axis_count = select(1u, 2u, domain == 1u);
    let threshold = 1.0 + epsilon / 2.0;
    var frame = false;
    for (var edge = 0u; edge < edge_count; edge++) {
        let value = domain_patch.edges[edge];
        result.culled = result.culled || !(value > 0.0);
        frame = frame || value > threshold;
        let f = factor(value, partitioning, lower, false);
        result.outside[edge] = f;
        result.boundary += segments(f);
        result.unit = result.unit && f.fixed == one;
    }
    for (var axis = 0u; axis < axis_count; axis++) {
        frame = frame || domain_patch.inside[axis] > threshold;
    }
    let inside_lower = select(lower, 1.0 + epsilon, partitioning == 1u && frame);
    for (var axis = 0u; axis < axis_count; axis++) {
        let f = factor(domain_patch.inside[axis], partitioning, inside_lower, true);
        result.inside[axis] = f;
        result.unit = result.unit && f.fixed == one;
    }
    return result;
}

fn tess_size(domain_patch: TessPatch, f: Factors) -> vec2<u32> {
    if f.culled {
        return vec2<u32>();
    }
    if domain_patch.domain == 2u {
        let lines = segments(f.outside[0]);
        let steps = segments(f.outside[1]);
        let vertices = lines * (steps + 1u);
        return vec2<u32>(vertices, select(2u * lines * steps, vertices, domain_patch.output == 0u));
    }
    var vertices = f.boundary;
    if !f.unit {
        let u = segments(f.inside[0]);
        if domain_patch.domain == 0u {
            let rings = (u + 1u) / 2u - 1u;
            vertices += select(3u * rings * (rings + 1u) + 1u, 3u * rings * rings, f.inside[0].odd);
        } else {
            vertices += (u - 1u) * (segments(f.inside[1]) - 1u);
        }
    }
    let count = select(3u * (2u * vertices - f.boundary - 2u), vertices, domain_patch.output == 0u);
    return vec2<u32>(vertices, count);
}

@compute @workgroup_size(64) fn tess_count(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= tess_active() {
        return;
    }
    let domain_patch = patches[id.x];
    let size = tess_size(domain_patch, factors(domain_patch));
    ranges[id.x] = TessRange(size.x, size.y, 0u, 0u);
}

@compute @workgroup_size(1) fn tess_scan() {
    var start = vec2<u32>();
    for (var i = 0u; i < tess_active(); i++) {
        var range = ranges[i];
        range.first_point = start.x;
        range.first_index = start.y;
        start += vec2<u32>(range.points, range.indices);
        ranges[i] = range;
    }
}

var<private> point_base: u32;
var<private> index_base: u32;
var<private> point_count: u32;
var<private> index_count: u32;
var<private> output_topology: u32;
var<private> patch_index: u32;

fn point(location: vec3<u32>) {
    points[point_base + point_count] = vec4<u32>(bitcast<vec3<u32>>(vec3<f32>(location) * epsilon), patch_index);
    point_count++;
}

fn index(at: u32) {
    indices[index_base + index_count] = point_base + at;
    index_count++;
}

fn triangle(a: u32, b: u32, c: u32) {
    index(a);
    index(select(b, c, output_topology == 3u));
    index(select(c, b, output_topology == 3u));
}

struct Side {
    start: u32,
    end: u32,
    segments: u32,
    reversed: bool,
}

fn straight(start: u32, steps: u32) -> Side {
    return Side(start, start + steps, steps, false);
}

fn reverse(side: Side) -> Side {
    return Side(side.start, side.end, side.segments, !side.reversed);
}

fn side_index(side: Side, at: u32) -> u32 {
    let n = select(at, side.segments - at, side.reversed);
    return select(side.start + n, side.end, n == side.segments);
}

fn ring(base: u32, steps: vec4<u32>, n: u32) -> array<Side, 4> {
    var sides: array<Side, 4>;
    var start = base;
    for (var i = 0u; i < n; i++) {
        sides[i] = straight(start, steps[i]);
        start += steps[i];
    }
    sides[n - 1u].end = base;
    return sides;
}

fn regular(inside: Side, outside: Side, trapezoid: bool, diagonals: u32) {
    let offset = u32(trapezoid);
    if trapezoid {
        triangle(side_index(outside, 0u), side_index(outside, 1u), side_index(inside, 0u));
    }
    for (var p = 0u; p < inside.segments; p++) {
        let i0 = side_index(inside, p);
        let i1 = side_index(inside, p + 1u);
        let o0 = side_index(outside, p + offset);
        let o1 = side_index(outside, p + offset + 1u);
        if (diagonals == 1u && p == inside.segments / 2u)
                || (diagonals == 2u && p < (inside.segments + 1u) / 2u) {
            triangle(o0, i1, i0);
            triangle(o0, o1, i1);
        } else {
            triangle(i0, o0, o1);
            triangle(i0, o1, i1);
        }
    }
    if trapezoid {
        let end = outside.segments;
        triangle(side_index(outside, end - 1u), side_index(outside, end), side_index(inside, inside.segments));
    }
}

const positions = array<u32, 33>(
    0, 32, 16, 8, 17, 4, 18, 9, 19, 2, 20, 10, 21, 5, 22, 11, 23, 1, 24, 12, 25, 6,
    26, 13, 27, 3, 28, 14, 29, 7, 30, 15, 31,
);

fn transition(inside: Side, outside: Side, inner: TessFactor, outer: TessFactor) {
    let inner_half = inner.half_points - u32(inner.odd);
    let outer_half = outer.half_points - u32(outer.odd);
    var i = 0u;
    var o = 0u;
    if outer_half != 0u {
        triangle(side_index(outside, o), side_index(outside, o + 1u), side_index(inside, i));
        o++;
    }
    for (var p = 1u; p < 33u; p++) {
        let position = positions[p];
        if position < inner_half {
            triangle(side_index(inside, i), side_index(outside, o), side_index(inside, i + 1u));
            i++;
        }
        if position < outer_half {
            triangle(side_index(outside, o), side_index(outside, o + 1u), side_index(inside, i));
            o++;
        }
    }
    if inner.odd {
        triangle(side_index(inside, i), side_index(outside, o), side_index(inside, i + 1u));
        i++;
    }
    if outer.odd {
        triangle(side_index(inside, i), side_index(outside, o), side_index(outside, o + 1u));
        o++;
    }
    for (var p = 32u; p != 0u; p--) {
        let position = positions[p];
        if position < outer_half {
            triangle(side_index(outside, o), side_index(outside, o + 1u), side_index(inside, i));
            o++;
        }
        if position < inner_half {
            triangle(side_index(inside, i), side_index(outside, o), side_index(inside, i + 1u));
            i++;
        }
    }
    if outer_half != 0u {
        triangle(side_index(outside, o), side_index(outside, o + 1u), side_index(inside, i));
    }
}

fn stitch(inside: array<Side, 4>, outside: array<Side, 4>,
          inner: array<TessFactor, 4>, outer: array<TessFactor, 4>, n: u32, first: bool) {
    if output_topology == 0u {
        return;
    }
    for (var edge = 0u; edge < n; edge++) {
        if first {
            transition(inside[edge], outside[edge], inner[edge], outer[edge]);
        } else {
            regular(inside[edge], outside[edge], true, 2u);
        }
    }
}

fn tri(f: Factors) {
    if f.unit {
        point(vec3<u32>(0u, one, 0u));
        point(vec3<u32>(0u, 0u, one));
        point(vec3<u32>(one, 0u, 0u));
        if output_topology != 0u {
            triangle(0u, 1u, 2u);
        }
        return;
    }
    let outside = f.outside;
    let inside = f.inside[0];
    var previous = ring(0u, vec4<u32>(segments(outside[0]), segments(outside[1]), segments(outside[2]), 0u), 3u);
    for (var edge = 0u; edge < 3u; edge++) {
        let n = segments(outside[edge]);
        for (var p = 0u; p < n; p++) {
            let at = location(outside[edge], select(n - p, p, edge == 1u));
            let u = select(at, 0u, edge == 0u);
            let v = select(select(one - at, 0u, edge == 1u), at, edge == 0u);
            point(vec3<u32>(u, v, one - u - v));
        }
    }
    let n = segments(inside);
    for (var r = 1u; r <= n / 2u; r++) {
        let side = n - 2u * r;
        let current = ring(point_count, vec4<u32>(side), 3u);
        if side == 0u {
            point(vec3<u32>(0x5555u, 0x5555u, one - 2u * 0x5555u));
        } else {
            let inset = (location(inside, r) * 0xaaaau + half_fixed) >> 16u;
            for (var edge = 0u; edge < 3u; edge++) {
                for (var p = r; p < n - r; p++) {
                    let at = location(inside, select(n - p, p, edge == 1u)) - (inset + 1u) / 2u;
                    let u = select(at, inset, edge == 0u);
                    let v = select(select(one - at - inset, inset, edge == 1u), at, edge == 0u);
                    point(vec3<u32>(u, v, one - u - v));
                }
            }
        }
        stitch(current, previous, array<TessFactor, 4>(inside, inside, inside, inside), outside, 3u, r == 1u);
        previous = current;
    }
    if inside.odd && output_topology != 0u {
        triangle(side_index(previous[0], 0u), side_index(previous[1], 0u), side_index(previous[2], 0u));
    }
}

fn quad(f: Factors) {
    if f.unit {
        point(vec3<u32>(0u, 0u, 0u));
        point(vec3<u32>(one, 0u, 0u));
        point(vec3<u32>(one, one, 0u));
        point(vec3<u32>(0u, one, 0u));
        if output_topology != 0u {
            triangle(0u, 1u, 3u);
            triangle(1u, 2u, 3u);
        }
        return;
    }
    let outside = f.outside;
    let inside = f.inside;
    var previous = ring(0u, vec4<u32>(segments(outside[0]), segments(outside[1]), segments(outside[2]), segments(outside[3])), 4u);
    for (var edge = 0u; edge < 4u; edge++) {
        let n = segments(outside[edge]);
        for (var p = 0u; p < n; p++) {
            let at = location(outside[edge], select(n - p, p, edge == 1u || edge == 2u));
            switch edge {
                case 0u: { point(vec3<u32>(0u, at, 0u)); }
                case 1u: { point(vec3<u32>(at, 0u, 0u)); }
                case 2u: { point(vec3<u32>(one, at, 0u)); }
                default: { point(vec3<u32>(at, one, 0u)); }
            }
        }
    }

    let u = segments(inside[0]);
    let v = segments(inside[1]);
    let inside_sides = array<TessFactor, 4>(inside[1], inside[0], inside[1], inside[0]);
    for (var r = 1u; r <= min(u, v) / 2u; r++) {
        let base = point_count;
        let width = u - 2u * r;
        let height = v - 2u * r;
        var current: array<Side, 4>;
        if height == 0u {
            for (var p = r; p <= u - r; p++) {
                point(vec3<u32>(location(inside[0], p), half_fixed, 0u));
            }
            let row = straight(base, width);
            current = array<Side, 4>(straight(base, 0u), row, straight(base + width, 0u), reverse(row));
        } else if width == 0u {
            for (var p = v - r; p >= r; p--) {
                point(vec3<u32>(half_fixed, location(inside[1], p), 0u));
            }
            let row = straight(base, height);
            current = array<Side, 4>(row, straight(base + height, 0u), reverse(row), straight(base, 0u));
        } else {
            for (var edge = 0u; edge < 4u; edge++) {
                let f = inside_sides[edge];
                let n = segments(f);
                let other = inside[edge % 2u];
                let inset = location(other, select(segments(other) - r, r, edge < 2u));
                for (var p = r; p < n - r; p++) {
                    let at = location(f, select(n - p, p, edge == 1u || edge == 2u));
                    point(select(vec3<u32>(inset, at, 0u), vec3<u32>(at, inset, 0u), (edge & 1u) != 0u));
                }
            }
            current = ring(base, vec4<u32>(height, width, height, width), 4u);
        }
        stitch(current, previous, inside_sides, outside, 4u, r == 1u);
        previous = current;
    }
    if output_topology != 0u {
        if u > v && inside[1].odd {
            regular(reverse(previous[3]), previous[1], false, 0u);
        } else if v >= u && inside[0].odd {
            regular(reverse(previous[2]), previous[0], false, u32(inside[1].odd));
        }
    }
}

fn isoline(f: Factors) {
    let lines = segments(f.outside[0]);
    let steps = segments(f.outside[1]);
    for (var line = 0u; line < lines; line++) {
        let v = location(f.outside[0], line);
        let start = point_count;
        for (var p = 0u; p <= steps; p++) {
            point(vec3<u32>(location(f.outside[1], p), v, 0u));
        }
        if output_topology != 0u {
            for (var p = 0u; p < steps; p++) {
                index(start + p);
                index(start + p + 1u);
            }
        }
    }
}

@compute @workgroup_size(64) fn tess_emit(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= tess_active() {
        return;
    }
    let range = ranges[id.x];
    if range.points == 0u {
        return;
    }
    let domain_patch = patches[id.x];
    let f = factors(domain_patch);
    point_base = range.first_point;
    index_base = range.first_index;
    output_topology = domain_patch.output;
    patch_index = id.x;
    switch domain_patch.domain {
        case 0u: { tri(f); }
        case 1u: { quad(f); }
        default: { isoline(f); }
    }
    if output_topology == 0u {
        for (var i = 0u; i < point_count; i++) {
            index(i);
        }
    }
}
