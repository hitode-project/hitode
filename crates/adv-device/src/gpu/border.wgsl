// Portable arbitrary-border filtering. The caller supplies the unbounded LOD;
// choosing the min/mag filter happens before restricting the mip range.
// Address modes use Repeat=0, Mirror=1, Clamp=2, Border=3.
fn adv_address_texel(coordinate: i32, size: i32, mode: u32) -> i32 {
    switch mode {
        case 0u: { return ((coordinate % size) + size) % size; }
        case 1u: {
            let period = size * 2;
            let wrapped = ((coordinate % period) + period) % period;
            return select(wrapped, period - wrapped - 1, wrapped >= size);
        }
        default: { return clamp(coordinate, 0, size - 1); }
    }
}
fn adv_border_texel(image: texture_2d<f32>, coordinate: vec2<i32>, level: i32,
    address_flags: u32, border: vec4<f32>) -> vec4<f32> {
    let dimensions = vec2<i32>(textureDimensions(image, level));
    if (((address_flags & 1u) != 0u && (coordinate.x < 0 || coordinate.x >= dimensions.x)) ||
        ((address_flags & 2u) != 0u && (coordinate.y < 0 || coordinate.y >= dimensions.y))) {
        return border;
    }
    let addressed = vec2(adv_address_texel(coordinate.x, dimensions.x, (address_flags >> 4u) & 3u),
        adv_address_texel(coordinate.y, dimensions.y, (address_flags >> 6u) & 3u));
    return textureLoad(image, addressed, level);
}
fn adv_border_level(image: texture_2d<f32>, uv: vec2<f32>, level: i32,
    linear: bool, address_flags: u32, border: vec4<f32>) -> vec4<f32> {
    let dimensions = vec2<f32>(textureDimensions(image, level));
    if !linear {
        return adv_border_texel(image, vec2<i32>(floor(uv * dimensions)), level, address_flags, border);
    }
    let position = uv * dimensions - vec2(0.5);
    let lower = vec2<i32>(floor(position));
    let fraction = fract(position);
    let top = mix(adv_border_texel(image, lower, level, address_flags, border),
        adv_border_texel(image, lower + vec2(1, 0), level, address_flags, border), fraction.x);
    let bottom = mix(adv_border_texel(image, lower + vec2(0, 1), level, address_flags, border),
        adv_border_texel(image, lower + vec2(1, 1), level, address_flags, border), fraction.x);
    return mix(top, bottom, fraction.y);
}
fn adv_border_sample(image: texture_2d<f32>, uv: vec2<f32>, lod: f32,
    address_flags: u32, border_rgba: u32, first_level: u32, filter_flags: u32) -> vec4<f32> {
    let border = vec4<f32>(f32(border_rgba & 255u), f32((border_rgba >> 8u) & 255u),
        f32((border_rgba >> 16u) & 255u), f32(border_rgba >> 24u)) / 255.0;
    let linear = select((filter_flags & 1u) != 0u, (filter_flags & 2u) != 0u, lod <= 0.0);
    let last = textureNumLevels(image) - 1u;
    let first = min(first_level, last);
    if (filter_flags & 8u) == 0u {
        return adv_border_level(image, uv, i32(first), linear, address_flags, border);
    }
    let permitted_lod = clamp(lod, f32(first), f32(last));
    if (filter_flags & 4u) == 0u {
        return adv_border_level(image, uv, i32(floor(permitted_lod + 0.5)), linear, address_flags, border);
    }
    let lower = floor(permitted_lod);
    let upper = min(lower + 1.0, f32(last));
    return mix(adv_border_level(image, uv, i32(lower), linear, address_flags, border),
        adv_border_level(image, uv, i32(upper), linear, address_flags, border), fract(permitted_lod));
}
