//! Assembly dependencies and factory state.
use super::abi::DXGI_GAMMA_CONTROL;
use super::caps::{AdapterInfo, Display, PhysicalMode};
use adv_abi::{Env, Outcome, com::Com};
use adv_device::surface::SurfaceStore;
use adv_machine::{Ext, Objects};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

pub struct Provider {
    pub adapter: AdapterInfo,
    /// Borrow display facts from assembly when a method is called, including
    /// the DPI awareness of the calling context.
    pub display: fn(&Ext, u32) -> Option<Display>,
    pub window_exists: fn(&Ext, &Objects, u32) -> bool,
    pub next_vblank: fn(&Ext) -> Option<Duration>,
    pub devices: Option<&'static dyn Devices>,
}

/// Device-backed calls forwarded to the graphics device front.
pub trait Devices: Sync {
    fn invoke(&self, env: &mut Env<'_>, call: DeviceCall) -> Outcome;
}

#[derive(Clone, Copy, Debug)]
pub enum DeviceOperation {
    CreateSwapChain,
    CreateSwapChainForHwnd,
    CreateSwapChainForCoreWindow,
    CreateSwapChainForComposition,
    GetSharedResourceAdapterLuid,
    TakeOwnership,
    ReleaseOwnership,
    GetGammaControlCapabilities,
    SetGammaControl,
    GetGammaControl,
    SetDisplaySurface,
    GetDisplaySurfaceData,
    GetDisplaySurfaceData1,
    DuplicateOutput,
    DuplicateOutput1,
}

#[derive(Clone, Copy)]
pub struct DeviceCall {
    pub operation: DeviceOperation,
    pub this: u32,
    pub arguments: [u32; 6],
}

/// The current virtual display profile used by DirectDraw. An assembly with
/// real display facts supplies them through Provider instead.
pub fn shared_display(ext: &Ext, _: u32) -> Option<Display> {
    let store = ext.try_get::<SurfaceStore>()?;
    let size = (store.width, store.height);
    let modes = crate::ddraw::front::modes::sizes(size)
        .into_iter()
        .map(|size| PhysicalMode {
            size,
            refresh: (60, NonZeroU32::MIN),
            scaling: 0,
            scanline: 1,
            stereo: false,
        })
        .collect();
    Some(Display {
        name: r"\\.\DISPLAY1".into(),
        monitor: 1,
        physical_rect: [0, 0, size.0 as i32, size.1 as i32],
        dpi: NonZeroU32::new(96).unwrap(),
        dpi_aware: true,
        modes,
    })
}

pub(super) struct State {
    pub com: Com,
    pub factory_vtable: u32,
    pub adapter_vtable: u32,
    pub output_vtable: u32,
    pub swap_chain_vtable: u32,
    pub duplication_vtable: u32,
    pub output: Output,
    pub provider: Provider,
    pub presentation: u64,
    pub chains: BTreeMap<u32, Vec<u32>>,
}

#[derive(Default)]
pub(super) struct Factory {
    pub window: Option<u32>,
    pub window_flags: u32,
}

#[derive(Default)]
pub(super) struct Output {
    pub owner: Option<Owner>,
    pub gamma: Option<Box<DXGI_GAMMA_CONTROL>>,
    pub gamma_version: u64,
    pub epoch: u64,
    pub duplicating: bool,
}

pub(super) struct Owner {
    pub claimant: u32,
    pub device: u32,
    pub retained: bool,
}
