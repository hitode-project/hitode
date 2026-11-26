use super::*;
use crate::gpu::{DrawState, Program, ProgramTexture, ProgrammableDraw, Target};
use std::sync::LazyLock;

static BLT: LazyLock<Program> = LazyLock::new(|| {
    let mut program = Program::new(include_str!("blt.wgsl").into(), vec![false]);
    program.constants = Default::default();
    program
});

impl<P: Presenter> Surfaces<P> {
    pub(super) fn blt_gpu_fill(
        &mut self,
        destination: BltSurface,
        format: PixelFormat,
        value: u32,
        memory: &dyn SurfaceMemory,
    ) -> Result<(), String> {
        let image = self.gpu_access(destination.id, destination.rect, Access::Write, memory)?;
        let value = if format.packed() {
            value & raw_mask(format)
        } else {
            value
        };
        let mut bytes = value.to_le_bytes();
        bytes[format.bytes() as usize..].fill(0);
        let [b, g, r, a] = bytes.map(|v| f64::from(v) / 255.);
        self.gpu
            .clear_region(
                &Target::from_parts(image, None),
                destination.rect,
                Some([r, g, b, a]),
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
        self.surfaces.get_mut(&destination.id).unwrap().version += 1;
        Ok(())
    }

    pub(super) fn blt_gpu(
        &mut self,
        source: BltSurface,
        destination: BltSurface,
        params: Parameters,
        memory: &dyn SurfaceMemory,
    ) -> Result<(), String> {
        let sr = source.rect;
        let dr = destination.rect;
        let mut src = self.gpu_access(source.id, sr, Access::Read, memory)?;
        let dst = self.gpu_access(destination.id, dr, Access::Write, memory)?;
        let gpu = &mut self.gpu;
        let result = (|| {
            let origin = if src.shares_storage(&dst) {
                let scratch = gpu.target(sr.w, sr.h, false)?.image;
                gpu.copy(&src, sr, &scratch, 0, 0)?;
                src = scratch;
                [0, 0]
            } else {
                [sr.x, sr.y]
            };
            if params.direct_copy() {
                let [x, y] = origin;
                return gpu.copy(&src, Rect { x, y, ..sr }, &dst, dr.x, dr.y);
            }
            let mut parameters = [[0; 4]; 40];
            parameters[0] = [origin[0], origin[1], dr.x, dr.y];
            parameters[1] = params.x.parameters();
            parameters[2] = params.y.parameters();
            parameters[3] = [dr.w, dr.h, params.mask, params.conversion as u32];
            parameters[4] = [
                u32::from(params.key.is_some()),
                params.key.unwrap_or(0),
                0,
                0,
            ];
            let mut state = DrawState::new(dst.width(), dst.height());
            state.scissor = Some(dr);
            let draw = ProgrammableDraw {
                instance_count: 1,
                program: BLT.clone(),
                streams: Vec::new(),
                indices: None,
                vertex_count: 3,
                vertex_constants: Default::default(),
                pixel_constants: Default::default(),
                textures: vec![ProgramTexture {
                    image: Some(src),
                    sampler: state.sampler,
                }],
                parameters,
            };
            gpu.draw_programmable(&Target::from_parts(dst, None), &draw, &state)
        })();
        result.map_err(|error| error.to_string())?;
        self.surfaces.get_mut(&destination.id).unwrap().version += 1;
        Ok(())
    }
}
