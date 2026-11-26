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
    use super::object::Object;
    use crate::argument::CallError;
    use crate::ddraw::{ComState, DirectDraw, Kind, creation};
    use adv_abi::{
        com::{Com, ComIdentity, Iid, QueryError},
        gfx_iids::*,
    };
    use adv_device::surface::{Presenter, SurfaceMemory, Surfaces};
    use adv_machine::Objects;
    use std::collections::BTreeMap;

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
    }

    impl Face {
        pub const ALL: [Self; 15] = [
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
            }
        }
    }

    pub(in crate::ddraw) struct State {
        pub tables: BTreeMap<Face, u32>,
        pub devices: Vec<u32>,
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
                let parent = draw
                    .surface_owners
                    .get(&sid)
                    .and_then(|owner| draw.device_objects.get(owner))
                    .and_then(|&identity| com.canonical(objects, identity))
                    .unwrap_or(0);
                let target = com.get::<ComState>(objects, this).unwrap().old;
                draw.d3d_surface_devices
                    .insert(pointer, super::object::Device::new(parent, target));
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
                    draw.release_com(objects, surfaces, device.target);
                    draw.release_com(objects, surfaces, device.parent);
                    device.dispose(surfaces);
                }
                Object::Viewport { lights, .. } => {
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
