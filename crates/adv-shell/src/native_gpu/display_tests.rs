use adv_device::gpu::{Gpu, GpuBackend};
use adv_device::surface::Rect;
use objc2::{msg_send, runtime::AnyObject};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSDictionary;
use objc2_metal::*;
use objc2_quartz_core::*;

// spec: HOST-VIEW-11
#[test]
fn shared_layer_preserves_rgb_independently_of_guest_alpha_and_output_color_space() {
    let mut gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    let context = gpu.context().clone();
    let device = unsafe { context.device.as_hal::<wgpu::hal::api::Metal>() }.unwrap();
    let device = device.raw_device();
    let queue = device.newCommandQueue().unwrap();
    let rect = CGRect::new(CGPoint::new(0., 0.), CGSize::new(5., 4.));
    for profile in unsafe {
        [
            objc2_core_graphics::kCGColorSpaceSRGB,
            objc2_core_graphics::kCGColorSpaceDisplayP3,
        ]
    } {
        let mut reference = None;
        for alpha in [false, true] {
            let target = gpu.presentation_target(5, 4, false).unwrap();
            let pixels: Vec<_> = (0u8..20)
                .flat_map(|i| {
                    [
                        i.wrapping_mul(31),
                        i * 7,
                        255 - i * 11,
                        if alpha { i * 13 } else { 255 },
                    ]
                })
                .collect();
            gpu.upload(
                &target.image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 5,
                    h: 4,
                },
                20,
                &pixels,
            )
            .unwrap();
            let frame = gpu.completed_frame(target.image, 1).unwrap();
            let wake = adv_sync::WakeEvent::new().unwrap();
            let fence = context
                .submission()
                .unwrap()
                .display_completion(wake.clone())
                .unwrap();
            while !fence.ready().unwrap() {
                assert!(wake.wait(Some(std::time::Duration::from_secs(10))).unwrap());
            }
            let layer = CALayer::new();
            layer.setFrame(rect);
            unsafe {
                let _: () = msg_send![&layer, setContentsOpaque: true];
                layer.setContents(Some(
                    &*(std::ptr::from_ref(frame.io_surface().unwrap()).cast::<AnyObject>()),
                ));
            }
            let root = CALayer::new();
            root.setFrame(rect);
            root.setBackgroundColor(Some(&objc2_core_graphics::CGColor::new_generic_gray(
                0., 1.,
            )));
            root.addSublayer(&layer);
            let descriptor = MTLTextureDescriptor::new();
            descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            descriptor.setStorageMode(MTLStorageMode::Shared);
            descriptor.setUsage(MTLTextureUsage::RenderTarget);
            unsafe {
                descriptor.setWidth(5);
                descriptor.setHeight(4);
            }
            let texture = device.newTextureWithDescriptor(&descriptor).unwrap();
            let color = objc2_core_graphics::CGColorSpace::with_name(Some(profile)).unwrap();
            let options = NSDictionary::from_slices(
                &unsafe { [kCARendererMetalCommandQueue, kCARendererColorSpace] },
                &unsafe {
                    [
                        &*(std::ptr::from_ref(&*queue).cast::<AnyObject>()),
                        &*(std::ptr::from_ref(&*color).cast::<AnyObject>()),
                    ]
                },
            );
            let renderer = unsafe {
                CARenderer::rendererWithMTLTexture_options(
                    &texture,
                    Some(&*(std::ptr::from_ref(&*options).cast::<NSDictionary>())),
                )
            };
            renderer.setLayer(Some(&root));
            renderer.setBounds(rect);
            CATransaction::flush();
            unsafe {
                renderer.beginFrameAtTime_timeStamp(CACurrentMediaTime(), std::ptr::null_mut())
            };
            renderer.addUpdateRect(rect);
            renderer.render();
            renderer.endFrame();
            let done = queue.commandBuffer().unwrap();
            done.commit();
            done.waitUntilCompleted();
            assert!(done.error().is_none());
            let mut output = [0u8; 80];
            unsafe {
                texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                    std::ptr::NonNull::from(&mut output).cast(),
                    20,
                    MTLRegion {
                        origin: MTLOrigin { x: 0, y: 0, z: 0 },
                        size: MTLSize {
                            width: 5,
                            height: 4,
                            depth: 1,
                        },
                    },
                    0,
                )
            };
            if let Some(reference) = reference {
                assert_eq!(output, reference);
            } else {
                reference = Some(output);
            }
            assert!(
                output
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[3] == 255)
            );
        }
    }
}
