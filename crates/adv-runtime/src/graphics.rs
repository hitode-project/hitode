//! Assembly of the graphics fronts: GDI raster calls (adv-win32) and DirectDraw (adv-dx)
//! over one surface store (adv-device) and one device-context table (adv-os).
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use adv_cpu::{ApiCall, ApiMemory, ApiOutcome, ApiSpec, Convention, HostServices, Machine};
use adv_dx::ddraw::DirectDraw;
use adv_os::gdi::{DeviceContexts, WINDOW_DC};
use adv_win32::gdi32::raster::Raster;

pub use adv_device::surface::{
    FrameConsumer, FrameProducer, FrameView, GraphicsStats, PendingFrame, Presenter, Snapshot,
    SurfaceMemory, Surfaces, frame_mailbox,
};
pub use adv_dx::ddraw::{DEVICE_METHODS, IID_DDRAW7, IID_SURFACE7, SURFACE_METHODS};

/// Call ids of both fronts: raster calls below 200, DirectDraw from 200.
pub mod api {
    pub use adv_dx::ddraw::api::*;
    pub use adv_win32::gdi32::raster::api::*;
}

/// (id, module, name, argc) of every graphics call.
pub fn api_specs() -> impl Iterator<Item = &'static (u32, &'static str, &'static str, u8)> {
    adv_dx::ddraw::API_SPECS
        .iter()
        .chain(adv_win32::gdi32::raster::API_SPECS)
}

pub(crate) struct GuestMemory<'a>(pub &'a mut dyn adv_cpu::backend::Guest);
impl SurfaceMemory for GuestMemory<'_> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        if let Some(bytes) = self.0.shared_bytes(addr, len) {
            return Ok(Cow::Borrowed(bytes));
        }
        let mut bytes = vec![0; len];
        self.0.read(addr, &mut bytes).map_err(|e| e.to_string())?;
        Ok(Cow::Owned(bytes))
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.0.write(addr, data).map_err(|e| e.to_string())
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        if self
            .0
            .accessible(addr, len as u32, adv_cpu::backend::Prot::WRITE)
        {
            Ok(())
        } else {
            Err(format!(
                "surface memory is not writable at {addr:#x} (+{len})"
            ))
        }
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.0.copy_memory(src, dst, len).map_err(|e| e.to_string())
    }
}

/// Guest memory of the standalone `adv_cpu::Machine`.
struct ApiGuest<'a, 'b>(&'a mut ApiMemory<'b>);
impl SurfaceMemory for ApiGuest<'_, '_> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        self.0.read(addr, len)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.0.write(addr, data)
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        self.0.writable(addr, len)
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.0.copy(src, dst, len)
    }
}

/// Returns id -> guest entry. COM vtables use these exact same API thunks.
pub fn install(machine: &mut Machine, base: u32) -> Result<BTreeMap<u32, u32>, String> {
    let mut entries = BTreeMap::new();
    for (index, &(id, module, name, argc)) in api_specs().enumerate() {
        let entry = base
            .checked_add(index as u32 * 16)
            .ok_or("API address overflow")?;
        machine.register_api(ApiSpec::new(
            id,
            module,
            name,
            entry,
            Convention::Stdcall(argc),
        ))?;
        entries.insert(id, entry);
    }
    Ok(entries)
}

/// The surface store with both fronts. Dereferences to the store for presentation.
pub struct Graphics<P: Presenter> {
    surfaces: Surfaces<P>,
    dcs: DeviceContexts,
    raster: Raster,
    ddraw: DirectDraw,
}

impl<P: Presenter> Deref for Graphics<P> {
    type Target = Surfaces<P>;
    fn deref(&self) -> &Surfaces<P> {
        &self.surfaces
    }
}

impl<P: Presenter> DerefMut for Graphics<P> {
    fn deref_mut(&mut self) -> &mut Surfaces<P> {
        &mut self.surfaces
    }
}

impl<P: Presenter> Graphics<P> {
    pub fn new(
        width: u32,
        height: u32,
        arena: u32,
        len: u32,
        entries: BTreeMap<u32, u32>,
        presenter: P,
    ) -> Result<Self, String> {
        Ok(Self {
            surfaces: Surfaces::new(width, height, arena, len, presenter)?,
            dcs: DeviceContexts::default(),
            raster: Raster::default(),
            ddraw: DirectDraw::new(entries),
        })
    }

    pub(crate) fn handle(
        &mut self,
        id: u32,
        args: &[u32],
        memory: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        if id < 200 {
            self.raster
                .call(id, args, &mut self.surfaces, &mut self.dcs, memory)
        } else {
            self.ddraw
                .call(id, args, &mut self.surfaces, &mut self.dcs, memory)
        }
    }

    pub(crate) fn owns_dc(&self, dc: u32) -> bool {
        dc == WINDOW_DC || self.dcs.contains(dc)
    }

    pub(crate) fn dc_bitmap(&self, dc: u32) -> Option<u32> {
        if dc == WINDOW_DC {
            self.surfaces.primary_id()
        } else {
            self.dcs.get(dc)
        }
    }

    pub(crate) fn owns_bitmap(&self, object: u32) -> bool {
        self.surfaces.contains(object)
    }

    pub(crate) fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.ddraw.configure_clipper(clipper, origin);
    }
}

impl<P: Presenter> HostServices for Graphics<P> {
    fn call(&mut self, c: ApiCall<'_>, m: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
        let v = self.handle(c.spec.id, c.args, &mut ApiGuest(m))?;
        Ok(ApiOutcome::Return(v))
    }
}
