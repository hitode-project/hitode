//! Swap-chain descriptions and window-dependent present results.
use super::{
    Error,
    abi::{DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_CHAIN_DESC1},
    caps::{SwapCoverage, SwapEffect, SwapModel},
};
use adv_gpu::Caps;

#[derive(Clone, Copy)]
pub struct Window {
    pub client: [u32; 2],
    pub minimized: bool,
}

#[derive(Clone, Copy)]
pub struct Description {
    desc: DXGI_SWAP_CHAIN_DESC1,
    model: SwapModel,
}

impl Description {
    pub fn legacy(
        desc: DXGI_SWAP_CHAIN_DESC,
        window: Window,
        caps: &Caps,
        coverage: SwapCoverage,
    ) -> Result<Self, Error> {
        Self::new(
            DXGI_SWAP_CHAIN_DESC1 {
                Width: desc.BufferDesc.Width,
                Height: desc.BufferDesc.Height,
                Format: desc.BufferDesc.Format,
                SampleDesc: desc.SampleDesc,
                BufferUsage: desc.BufferUsage,
                BufferCount: desc.BufferCount,
                SwapEffect: desc.SwapEffect,
                Flags: desc.Flags,
                ..Default::default()
            },
            window,
            caps,
            coverage,
        )
    }

    pub fn new(
        mut desc: DXGI_SWAP_CHAIN_DESC1,
        window: Window,
        caps: &Caps,
        coverage: SwapCoverage,
    ) -> Result<Self, Error> {
        let effect = match desc.SwapEffect {
            0 => SwapEffect::Discard,
            1 => SwapEffect::Sequential,
            3 => SwapEffect::FlipSequential,
            4 => SwapEffect::FlipDiscard,
            _ => return Err(Error::InvalidCall),
        };
        let model = effect.model();
        if !model.supports(
            caps,
            coverage,
            desc.Format as u32,
            desc.BufferCount,
            desc.SampleDesc.Count,
            desc.Flags,
        ) {
            return Err(match effect {
                SwapEffect::Discard | SwapEffect::Sequential => Error::InvalidArgument,
                SwapEffect::FlipSequential | SwapEffect::FlipDiscard => Error::InvalidCall,
            });
        }
        if desc.Stereo != 0
            || !(0..=2).contains(&desc.Scaling)
            || !(0..=3).contains(&desc.AlphaMode)
        {
            return Err(Error::InvalidCall);
        }
        if desc.SampleDesc.Quality != 0 {
            return Err(Error::InvalidCall);
        }
        if desc.Width == 0 {
            desc.Width = window.client[0];
        }
        if desc.Height == 0 {
            desc.Height = window.client[1];
        }
        Ok(Self { desc, model })
    }

    pub fn desc(&self) -> DXGI_SWAP_CHAIN_DESC1 {
        self.desc
    }

    pub fn buffer(&self, index: u32) -> Result<u32, Error> {
        if index >= self.desc.BufferCount || (index != 0 && !self.model.sequential_buffers) {
            return Err(Error::InvalidCall);
        }
        Ok(index)
    }

    pub fn resize(
        &self,
        resize: Resize,
        window: Window,
        caps: &Caps,
        coverage: SwapCoverage,
    ) -> Result<Self, Error> {
        if resize.referenced {
            return Err(Error::InvalidCall);
        }
        let mut desc = self.desc;
        if resize.buffers != 0 {
            desc.BufferCount = resize.buffers;
        }
        if resize.format != 0 {
            desc.Format = resize.format;
        }
        desc.Width = resize.size[0];
        desc.Height = resize.size[1];
        desc.Flags = resize.flags;
        Self::new(desc, window, caps, coverage)
    }

    pub fn present_status(&self, window: Window) -> u32 {
        if self.model.occluded_status && (window.minimized || window.client.contains(&0)) {
            0x087a0001
        } else {
            0
        }
    }
}

pub struct Resize {
    pub buffers: u32,
    pub size: [u32; 2],
    pub format: i32,
    pub flags: u32,
    pub referenced: bool,
}

#[cfg(test)]
mod tests;

use crate::pacing::PresentQueue;
use adv_abi::types::{DXGI_MODE_DESC, DXGI_SWAP_CHAIN_FULLSCREEN_DESC};
use adv_abi::{Env, Outcome, com::Iid};
use adv_machine::{Ext, Objects};

/// The device front owns back-buffer objects and performs recorded presentation.
pub trait Buffers: Sync {
    fn identity(&self, env: &Env<'_>, device: u32) -> Result<u32, Error>;
    fn adapter(&self, env: &Env<'_>, device: u32) -> Result<super::caps::Adapter, Error>;
    fn validate_device(&self, env: &Env<'_>, device: u32) -> Result<(), Error> {
        self.identity(env, device).map(|_| ())
    }
    fn surface_owner(&self, env: &Env<'_>, pointer: u32) -> Result<(u32, u32), Error>;
    fn scanout(&self, env: &mut Env<'_>, surface: u32, serial: u64) -> Result<Option<u64>, Error>;
    #[cfg(feature = "gpu")]
    fn capture(&self, env: &mut Env<'_>) -> Result<adv_device::frame::Waker, Error>;
    #[cfg(feature = "gpu")]
    fn copy_display(&self, env: &mut Env<'_>, surface: u32) -> Result<(), Error>;
    #[cfg(feature = "gpu")]
    fn desktop_resource(
        &self,
        env: &mut Env<'_>,
        device: u32,
        mode: DXGI_MODE_DESC,
        frame: &adv_device::frame::Captured,
    ) -> Result<u32, Error>;
    fn caps<'a>(&self, ext: &'a Ext) -> Result<&'a Caps, Error>;
    fn drop_device_ref(&self, env: &mut Env<'_>, device: u32);
    fn retain_device(&self, env: &mut Env<'_>, device: u32) -> Result<(), Error>;
    fn create(
        &self,
        env: &mut Env<'_>,
        device: u32,
        desc: DXGI_SWAP_CHAIN_DESC1,
    ) -> Result<Vec<u32>, Error>;
    fn query(&self, env: &mut Env<'_>, pointer: u32, iid: Iid) -> Result<u32, Error>;
    fn referenced(&self, env: &Env<'_>, pointer: u32) -> bool;
    fn release(&self, env: &mut Env<'_>, pointer: u32, interfaces: &mut Vec<u32>);
    fn present(
        &self,
        env: &mut Env<'_>,
        resource: u32,
        size: [u32; 2],
        serial: u64,
        first: bool,
        composition: bool,
    ) -> Result<(bool, Option<u64>), Error>;
    /// How text records (WINE-UNIX-8) name a buffer's pixels, if they can.
    fn text_store(&self, env: &Env<'_>, buffer: u32) -> Option<adv_machine::text_records::Store> {
        let _ = (env, buffer);
        None
    }
    #[cfg(feature = "gpu")]
    fn present_wake_generation(&self, ext: &Ext) -> Option<u64>;
    #[cfg(feature = "gpu")]
    fn wait_for_present(
        &self,
        env: &mut Env<'_>,
        generation: u64,
        then: adv_machine::Continuation,
    ) -> Outcome;
    fn latency(&self, env: &Env<'_>, device: u32) -> u32;
    fn shared_adapter(&self, env: &Env<'_>, handle: u32) -> Result<super::caps::Adapter, Error>;
    fn output(&self, env: &mut Env<'_>, device: u32) -> Result<u32, Error>;
}

pub type ResizeWindow = fn(&mut Env<'_>, u32, DXGI_MODE_DESC, adv_machine::Continuation) -> Outcome;
pub type FrameCompleted = fn(&mut Env<'_>, u32, u64) -> Option<Outcome>;

pub struct Services {
    pub reported_build: u32,
    pub buffers: &'static dyn Buffers,
    pub window: fn(&Ext, &Objects, u32) -> Option<Window>,
    pub resize: ResizeWindow,
    pub completed: Option<FrameCompleted>,
}

pub(super) struct Chain {
    pub device: u32,
    pub buffers: Vec<u32>,
    pub description: Description,
    pub window: u32,
    pub composition: Option<u32>,
    pub matrix: adv_abi::types::DXGI_MATRIX_3X2_F,
    pub fullscreen: DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
    pub target: u32,
    pub generation: u64,
    pub queue: PresentQueue,
    pub background: [f32; 4],
    pub rotation: i32,
    pub source: [u32; 2],
    pub waitable: u32,
    pub latency: u32,
}

impl Chain {
    pub fn destroy(self, env: &mut Env<'_>, pointer: u32, interfaces: &mut Vec<u32>) {
        if self.window != 0
            && let std::collections::btree_map::Entry::Occupied(mut entry) = env
                .ext
                .get_mut::<super::state::State>()
                .chains
                .entry(self.window)
        {
            entry.get_mut().retain(|&chain| chain != pointer);
            if entry.get().is_empty() {
                entry.remove();
            }
        }
        if self.waitable != 0 {
            env.sys.objects.close(self.waitable);
        }
        let backend = env.ext.get::<Services>().buffers;
        for buffer in self.buffers {
            backend.release(env, buffer, interfaces);
        }
        interfaces.push(self.device);
        if self.target != 0 {
            interfaces.push(self.target);
        }
        if let Some(restrict) = self.composition.filter(|&restrict| restrict != 0) {
            interfaces.push(restrict);
        }
    }
}
