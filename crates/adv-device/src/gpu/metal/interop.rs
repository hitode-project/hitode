use super::*;
use objc2_foundation::NSString;
use objc2_metal::*;

// spec: GFX-METAL-1
#[test]
fn wgpu_clear_metal_triangle_wgpu_readback() {
    let mut gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    let target = gpu.target(4, 4, false).unwrap();
    gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
    let context = gpu.context.clone();
    let submission = context.submission().unwrap();
    submission.flush();
    unsafe {
        let device = context.device.as_hal::<wgpu::hal::api::Metal>().unwrap();
        let queue = context.queue.as_hal::<wgpu::hal::api::Metal>().unwrap();
        let texture = target
            .image
            .texture
            .as_hal::<wgpu::hal::api::Metal>()
            .unwrap();
        let source = NSString::from_str(
            r#"
#include <metal_stdlib>
using namespace metal;
vertex float4 vertex_main(uint id [[vertex_id]]) {
    const float2 p[] = {float2(-1, -1), float2(1, -1), float2(-1, 1)};
    return float4(p[id], 0, 1);
}
fragment float4 fragment_main() { return float4(0, 1, 0, 1); }
"#,
        );
        let library = device
            .raw_device()
            .newLibraryWithSource_options_error(&source, None)
            .unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        descriptor.setVertexFunction(
            library
                .newFunctionWithName(&NSString::from_str("vertex_main"))
                .as_deref(),
        );
        descriptor.setFragmentFunction(
            library
                .newFunctionWithName(&NSString::from_str("fragment_main"))
                .as_deref(),
        );
        descriptor
            .colorAttachments()
            .objectAtIndexedSubscript(0)
            .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        let pipeline = device
            .raw_device()
            .newRenderPipelineStateWithDescriptor_error(&descriptor)
            .unwrap();
        let pass = MTLRenderPassDescriptor::new();
        let color = pass.colorAttachments().objectAtIndexedSubscript(0);
        color.setTexture(Some(texture.raw_handle()));
        color.setLoadAction(MTLLoadAction::Load);
        color.setStoreAction(MTLStoreAction::Store);
        let command = queue.as_raw().commandBuffer().unwrap();
        let encoder = command.renderCommandEncoderWithDescriptor(&pass).unwrap();
        encoder.setRenderPipelineState(&pipeline);
        encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        encoder.endEncoding();
        command.commit();
    }
    drop(submission);
    let mut readback = gpu.readback(&target.image, None).unwrap();
    context
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
        .unwrap();
    let pixels = readback.poll().unwrap().unwrap();
    let expected: Vec<u8> = (0..4)
        .flat_map(|y| {
            (0..4).flat_map(move |x| {
                if x < y {
                    [0, 255, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
        })
        .collect();
    assert_eq!(pixels, expected);
    gpu.check().unwrap();
}
