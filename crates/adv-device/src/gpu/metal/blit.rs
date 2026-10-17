//! Native texture transfers retain the current command buffer.
use super::*;
impl State {
    pub(in crate::gpu::renderer) fn active(&self) -> bool {
        self.last_metal
    }
    pub(in crate::gpu::renderer) fn copy(
        &mut self,
        gpu: &Gpu,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
    ) {
        if !Arc::ptr_eq(&self.owner, &gpu.programmable.batch_stats) {
            self.flush();
            self.owner = gpu.programmable.batch_stats.clone();
        }
        if [source, destination]
            .iter()
            .any(|i| !i.metal.initialized.load(Ordering::Relaxed))
        {
            // These allocations were absent from the native batch. Queue their
            // initialization before it at the existing submission boundary.
            let encoder = self.initialization.get_or_insert_with(|| {
                gpu.context
                    .device
                    .create_command_encoder(&Default::default())
            });
            source.initialize_metal(encoder, &self.owner);
            destination.initialize_metal(encoder, &self.owner);
        }
        if let Some(encoder) = self.encoder.take() {
            encoder.endEncoding();
        }
        self.attachments = None;
        if self.command.is_none() {
            self.batch_start = self.ring.head;
        }
        let command = self
            .command
            .get_or_insert_with(|| self.queue.commandBuffer().unwrap());
        let encoder = command.blitCommandEncoder().unwrap();
        unsafe {
            // Release each wgpu guard before acquiring the next device snatch lock.
            let from: Object<dyn MTLTexture> = source
                .texture
                .as_hal::<wgpu::hal::api::Metal>()
                .unwrap()
                .raw_handle()
                .into();
            let to: Object<dyn MTLTexture> = destination
                .texture
                .as_hal::<wgpu::hal::api::Metal>()
                .unwrap()
                .raw_handle()
                .into();
            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &from, 0, source.mip as usize,
                MTLOrigin { x: rectangle.x as usize, y: rectangle.y as usize, z: 0 },
                MTLSize { width: rectangle.w as usize, height: rectangle.h as usize, depth: 1 },
                &to, 0, destination.mip as usize,
                MTLOrigin { x: x as usize, y: y as usize, z: 0 },
            );
        }
        encoder.endEncoding();
    }
}
