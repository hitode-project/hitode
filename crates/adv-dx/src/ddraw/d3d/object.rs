//! Direct3D identities owned by the DirectDraw COM namespace.
use super::{Face, INVALID, OUT_OF_MEMORY};
use crate::argument::CallError;
use adv_abi::com::{Com, Iid};
use adv_device::surface::{Presenter, SurfaceMemory, Surfaces};
use adv_machine::Objects;
use std::collections::BTreeMap;

pub(super) enum Object {
    Device(Box<Device>),
    VertexBuffer {
        desc: adv_abi::types::D3DVERTEXBUFFERDESC,
        data: u32,
        size: u32,
        locked: bool,
    },
    ExecuteBuffer(ExecuteBuffer),
    Material([u32; 20]),
    Viewport {
        device: Option<u32>,
        lights: Vec<u32>,
        value: [u32; 11],
        value2: bool,
        background: u32,
        depth: u32,
    },
    Light {
        viewport: Option<u32>,
        value: [u32; 20],
    },
}

pub(super) struct ExecuteBuffer {
    pub device: u32,
    pub data: u32,
    pub size: u32,
    pub owned: bool,
    pub locked: bool,
    pub execute: adv_abi::types::D3DEXECUTEDATA,
}

pub(in crate::ddraw) struct Device {
    pub kind: super::caps::Device,
    pub(super) face: Face,
    pub parent: u32,
    pub target: u32,
    pub viewports: Vec<u32>,
    pub(super) current_viewport: u32,
    pub(super) legacy: LegacyState,
    pub(super) state: Option<super::state::State>,
    material_handles: BTreeMap<u32, u32>,
    texture_handles: BTreeMap<u32, u32>,
    next_texture: u32,
    retired_material_handles: Vec<u32>,
}

#[derive(Default)]
pub(super) struct LegacyState {
    pub matrices: BTreeMap<u32, [f32; 16]>,
    pub next_matrix: u32,
    pub render: BTreeMap<u32, u32>,
    pub light: BTreeMap<u32, u32>,
    pub batch: Option<PrimitiveBatch>,
    pub stats: [u32; 5],
    pub picked: Vec<adv_abi::types::D3DPICKRECORD>,
}

pub(super) struct PrimitiveBatch {
    pub primitive: u32,
    pub fvf: u32,
    pub flags: u32,
    pub vertices: Vec<u8>,
    pub indices: Option<Vec<u32>>,
}

impl Device {
    pub(super) fn texture_for_handle(&self, handle: u32) -> Option<u32> {
        self.texture_handles
            .iter()
            .find_map(|(&texture, &id)| (id == handle).then_some(texture))
    }

    pub(super) fn material_for_handle(&self, handle: u32) -> Option<u32> {
        self.material_handles
            .iter()
            .find_map(|(&material, &id)| (id == handle).then_some(material))
    }

    pub(super) fn swap_texture_handles(&mut self, a: u32, b: u32) {
        let first = self.texture_handles.remove(&a);
        let second = self.texture_handles.remove(&b);
        if let Some(first) = first {
            self.texture_handles.insert(b, first);
        }
        if let Some(second) = second {
            self.texture_handles.insert(a, second);
        }
    }

    pub fn new(kind: super::caps::Device, face: Face, parent: u32, target: u32) -> Self {
        Self {
            kind,
            face,
            parent,
            target,
            viewports: Vec::new(),
            current_viewport: 0,
            legacy: LegacyState::default(),
            state: None,
            material_handles: BTreeMap::new(),
            texture_handles: BTreeMap::new(),
            next_texture: 0x14,
            retired_material_handles: Vec::new(),
        }
    }

    pub fn material_handle<P: Presenter>(
        &mut self,
        material: u32,
        surfaces: &mut Surfaces<P>,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<u32, CallError> {
        if let Some(&handle) = self.material_handles.get(&material) {
            return Ok(handle);
        }
        let handle = surfaces.alloc(4, memory)?;
        self.material_handles.insert(material, handle);
        Ok(handle)
    }

    pub fn texture_handle(&mut self, texture: u32) -> u32 {
        *self.texture_handles.entry(texture).or_insert_with(|| {
            let handle = self.next_texture;
            self.next_texture += 1;
            handle
        })
    }

    pub fn forget_material(&mut self, material: u32) {
        if let Some(handle) = self.material_handles.remove(&material) {
            self.retired_material_handles.push(handle);
        }
    }

    pub fn forget_texture(&mut self, texture: u32) {
        self.texture_handles.remove(&texture);
    }

    pub fn dispose(self, surfaces: &mut Surfaces<impl Presenter>) {
        for handle in self
            .material_handles
            .into_values()
            .chain(self.retired_material_handles)
        {
            surfaces.release(handle, 4);
        }
    }
}

pub(super) fn create<P: Presenter>(
    com: Com,
    objects: &mut Objects,
    surfaces: &mut Surfaces<P>,
    memory: &mut dyn SurfaceMemory,
    faces: &[(Face, u32)],
    aliases: &[Iid],
    value: Object,
) -> Result<u32, CallError> {
    let mut pointers = Vec::with_capacity(faces.len());
    for &(face, table) in faces {
        if let Err(error) = super::allocate_face(surfaces, memory, table, &mut pointers) {
            for (_, pointer) in pointers {
                surfaces.release(pointer, 16);
            }
            return Err(error);
        }
        pointers.last_mut().unwrap().0 = face.iid();
    }
    let (iid, pointer) = pointers[0];
    let Some(identity) = com.create(objects, pointer, iid, value) else {
        for (_, pointer) in pointers {
            surfaces.release(pointer, 16);
        }
        return Ok(OUT_OF_MEMORY);
    };
    let registered = pointers[1..]
        .iter()
        .chain(
            aliases
                .iter()
                .map(|iid| (*iid, pointer))
                .collect::<Vec<_>>()
                .iter(),
        )
        .all(|&(iid, pointer)| com.register_interface(objects, identity, iid, pointer));
    if !registered {
        com.release_identity(objects, identity);
        for (_, pointer) in pointers {
            surfaces.release(pointer, 16);
        }
        return Ok(OUT_OF_MEMORY);
    }
    Ok(pointer)
}

pub(super) fn attachment<P: Presenter>(
    draw: &mut crate::ddraw::DirectDraw,
    objects: &mut Objects,
    surfaces: &mut Surfaces<P>,
    parent: u32,
    child: u32,
    add: bool,
    viewport: bool,
) -> u32 {
    let com = draw.com.unwrap();
    let Some(identity) = com.identity::<Object>(objects, child) else {
        return INVALID;
    };
    let child = com.canonical(objects, identity).unwrap();
    let parent = if viewport {
        if let Some(identity) = com.identity::<Object>(objects, parent) {
            let canonical = com.canonical(objects, identity).unwrap();
            if !matches!(
                com.get::<Object>(objects, canonical),
                Some(Object::Device(_))
            ) {
                return INVALID;
            }
            canonical
        } else if !draw.d3d_surface_devices.contains_key(&parent) {
            return INVALID;
        } else {
            parent
        }
    } else {
        if !matches!(
            com.get::<Object>(objects, parent),
            Some(Object::Viewport { .. })
        ) {
            return INVALID;
        }
        parent
    };
    let children = if viewport {
        let Some(Object::Viewport { device, .. }) = com.get_mut::<Object>(objects, child) else {
            return INVALID;
        };
        if add && device.is_some() || !add && *device != Some(parent) {
            return INVALID;
        }
        *device = add.then_some(parent);
        let device = super::device_mut(draw, objects, parent).unwrap();
        if !add && device.current_viewport == child {
            device.current_viewport = 0;
        }
        &mut device.viewports
    } else {
        let Some(Object::Light { viewport, .. }) = com.get_mut::<Object>(objects, child) else {
            return INVALID;
        };
        if add && viewport.is_some() {
            return 0x887602ef;
        }
        if !add && *viewport != Some(parent) {
            return 0x887602ee;
        }
        *viewport = add.then_some(parent);
        let Some(Object::Viewport { lights, .. }) = com.get_mut::<Object>(objects, parent) else {
            return INVALID;
        };
        lights
    };
    if add {
        children.push(child);
        com.add_ref::<Object>(objects, child).unwrap();
    } else {
        children.retain(|&pointer| pointer != child);
        super::release(draw, surfaces, objects, child);
    }
    0
}
