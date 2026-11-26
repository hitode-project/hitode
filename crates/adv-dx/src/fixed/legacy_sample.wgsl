fn legacy_tap(at: vec2<i32>, level: i32, config: vec4<u32>,
    masks: vec4<u32>, key: vec4<u32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(image, level));
    if (((config.x & 1u) != 0u && (at.x < 0 || at.x >= size.x)) ||
        ((config.x & 2u) != 0u && (at.y < 0 || at.y >= size.y))) {
        return unpack4x8unorm(config.y);
    }
    let xy = vec2(adv_address_texel(at.x, size.x, (config.x >> 4u) & 3u),
        adv_address_texel(at.y, size.y, (config.x >> 6u) & 3u));
    let raw = legacy_raw(textureLoad(image, xy, level));
    if (key.z & 1u) != 0u && (raw & key.y) == (key.x & key.y) { discard; }
    return vec4(legacy_channel(raw, masks.x), legacy_channel(raw, masks.y),
        legacy_channel(raw, masks.z), legacy_channel(raw, masks.w));
}
fn legacy_level(uv: vec2<f32>, level: i32, linear: bool,
    config: vec4<u32>, masks: vec4<u32>, key: vec4<u32>) -> vec4<f32> {
    let dimensions = vec2<f32>(textureDimensions(image, level));
    if !linear {
        return legacy_tap(vec2<i32>(floor(uv * dimensions)), level, config, masks, key);
    }
    let point = uv * dimensions - 0.5;
    let lower = vec2<i32>(floor(point));
    let f = fract(point);
    var value = legacy_tap(lower, level, config, masks, key) * (1.0 - f.x) * (1.0 - f.y);
    if f.x != 0.0 {
        value += legacy_tap(lower + vec2(1, 0), level, config, masks, key) * f.x * (1.0 - f.y);
    }
    if f.y != 0.0 {
        value += legacy_tap(lower + vec2(0, 1), level, config, masks, key) * (1.0 - f.x) * f.y;
        if f.x != 0.0 {
            value += legacy_tap(lower + vec2(1, 1), level, config, masks, key) * f.x * f.y;
        }
    }
    return value;
}
fn legacy_sample(uv: vec2<f32>, config: vec4<u32>,
    masks: vec4<u32>, key: vec4<u32>) -> vec4<f32> {
    if (key.z & 2u) == 0u { return vec4<f32>(1.0); }
    let size = vec2<f32>(textureDimensions(image, 0));
    let dx = dpdx(uv) * size;
    let dy = dpdy(uv) * size;
    let lod = 0.5 * log2(max(max(dot(dx, dx), dot(dy, dy)), 1e-30)) + bitcast<f32>(key.w);
    let linear = select((config.w & 1u) != 0u, (config.w & 2u) != 0u, lod <= 0.0);
    let last = textureNumLevels(image) - 1u;
    let first = min(config.z, last);
    if (config.w & 8u) == 0u { return legacy_level(uv, i32(first), linear, config, masks, key); }
    let permitted = clamp(lod, f32(first), f32(last));
    if (config.w & 4u) == 0u {
        return legacy_level(uv, i32(floor(permitted + 0.5)), linear, config, masks, key);
    }
    let lower = floor(permitted);
    let value = legacy_level(uv, i32(lower), linear, config, masks, key);
    if lower == permitted { return value; }
    return mix(value, legacy_level(uv, i32(lower) + 1, linear, config, masks, key), fract(permitted));
}
