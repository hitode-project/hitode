//! Direct3D identities and method entry points over DirectDraw surfaces.
#![deny(clippy::todo, clippy::unimplemented, clippy::wildcard_enum_match_arm)]

pub mod caps;
#[cfg(feature = "gpu")]
pub(super) mod object;
#[cfg(feature = "gpu")]
mod ops;

#[cfg(feature = "gpu")]
pub use ops::{install, register};

#[cfg(feature = "gpu")]
pub(super) use legacy::*;

#[cfg(feature = "gpu")]
mod legacy {
    use super::{caps, object::Object};
    use crate::argument::CallError;
    use crate::ddraw::{ComState, DirectDraw, Kind, creation};
    use adv_abi::{
        com::{Com, ComIdentity, Iid, QueryError},
        gfx_iids::*,
    };
    use adv_device::surface::{Presenter, SurfaceMemory, Surfaces};
    use adv_machine::Objects;
    use std::collections::{BTreeMap, BTreeSet};

    pub(in crate::ddraw) const INVALID: u32 = 0x80070057;
    pub(in crate::ddraw) const NO_INTERFACE: u32 = 0x80004002;
    pub(in crate::ddraw) const OUT_OF_MEMORY: u32 = 0x8007000e;

    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub(in crate::ddraw) enum Face {
        Direct3D,
        Direct3D2,
        Direct3D3,
        Direct3D7,
        Device1,
        Device2,
        Device3,
        Device7,
        Material1,
        Material2,
        Material3,
        Viewport3,
        Light,
        Texture1,
        Texture2,
        VertexBuffer7,
        VertexBuffer,
        ExecuteBuffer,
    }

    impl Face {
        pub const ALL: [Self; 18] = [
            Self::Direct3D,
            Self::Direct3D2,
            Self::Direct3D3,
            Self::Direct3D7,
            Self::Device1,
            Self::Device2,
            Self::Device3,
            Self::Device7,
            Self::Material1,
            Self::Material2,
            Self::Material3,
            Self::Viewport3,
            Self::Light,
            Self::Texture1,
            Self::Texture2,
            Self::VertexBuffer7,
            Self::VertexBuffer,
            Self::ExecuteBuffer,
        ];
        pub const fn name(self) -> &'static str {
            match self {
                Self::Direct3D => "IDirect3D",
                Self::Direct3D2 => "IDirect3D2",
                Self::Direct3D3 => "IDirect3D3",
                Self::Direct3D7 => "IDirect3D7",
                Self::Device1 => "IDirect3DDevice",
                Self::Device2 => "IDirect3DDevice2",
                Self::Device3 => "IDirect3DDevice3",
                Self::Device7 => "IDirect3DDevice7",
                Self::Material1 => "IDirect3DMaterial",
                Self::Material2 => "IDirect3DMaterial2",
                Self::Material3 => "IDirect3DMaterial3",
                Self::Viewport3 => "IDirect3DViewport3",
                Self::Light => "IDirect3DLight",
                Self::Texture1 => "IDirect3DTexture",
                Self::Texture2 => "IDirect3DTexture2",
                Self::VertexBuffer7 => "IDirect3DVertexBuffer7",
                Self::VertexBuffer => "IDirect3DVertexBuffer",
                Self::ExecuteBuffer => "IDirect3DExecuteBuffer",
            }
        }
        pub const fn iid(self) -> Iid {
            match self {
                Self::Direct3D => IID_IDIRECT3D,
                Self::Direct3D2 => IID_IDIRECT3D2,
                Self::Direct3D3 => IID_IDIRECT3D3,
                Self::Direct3D7 => IID_IDIRECT3D7,
                Self::Device1 => IID_IDIRECT3DDEVICE,
                Self::Device2 => IID_IDIRECT3DDEVICE2,
                Self::Device3 => IID_IDIRECT3DDEVICE3,
                Self::Device7 => IID_IDIRECT3DDEVICE7,
                Self::Material1 => IID_IDIRECT3DMATERIAL,
                Self::Material2 => IID_IDIRECT3DMATERIAL2,
                Self::Material3 => IID_IDIRECT3DMATERIAL3,
                Self::Viewport3 => IID_IDIRECT3DVIEWPORT3,
                Self::Light => IID_IDIRECT3DLIGHT,
                Self::Texture1 => IID_IDIRECT3DTEXTURE,
                Self::Texture2 => IID_IDIRECT3DTEXTURE2,
                Self::VertexBuffer7 => IID_IDIRECT3DVERTEXBUFFER7,
                Self::VertexBuffer => IID_IDIRECT3DVERTEXBUFFER,
                Self::ExecuteBuffer => IID_IDIRECT3DEXECUTEBUFFER,
            }
        }
    }

    pub(in crate::ddraw) struct State {
        pub tables: BTreeMap<Face, u32>,
        pub devices: Vec<u32>,
        pub gpu: adv_device::gpu::Gpu,
        pub(super) programs: std::collections::HashMap<u128, adv_device::gpu::Program>,
        pub(super) queue: super::recording::Queue,
        pub(super) depths: BTreeMap<u32, adv_device::gpu::Depth>,
        /// Surfaces recorded commands wrote since the last presentation.
        pub(super) written: BTreeSet<u32>,
        /// Surface versions and storage stamps at the last presentation, for
        /// every surface whose damage history the recording keeps or that a
        /// presented chain holds. CPU writes and exchanges change them.
        pub(super) versions: BTreeMap<u32, (u64, (u64, u64))>,
        pub counters: crate::record::Counters,
    }

    impl State {
        pub fn forget_surface(&mut self, id: u32) {
            self.depths.remove(&id);
            self.written.remove(&id);
            self.versions.remove(&id);
            self.queue.forget((u64::from(id), 0));
        }

        pub fn flush<P: Presenter>(
            &mut self,
            store: &mut Surfaces<P>,
            memory: &dyn SurfaceMemory,
        ) -> Result<(), CallError> {
            super::recording::flush(
                &mut self.queue,
                &mut self.gpu,
                store,
                memory,
                super::recording::Boundary::SurfaceAccess,
            )
        }

        /// Whether a recorded command still to run writes `surface`.
        pub fn pending(&self, surface: u32) -> bool {
            self.written.contains(&surface)
                && self.queue.observes(super::recording::key(surface, 0), true)
        }

        /// Run the recording for presenting `target` (GFX-D3D7-8). `holders`
        /// must hold its version for an unchanged frame to skip execution.
        /// Under `rotation` the frame must overwrite all of `target` and
        /// leave the other `holders` unwritten. Returns the cells changed
        /// since the previous presentation, or `None` when nothing ran.
        pub fn present<P: Presenter>(
            &mut self,
            store: &mut Surfaces<P>,
            memory: &dyn SurfaceMemory,
            (target, identity, serial): (u32, u128, u64),
            holders: &[u32],
            rotation: bool,
        ) -> Result<Option<adv_device::surface::Region>, CallError> {
            use crate::record::key::hash;
            let content = super::recording::key(target, 0);
            let external = self.versions.iter().any(|(&id, &version)| {
                store.get(id).map(|s| s.version).zip(store.cpu_stamp(id)) != Some(version)
            });
            let unsupported = rotation
                && (!self.queue.overwritten(content)
                    || holders
                        .iter()
                        .any(|id| *id != target && self.written.contains(id)));
            if external || unsupported {
                self.queue.invalidate_damage();
                self.versions.clear();
            } else if self.queue.repeated(identity, Some(serial))
                && holders.iter().all(|&id| {
                    store
                        .native_target(id)
                        .is_some_and(|t| self.queue.holds(content, hash(t.image.id())))
                })
            {
                self.queue.end(None);
                self.counters.eliminated_frames += 1;
                return Ok(None);
            }
            let image = store
                .native_target(target)
                .ok_or("presented Direct3D target has no image")?
                .image;
            let region = super::recording::present(
                &mut self.queue,
                &mut self.gpu,
                store,
                memory,
                &mut self.counters,
                (hash(image.id()), (image.width(), image.height())),
            )?;
            Ok(Some(region))
        }

        /// End the frame `present` began; `tracked` surfaces join the written
        /// ones whose versions the next presentation checks.
        pub fn presented<P: Presenter>(
            &mut self,
            store: &Surfaces<P>,
            (identity, serial): (u128, u64),
            tracked: &[u32],
        ) {
            self.queue.finish_metrics(&mut self.counters);
            self.queue.finish(identity, Some(serial));
            for &id in self.written.iter().chain(tracked) {
                let version = store.get(id).map(|s| s.version);
                if let Some(version) = version.zip(store.cpu_stamp(id)) {
                    self.versions.insert(id, version);
                }
            }
            self.written.clear();
        }
    }

    pub(in crate::ddraw) fn allocate_face(
        surfaces: &mut Surfaces<impl Presenter>,
        memory: &mut dyn SurfaceMemory,
        table: u32,
        allocated: &mut Vec<(Iid, u32)>,
    ) -> Result<u32, CallError> {
        let pointer = surfaces.alloc(16, memory)?;
        allocated.push(([0; 16], pointer));
        memory.put_u32(pointer, table)?;
        memory.put_u32(pointer + 4, 0)?;
        Ok(pointer)
    }

    pub(in crate::ddraw) fn attach_faces(
        state: &State,
        com: Com,
        objects: &mut Objects,
        identity: ComIdentity<ComState>,
        surfaces: &mut Surfaces<impl Presenter>,
        memory: &mut dyn SurfaceMemory,
        surface_caps: u32,
    ) -> Result<bool, CallError> {
        let object = com.get_identity(objects, identity).unwrap();
        let kind = object.kind;
        let modern = com.canonical(objects, identity).unwrap() != object.old;
        let mut allocated = Vec::new();
        let attached = (|| {
            let mut attach = |face: Face, aliases: &[Iid]| {
                let pointer = allocate_face(surfaces, memory, state.tables[&face], &mut allocated)?;
                let attached = aliases
                    .iter()
                    .all(|&iid| com.register_interface(objects, identity, iid, pointer));
                Ok::<_, CallError>((pointer, attached))
            };
            match kind {
                Kind::Device => {
                    let faces: &[Face] = if modern {
                        &[Face::Direct3D7]
                    } else {
                        &[Face::Direct3D, Face::Direct3D2, Face::Direct3D3]
                    };
                    for &face in faces {
                        if !attach(face, &[face.iid()])?.1 {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                Kind::Surface(_) => {
                    if surface_caps & creation::TEXTURE != 0
                        && (!attach(Face::Texture1, &[IID_IDIRECT3DTEXTURE])?.1
                            || !attach(Face::Texture2, &[IID_IDIRECT3DTEXTURE2])?.1)
                    {
                        return Ok(false);
                    }
                    if surface_caps & creation::THREE_D_DEVICE != 0 {
                        let (hal, a) = attach(
                            Face::Device1,
                            &[IID_IDIRECT3DHALDEVICE, IID_IDIRECT3DDEVICE],
                        )?;
                        let (rgb, b) = attach(Face::Device1, &[IID_IDIRECT3DRGBDEVICE])?;
                        let object = com.get_identity_mut(objects, identity).unwrap();
                        object.d3d_device_faces = [hal, rgb];
                        return Ok(a && b);
                    }
                    Ok(true)
                }
                Kind::Clipper => Ok(true),
            }
        })();
        if !matches!(attached, Ok(true)) {
            for (_, pointer) in allocated {
                surfaces.release(pointer, 16);
            }
        }
        attached
    }

    pub(in crate::ddraw) fn query(
        draw: &mut DirectDraw,
        objects: &mut Objects,
        this: u32,
        iid: Iid,
    ) -> Result<u32, u32> {
        let com = draw.com.ok_or(INVALID)?;
        let map = |error| match error {
            QueryError::NoInterface => NO_INTERFACE,
            QueryError::ReferenceLimit => OUT_OF_MEMORY,
            QueryError::InvalidObject => INVALID,
        };
        if let Some(object) = com.get::<ComState>(objects, this) {
            let faces = object.d3d_device_faces;
            if faces.contains(&this) && [IID_IDIRECT3DDEVICE2, IID_IDIRECT3DDEVICE3].contains(&iid)
            {
                return Err(INVALID);
            }
            if faces.contains(&this) && iid == IID_IDIRECT3DDEVICE {
                com.add_ref::<ComState>(objects, this)
                    .ok_or(OUT_OF_MEMORY)?;
                return Ok(this);
            }
            let kind = object.kind;
            let pointer = com.query::<ComState>(objects, this, iid).map_err(map)?;
            if let Kind::Surface(sid) = kind
                && faces.contains(&pointer)
                && !draw.d3d_surface_devices.contains_key(&pointer)
            {
                let kind = if pointer == faces[0] {
                    caps::Device::Hal
                } else {
                    caps::Device::Rgb
                };
                let parent = draw
                    .surface_owners
                    .get(&sid)
                    .and_then(|owner| draw.device_objects.get(owner))
                    .and_then(|&identity| com.canonical(objects, identity))
                    .unwrap_or(0);
                let target = com.get::<ComState>(objects, this).unwrap().old;
                let mut device = super::object::Device::new(kind, Face::Device1, parent, target);
                let desc = draw.described[&sid];
                let depth = draw
                    .attached(sid)
                    .into_iter()
                    .any(|id| draw.caps(id)[0] & creation::ZBUFFER != 0);
                device.state = Some(super::state::State::new(
                    desc.width,
                    desc.height,
                    depth,
                    None,
                ));
                draw.d3d_surface_devices.insert(pointer, device);
            }
            Ok(pointer)
        } else {
            com.query::<Object>(objects, this, iid).map_err(map)
        }
    }

    pub(in crate::ddraw) fn add_ref(draw: &DirectDraw, objects: &mut Objects, this: u32) -> u32 {
        let Some(com) = draw.com else { return 0 };
        com.add_ref::<ComState>(objects, this)
            .or_else(|| com.add_ref::<Object>(objects, this))
            .unwrap_or(0)
    }

    pub(in crate::ddraw) fn device<'a>(
        draw: &'a DirectDraw,
        objects: &'a Objects,
        this: u32,
    ) -> Option<&'a super::object::Device> {
        if let Some(Object::Device(device)) = draw.com?.get::<Object>(objects, this) {
            Some(device)
        } else {
            draw.d3d_surface_devices.get(&this)
        }
    }

    pub(in crate::ddraw) fn device_mut<'a>(
        draw: &'a mut DirectDraw,
        objects: &'a mut Objects,
        this: u32,
    ) -> Option<&'a mut super::object::Device> {
        if let Some(Object::Device(device)) = draw.com?.get_mut::<Object>(objects, this) {
            Some(device)
        } else {
            draw.d3d_surface_devices.get_mut(&this)
        }
    }

    pub(in crate::ddraw) fn release(
        draw: &mut DirectDraw,
        surfaces: &mut Surfaces<impl Presenter>,
        objects: &mut Objects,
        this: u32,
    ) -> u32 {
        let Some(com) = draw.com else { return 0 };
        let Some(released) = com.release::<Object>(objects, this) else {
            return draw.release_com(objects, surfaces, this);
        };
        if let Some(destroyed) = released.destroyed {
            match destroyed.value {
                Object::Device(mut device) => {
                    draw.d3d
                        .as_mut()
                        .unwrap()
                        .devices
                        .retain(|p| !destroyed.interfaces.contains(p));
                    for viewport in std::mem::take(&mut device.viewports) {
                        if let Some(Object::Viewport { device, .. }) =
                            com.get_mut::<Object>(objects, viewport)
                        {
                            *device = None;
                        }
                        release(draw, surfaces, objects, viewport);
                    }
                    if let Some(state) = &device.state {
                        for pointer in state.texture_references() {
                            draw.release_com(objects, surfaces, pointer);
                        }
                    }
                    draw.release_com(objects, surfaces, device.target);
                    draw.release_com(objects, surfaces, device.parent);
                    device.dispose(surfaces);
                }
                Object::Viewport { lights, depth, .. } => {
                    if depth != 0 {
                        draw.release_com(objects, surfaces, depth);
                    }
                    for light in lights {
                        if let Some(Object::Light { viewport, .. }) =
                            com.get_mut::<Object>(objects, light)
                        {
                            *viewport = None;
                        }
                        release(draw, surfaces, objects, light);
                    }
                }
                Object::Material(_) => {
                    forget(draw, objects, &destroyed.interfaces, true);
                }
                Object::Light { .. } => (),
                Object::VertexBuffer { data, size, .. } => surfaces.release(data, size),
                Object::ExecuteBuffer(buffer) => {
                    if buffer.owned {
                        surfaces.release(buffer.data, buffer.size);
                    }
                    release(draw, surfaces, objects, buffer.device);
                }
            }
            for pointer in destroyed.interfaces {
                surfaces.release(pointer, 16);
            }
        }
        released.references
    }

    pub(in crate::ddraw) fn release_surface(
        draw: &mut DirectDraw,
        objects: &mut Objects,
        surfaces: &mut Surfaces<impl Presenter>,
        faces: [u32; 2],
        interfaces: &[u32],
    ) {
        for face in faces {
            if let Some(mut device) = draw.d3d_surface_devices.remove(&face) {
                for viewport in std::mem::take(&mut device.viewports) {
                    if let Some(Object::Viewport { device, .. }) =
                        draw.com.unwrap().get_mut::<Object>(objects, viewport)
                    {
                        *device = None;
                    }
                    release(draw, surfaces, objects, viewport);
                }
                if let Some(state) = &device.state {
                    for pointer in state.texture_references() {
                        draw.release_com(objects, surfaces, pointer);
                    }
                }
                device.dispose(surfaces);
            }
        }
        forget(draw, objects, interfaces, false);
    }

    fn forget(draw: &mut DirectDraw, objects: &mut Objects, interfaces: &[u32], material: bool) {
        let com = draw.com.unwrap();
        let devices = draw
            .d3d
            .as_ref()
            .unwrap()
            .devices
            .iter()
            .filter_map(|&pointer| com.identity::<Object>(objects, pointer))
            .collect::<Vec<_>>();
        let forget = |device: &mut super::object::Device| {
            for &pointer in interfaces {
                if material {
                    device.forget_material(pointer);
                } else {
                    device.forget_texture(pointer);
                }
            }
        };
        for identity in devices {
            if let Some(Object::Device(device)) = com.get_identity_mut(objects, identity) {
                forget(device);
            }
        }
        for device in draw.d3d_surface_devices.values_mut() {
            forget(device);
        }
    }
}

#[cfg(all(test, feature = "gpu"))]
mod tests;

#[cfg(feature = "gpu")]
mod desc;
#[cfg(feature = "gpu")]
mod state;

#[cfg(feature = "gpu")]
mod recording;

#[cfg(feature = "gpu")]
mod report;
