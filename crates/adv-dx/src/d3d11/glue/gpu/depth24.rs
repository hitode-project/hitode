//! Pack the native depth and stencil transfer planes into a guest D24S8 row.

pub(super) fn pack_row(depth: &[u8], stencil: &[u8], packed: &mut [u8]) {
    for ((&depth, &stencil), output) in depth
        .as_chunks::<4>()
        .0
        .iter()
        .zip(stencil)
        .zip(packed.as_chunks_mut::<4>().0)
    {
        let bits = u32::from_le_bytes(depth);
        let code = if cfg!(target_vendor = "apple") {
            // Metal backs D24S8 with D32FS8. Round the float depth once to D24.
            (f64::from(f32::from_bits(bits)) * 16_777_215.0).round() as u32
        } else {
            // Vulkan copies X8_D24_UNORM_PACK32 without a normalized float load.
            bits
        };
        *output = code.to_le_bytes();
        output[3] = stencil;
    }
}
