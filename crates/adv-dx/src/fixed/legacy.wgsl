// Legacy DDPIXELFORMAT texels are uploaded without discarding their stored bits.
fn legacy_raw(p: vec4<f32>) -> u32 {
    let b = vec4<u32>(round(p * 255.0));
    return b.z | (b.y << 8u) | (b.x << 16u) | (b.w << 24u);
}
fn legacy_channel(raw: u32, mask: u32) -> f32 {
    if mask == 0u { return 1.0; }
    let shift = countTrailingZeros(mask);
    let width = countOneBits(mask);
    let value = (raw & mask) >> shift;
    if width == 1u { return f32(value); }
    if width == 4u { return f32((value << 4u) | value) / 255.0; }
    if width == 5u { return f32((value << 3u) | (value >> 2u)) / 255.0; }
    if width == 6u { return f32((value << 2u) | (value >> 4u)) / 255.0; }
    return f32(value) / f32(mask >> shift);
}
fn legacy_pack(color: vec4<f32>, masks: vec4<u32>) -> vec4<f32> {
    if masks.x == 0u { return color; }
    let shift = vec4<u32>(countTrailingZeros(masks.x), countTrailingZeros(masks.y), countTrailingZeros(masks.z), countTrailingZeros(masks.w)) & vec4<u32>(31);
    let bits = vec4<u32>(round(clamp(color, vec4<f32>(0.0), vec4<f32>(1.0)) * vec4<f32>(masks >> shift))) << shift;
    let raw = bits.x | bits.y | bits.z | bits.w;
    return vec4<f32>(f32((raw >> 16u) & 255u), f32((raw >> 8u) & 255u), f32(raw & 255u), f32(raw >> 24u)) / 255.0;
}
fn legacy_stage(op: u32, a: vec4<f32>, b: vec4<f32>, current: vec4<f32>,
    diffuse: vec4<f32>, texture: vec4<f32>) -> vec4<f32> {
    var result = current;
    switch op {
        case 2u: { return a; }
        case 3u: { return b; }
        case 4u: { result = a * b; }
        case 5u: { result = a * b * 2.0; }
        case 6u: { result = a * b * 4.0; }
        case 7u: { result = a + b; }
        case 8u: { result = a + b - 0.5; }
        case 9u: { result = (a + b - 0.5) * 2.0; }
        case 10u: { result = a - b; }
        case 11u: { result = a + b * (1.0 - a); }
        case 12u: { result = mix(b, a, diffuse.a); }
        case 13u: { result = mix(b, a, texture.a); }
        case 14u: { result = mix(b, a, bitcast<vec4<f32>>(draw_parameters.values[36]).a); }
        case 15u: { result = a + b * (1.0 - texture.a); }
        case 16u: { result = mix(b, a, current.a); }
        case 18u: { result = a + a.a * b; }
        case 19u: { result = a * b + a.a; }
        case 20u: { result = a + (1.0 - a.a) * b; }
        case 21u: { result = (1.0 - a) * b + a.a; }
        case 24u: { result = vec4<f32>(dot(a.rgb * 2.0 - 1.0, b.rgb * 2.0 - 1.0)); }
        default: {}
    }
    return clamp(result, vec4<f32>(0.0), vec4<f32>(1.0));
}
