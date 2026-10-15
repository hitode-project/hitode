//! A partial clear is a neutral draw on the existing renderer and upload pool.
//! Full attachments retain the render-pass clear fast path.
use super::*;

impl Gpu {
    pub fn clear_region(
        &mut self,
        target: &Target,
        mut rectangle: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        self.own_target(target)?;
        let invalid = |message: &str| Error::Invalid(message.into());
        if color.is_some_and(|c| c.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)))
            || depth.is_some_and(|z| !z.is_finite() || !(0.0..=1.0).contains(&z))
            || (depth.is_some() && target.depth().is_none())
            || (stencil.is_some() && target.depth().is_none_or(|d| !d.format().has_stencil()))
        {
            return Err(invalid("clear value or depth attachment is invalid"));
        }
        if color.is_none() && depth.is_none() && stencil.is_none() {
            return Err(invalid("clear has no selected component"));
        }
        let width = target.image.width();
        let height = target.image.height();
        if rectangle
            .x
            .checked_add(rectangle.w)
            .is_none_or(|x| x > width)
            || rectangle
                .y
                .checked_add(rectangle.h)
                .is_none_or(|y| y > height)
        {
            return Err(invalid("clear rectangle is outside the target"));
        }
        if let Some(clip) = self.clip {
            rectangle = rectangle.clipped(clip);
        }
        // Even an empty command must validate ownership, arguments and the
        // selected attachment. A valid no-op neither writes nor acquires a fence.
        if rectangle.w == 0 || rectangle.h == 0 {
            return Ok(());
        }
        if !target.padded_attachments()
            && rectangle
                == (Rect {
                    x: 0,
                    y: 0,
                    w: width,
                    h: height,
                })
        {
            return self.clear_with_stencil(target, color, depth, stencil);
        }

        let mut state = DrawState::new(width, height);
        state.scissor = Some(rectangle);
        state.color_write = if color.is_some() { 15 } else { 0 };
        if depth.is_some() {
            state.depth = Some(Compare::Always);
            state.depth_write = true;
        }
        if let Some(value) = stencil {
            let face = StencilFace {
                compare: Compare::Always,
                fail: StencilOp::Keep,
                depth_fail: StencilOp::Keep,
                pass: StencilOp::Replace,
            };
            state.stencil = Some(Stencil {
                front: face,
                back: face,
                read_mask: u32::MAX,
                write_mask: u32::MAX,
            });
            state.stencil_reference = value & 255;
        }
        let mut rgba = color.unwrap_or([0.; 4]).map(|v| v as f32);
        if target.image.alpha_mode() == AlphaMode::Opaque {
            rgba[3] = 1.;
        }
        let vertex = |x, y| Vertex {
            position: [x, y, depth.unwrap_or(0.), 1.],
            color: rgba,
            uv: [0.; 2],
        };
        let vertices = [
            vertex(-1., 1.),
            vertex(-1., -1.),
            vertex(1., 1.),
            vertex(1., 1.),
            vertex(-1., -1.),
            vertex(1., -1.),
        ];
        // The full-target viewport has depth range 0..1. The caller's viewport
        // supplies only the scissor, so its MinZ/MaxZ cannot remap the clear value.
        self.draw(target, &vertices, None, state)
    }
}

#[cfg(test)]
mod tests;
