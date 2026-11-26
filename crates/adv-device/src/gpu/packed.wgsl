// Packed surface conversion uses nearest quantization and bit replication.
fn packed_raw(p: vec4<f32>) -> u32 {
    let b = vec4<u32>(round(p * 255.0));
    return b.z | (b.y << 8u) | (b.x << 16u) | (b.w << 24u);
}
fn packed_channel(raw: u32, mask: u32) -> f32 {
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
fn packed_encode(color: vec4<f32>, masks: vec4<u32>) -> u32 {
    let shift = vec4<u32>(countTrailingZeros(masks.x), countTrailingZeros(masks.y), countTrailingZeros(masks.z), countTrailingZeros(masks.w)) & vec4<u32>(31);
    let scaled = clamp(color, vec4<f32>(0.0), vec4<f32>(1.0)) * vec4<f32>(masks >> shift);
    let bits = vec4<u32>(ceil(scaled - 0.5)) << shift;
    return bits.x | bits.y | bits.z | bits.w;
}
fn packed_decode(raw: u32, masks: vec4<u32>) -> vec4<f32> {
    return vec4(packed_channel(raw, masks.x), packed_channel(raw, masks.y),
                packed_channel(raw, masks.z), packed_channel(raw, masks.w));
}
fn packed_storage_masks(masks: vec4<u32>) -> vec4<u32> {
    // The X1R5G5B5 high bit is held in the backing's alpha channel.
    let x1 = select(0u, 0x8000u, (masks.x | masks.y | masks.z | masks.w) == 0x7fffu);
    return masks | vec4(0u, 0u, 0u, x1);
}
