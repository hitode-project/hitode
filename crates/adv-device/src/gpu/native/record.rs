//! Package C2: guest operations as [`super::stream::Command`]s. Each method
//! resolves its pipeline and bindings, writes its data into the ring and
//! records the pixels it may change, the input [`super::pass`] needs.

use super::super::{DrawState, Error, Filter, ProgrammableDraw, Rect, Vertex};
use super::{Gpu, Image, Target};
use crate::frame::Damage;
use std::sync::Arc;

/// A copy of an image the guest reads back.
pub struct Readback {
    pub width: u32,
    pub height: u32,
}

impl Readback {
    pub fn cancel(&self) {
        todo!("C2")
    }
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, Error>> {
        todo!("C2")
    }
    pub fn take_completion_job(&mut self) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        todo!("C2")
    }
}

impl Gpu {
    pub fn clear(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
    ) -> Result<(), Error> {
        self.clear_with_stencil(target, color, depth, None)
    }
    /// D3D9 `Clear` with rectangles: clears `rectangle` of the attachments.
    pub fn clear_region(
        &mut self,
        target: &Target,
        rectangle: Rect,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        let _ = (target, rectangle, color, depth, stencil);
        todo!("C2")
    }
    pub fn clear_with_stencil(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        let _ = (target, color, depth, stencil);
        todo!("C2")
    }
    pub fn draw(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        image: Option<&Image>,
        state: DrawState,
    ) -> Result<(), Error> {
        let _ = (target, vertices, image, state);
        todo!("C2")
    }
    pub fn draw_programmable(
        &mut self,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
    ) -> Result<(), Error> {
        let _ = (target, draw, state);
        todo!("C2")
    }
    pub fn blit(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
    ) -> Result<(), Error> {
        let _ = (source, rectangle, destination, region, filter);
        todo!("C2")
    }
    pub fn copy(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
    ) -> Result<(), Error> {
        let _ = (source, rectangle, destination, x, y);
        todo!("C2")
    }
    /// End the guest frame: submit and hand `image` to presentation.
    /// `damage` is what changed since the previous present of `image`.
    pub fn present(&mut self, image: &Image, damage: Damage, serial: u64) -> Result<(), Error> {
        let _ = (image, damage, serial);
        todo!("C2")
    }
    /// Submit what is recorded without ending the frame (D3D9 query flushes).
    pub fn flush(&mut self) -> Result<(), Error> {
        todo!("C2")
    }
    pub fn readback(
        &mut self,
        image: &Image,
        wake: Option<Arc<adv_sync::WakeEvent>>,
    ) -> Result<Readback, Error> {
        let _ = (image, wake);
        todo!("C2")
    }
    pub fn set_readback_limit(&mut self, limit: usize) -> Result<(), Error> {
        let _ = limit;
        todo!("C2")
    }
    pub fn poll(&self) -> Result<(), Error> {
        todo!("C2")
    }
}
