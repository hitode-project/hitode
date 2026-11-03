// Shared software-source decoder; composition stages can call software_pixel inline.
@group(3) @binding(0) var<storage, read> pixels: array<u32>;
// Layout, 256 packed BGRA palette entries, then tile rectangles.
@group(3) @binding(1) var<storage, read> parameters: array<vec4<u32>>;

fn byte_at(at: u32) -> u32 {
    return (pixels[at / 4u] >> ((at % 4u) * 8u)) & 255u;
}
fn software_pixel(x: u32, y: u32) -> vec4<f32> {
    let surface = parameters[0];
    let row = y * surface.z;
    let kind = surface.w;
    // Direct formats keep their channels apart. Assembling them into one u32
    // first is miscompiled on Adreno 750, which sign-extends bit 15 into the
    // upper channels.
    var bgra = vec4<u32>(0u);
    if kind == 0u {
        let at = row + x * 4u;
        bgra = vec4(byte_at(at), byte_at(at + 1u), byte_at(at + 2u), byte_at(at + 3u));
    } else if kind == 1u {
        let at = row + x * 3u;
        bgra = vec4(byte_at(at), byte_at(at + 1u), byte_at(at + 2u), 0u);
    } else if kind <= 3u {
        let at = row + x * 2u;
        let raw = byte_at(at) | (byte_at(at + 1u) << 8u);
        bgra = vec4((raw & 31u) * 255u / 31u, ((raw >> 5u) & 31u) * 255u / 31u,
            ((raw >> 10u) & 31u) * 255u / 31u, 0u);
        if kind == 2u {
            bgra.y = ((raw >> 5u) & 63u) * 255u / 63u;
            bgra.z = ((raw >> 11u) & 31u) * 255u / 31u;
        }
    } else {
        var index = 0u;
        if kind == 5u {
            index = (byte_at(row + x / 2u) >> (4u - (x % 2u) * 4u)) & 15u;
        } else if kind == 6u {
            index = (byte_at(row + x / 8u) >> (7u - x % 8u)) & 1u;
        }
        else { index = byte_at(row + x); }
        return unpack4x8unorm(parameters[1u + index / 4u][index % 4u]).bgra;
    }
    return vec4<f32>(bgra.zyxw) / 255.0;
}
fn software_vertex(vertex: u32, tile: u32) -> vec4<f32> {
    let corners = array<vec2<u32>, 6>(vec2(0u, 0u), vec2(1u, 0u), vec2(0u, 1u),
        vec2(0u, 1u), vec2(1u, 0u), vec2(1u, 1u));
    let rect = parameters[65u + tile];
    let xy = vec2<f32>(rect.xy + corners[vertex] * rect.zw) / vec2<f32>(parameters[0].xy);
    return vec4<f32>(xy.x * 2.0 - 1.0, 1.0 - xy.y * 2.0, 0.0, 1.0);
}
