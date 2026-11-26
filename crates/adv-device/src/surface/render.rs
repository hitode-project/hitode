//! Share surface images with a recording graphics front.
use super::{Access, Presenter, Rect, SurfaceMemory, Surfaces};
use crate::gpu::{self, Gpu, Target};

impl<P: Presenter> Surfaces<P> {
    pub fn fork_gpu(&self) -> Result<Gpu, gpu::Error> {
        self.gpu.fork()
    }

    /// Obtain the color attachment and preserve the requested cells before a draw.
    pub fn render_access(
        &mut self,
        id: u32,
        rect: Rect,
        access: Access,
        memory: &dyn SurfaceMemory,
    ) -> Result<Target, String> {
        Ok(Target::from_parts(
            self.gpu_access(id, rect, access, memory)?,
            None,
        ))
    }
}
