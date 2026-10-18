use super::*;

// spec: GFX-METAL-1, GFX-REC-2
#[test]
fn iosurface_target_shares_completed_gpu_pixels_without_readback_or_copy() {
    let mut gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
    let target = gpu.presentation_target(37, 19, false).unwrap();
    gpu.clear(&target, Some([0.25, 0.5, 1., 0.]), None).unwrap();
    let frame = gpu.completed_frame(target.image.clone(), 1).unwrap();
    let surface = frame.io_surface().unwrap();
    assert_eq!((surface.width(), surface.height()), (37, 19));
    assert_eq!(surface.pixel_format(), u32::from_be_bytes(*b"BGRA"));
    assert_eq!(gpu.clear(&target, Some([0.; 4]), None), Err(Error::Busy));
    let wake = adv_sync::WakeEvent::new().unwrap();
    let completion = gpu
        .context()
        .submission()
        .unwrap()
        .display_completion(wake.clone())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !completion.ready().unwrap() {
        assert!(Instant::now() < deadline);
        wake.wait(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
    }
    // Explicit CPU access is a test oracle only; presentation does not lock or map.
    unsafe {
        assert_eq!(
            surface.lock(
                objc2_io_surface::IOSurfaceLockOptions::ReadOnly,
                std::ptr::null_mut()
            ),
            0
        );
        let bytes = std::slice::from_raw_parts(
            surface.base_address().cast::<u8>().as_ptr(),
            surface.bytes_per_row() * 19,
        );
        for row in bytes.chunks_exact(surface.bytes_per_row()) {
            assert!(
                row[..37 * 4]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [255, 128, 64, 0])
            );
        }
        assert_eq!(
            surface.unlock(
                objc2_io_surface::IOSurfaceLockOptions::ReadOnly,
                std::ptr::null_mut()
            ),
            0
        );
    }
    assert_eq!(gpu.stats().readbacks, 0);
    assert_eq!(gpu.stats().native_frame_allocations, 0);
    assert_eq!(gpu.stats().image_copy_bytes, 0);
    drop(frame);
    gpu.clear(&target, Some([1.; 4]), None).unwrap();
    assert_eq!(read(&mut gpu, &target.image), [255; 4].repeat(37 * 19));
}
