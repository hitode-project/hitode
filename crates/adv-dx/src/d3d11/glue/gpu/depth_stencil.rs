//! Pack native depth and stencil transfer planes into the guest row layout.

use adv_gpu::Format;

pub(super) fn pack_row(
    depth: &[u8],
    stencil: &[u8],
    d24: bool,
    format: Format,
    packed: &mut Vec<u8>,
) {
    let texels = depth.as_chunks::<4>().0.iter().zip(stencil);
    packed.clear();
    if d24 {
        let native_d24 = !cfg!(target_vendor = "apple") && format == Format::Depth24UnormStencil8;
        for (&depth, &stencil) in texels {
            let code = if native_d24 {
                // Vulkan transfers X8_D24_UNORM_PACK32 as integer depth codes.
                depth
            } else {
                // The backing stores float depth. Quantize once to D24.
                let value = f64::from(f32::from_le_bytes(depth));
                ((value * 16_777_215.0).round() as u32).to_le_bytes()
            };
            packed.extend_from_slice(&[code[0], code[1], code[2], stencil]);
        }
    } else {
        for (depth, &stencil) in texels {
            packed.extend_from_slice(depth);
            packed.extend_from_slice(&[stencil, 0, 0, 0]);
        }
    }
}
