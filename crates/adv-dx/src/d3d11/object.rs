//! Object identities and ownership. Bindings and views own internal references.
pub mod map;

use super::{
    abi::limit,
    caps::Report,
    desc::{PipelineState, Resource, TextureKind, View, ViewKind},
};
use adv_abi::com::{Com, Destroyed, Iid, QueryError};
use adv_abi::gfx_iids::*;
use adv_machine::Objects;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidObject,
    NoInterface,
    Exhausted,
    TooManyUniqueStates,
}

impl From<QueryError> for Error {
    fn from(value: QueryError) -> Self {
        match value {
            QueryError::InvalidObject => Self::InvalidObject,
            QueryError::NoInterface => Self::NoInterface,
            QueryError::ReferenceLimit => Self::Exhausted,
        }
    }
}

pub enum PrivateData {
    Bytes(Vec<u8>),
    Interface(u32),
}

pub struct Object {
    device: u32,
    kind: Kind,
    private: BTreeMap<Iid, PrivateData>,
}

enum Kind {
    Device(Device),
    ImmediateContext,
    Resource { desc: Resource, priority: u32 },
    View { desc: Arc<View>, resource: u32 },
    Pipeline(Box<PipelineState>),
}

struct Device {
    report: Arc<Report>,
    states: [BTreeMap<Vec<u8>, u32>; 4],
}

impl Kind {
    fn interfaces(&self) -> (Iid, &'static [Iid]) {
        const DEVICE_CHILD: &[Iid] = &[IID_ID3D11DEVICECHILD];
        const RESOURCE: &[Iid] = &[IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE];
        const VIEW: &[Iid] = &[IID_ID3D11DEVICECHILD, IID_ID3D11VIEW];
        match self {
            Self::Device(_) => (IID_ID3D11DEVICE, &[]),
            Self::ImmediateContext => (IID_ID3D11DEVICECONTEXT, DEVICE_CHILD),
            Self::Resource { desc, .. } => {
                let iid = match desc {
                    Resource::Buffer(_) => IID_ID3D11BUFFER,
                    Resource::Texture(texture) => match texture.kind() {
                        TextureKind::D1 => IID_ID3D11TEXTURE1D,
                        TextureKind::D2 => IID_ID3D11TEXTURE2D,
                        TextureKind::D3 => IID_ID3D11TEXTURE3D,
                    },
                };
                (iid, RESOURCE)
            }
            Self::View { desc, .. } => {
                let iid = match desc.kind() {
                    ViewKind::ShaderResource => IID_ID3D11SHADERRESOURCEVIEW,
                    ViewKind::RenderTarget => IID_ID3D11RENDERTARGETVIEW,
                    ViewKind::DepthStencil => IID_ID3D11DEPTHSTENCILVIEW,
                    ViewKind::UnorderedAccess => IID_ID3D11UNORDEREDACCESSVIEW,
                };
                (iid, VIEW)
            }
            Self::Pipeline(state) => match state.as_ref() {
                PipelineState::Blend(_) => (
                    IID_ID3D11BLENDSTATE1,
                    &[IID_ID3D11DEVICECHILD, IID_ID3D11BLENDSTATE],
                ),
                PipelineState::DepthStencil(_) => (IID_ID3D11DEPTHSTENCILSTATE, DEVICE_CHILD),
                PipelineState::Rasterizer(_) => (
                    IID_ID3D11RASTERIZERSTATE1,
                    &[IID_ID3D11DEVICECHILD, IID_ID3D11RASTERIZERSTATE],
                ),
                PipelineState::Sampler(_) => (IID_ID3D11SAMPLERSTATE, DEVICE_CHILD),
            },
        }
    }
}

impl Object {
    pub fn resource(&self) -> Option<Resource> {
        match &self.kind {
            Kind::Resource { desc, .. } => Some(*desc),
            _ => None,
        }
    }

    pub fn view(&self) -> Option<(u32, &Arc<View>)> {
        match &self.kind {
            Kind::View { resource, desc } => Some((*resource, desc)),
            _ => None,
        }
    }

    pub fn pipeline(&self) -> Option<&PipelineState> {
        match &self.kind {
            Kind::Pipeline(pipeline) => Some(pipeline),
            _ => None,
        }
    }

    pub fn priority(&self) -> Option<u32> {
        match self.kind {
            Kind::Resource { priority, .. } => Some(priority),
            _ => None,
        }
    }

    pub fn set_priority(&mut self, value: u32) {
        if let Kind::Resource { priority, .. } = &mut self.kind {
            *priority = value;
        }
    }

    pub fn report(&self) -> Option<&Report> {
        match &self.kind {
            Kind::Device(device) => Some(&device.report),
            _ => None,
        }
    }

    pub fn device(&self) -> Option<u32> {
        (self.device != 0).then_some(self.device)
    }

    pub fn private(&self, key: Iid) -> Option<&PrivateData> {
        self.private.get(&key)
    }

    /// The caller acquires a new private interface reference before storing it
    /// and releases the replaced reference after this operation.
    pub fn set_private(&mut self, key: Iid, value: Option<PrivateData>) -> Option<PrivateData> {
        match value {
            Some(value) => self.private.insert(key, value),
            None => self.private.remove(&key),
        }
    }
}

#[derive(Default)]
pub struct Release {
    pub references: u32,
    pub headers: Vec<u32>,
    pub private_interfaces: Vec<u32>,
}

#[derive(Clone, Copy)]
pub struct Table {
    com: Com,
}

impl Table {
    pub fn new(objects: &mut Objects) -> Result<Self, Error> {
        Ok(Self {
            com: Com::new(objects).ok_or(Error::Exhausted)?,
        })
    }

    pub fn get<'a>(self, objects: &'a Objects, pointer: u32) -> Option<&'a Object> {
        self.com.get(objects, pointer)
    }

    pub fn get_mut<'a>(self, objects: &'a mut Objects, pointer: u32) -> Option<&'a mut Object> {
        self.com.get_mut(objects, pointer)
    }

    pub fn attach_device(
        self,
        objects: &mut Objects,
        pointer: u32,
        report: Arc<Report>,
    ) -> Result<(), Error> {
        self.attach(
            objects,
            pointer,
            0,
            Kind::Device(Device {
                report,
                states: Default::default(),
            }),
        )
    }

    pub fn attach_context(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
    ) -> Result<(), Error> {
        self.attach(objects, pointer, device, Kind::ImmediateContext)
    }

    pub fn attach_resource(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        desc: Resource,
    ) -> Result<(), Error> {
        self.attach(
            objects,
            pointer,
            device,
            Kind::Resource {
                desc,
                priority: 0x78000000,
            },
        )
    }

    pub fn attach_view(
        self,
        objects: &mut Objects,
        pointer: u32,
        resource: u32,
        desc: View,
    ) -> Result<(), Error> {
        let object = self.get(objects, resource).ok_or(Error::InvalidObject)?;
        if object.resource().is_none() {
            return Err(Error::InvalidObject);
        }
        let device = object.device;
        let identity = self.com.identity::<Object>(objects, resource).unwrap();
        if !self.com.retain_internal(objects, identity) {
            return Err(Error::Exhausted);
        }
        let attached = self.attach(
            objects,
            pointer,
            device,
            Kind::View {
                desc: Arc::new(desc),
                resource,
            },
        );
        if attached.is_err() {
            self.com.release_internal(objects, identity);
        }
        attached
    }

    /// An existing descriptor acquires a reference without claiming another header.
    pub fn find_pipeline(
        self,
        objects: &mut Objects,
        device: u32,
        desc: &PipelineState,
    ) -> Result<Option<u32>, Error> {
        let (class, limit) = state_class(desc);
        let states = &self.device(objects, device)?.states[class];
        if let Some(&pointer) = states.get(desc.descriptor_bytes()) {
            self.add_ref(objects, pointer)?;
            return Ok(Some(pointer));
        }
        if states.len() >= limit {
            return Err(Error::TooManyUniqueStates);
        }
        Ok(None)
    }

    pub fn attach_pipeline(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        desc: PipelineState,
    ) -> Result<u32, Error> {
        if let Some(existing) = self.find_pipeline(objects, device, &desc)? {
            return Ok(existing);
        }
        let (class, _) = state_class(&desc);
        let key = desc.descriptor_bytes().to_vec();
        self.attach(objects, pointer, device, Kind::Pipeline(Box::new(desc)))?;
        let Kind::Device(owner) = &mut self.get_mut(objects, device).unwrap().kind else {
            unreachable!()
        };
        owner.states[class].insert(key, pointer);
        Ok(pointer)
    }

    fn device(self, objects: &Objects, pointer: u32) -> Result<&Device, Error> {
        match &self.get(objects, pointer).ok_or(Error::InvalidObject)?.kind {
            Kind::Device(device) => Ok(device),
            _ => Err(Error::InvalidObject),
        }
    }

    fn attach(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        kind: Kind,
    ) -> Result<(), Error> {
        if device != 0 {
            self.device(objects, device)?;
            self.com
                .add_ref::<Object>(objects, device)
                .ok_or(Error::Exhausted)?;
        }
        let (iid, bases) = kind.interfaces();
        let object = Object {
            device,
            kind,
            private: BTreeMap::new(),
        };
        let Some(identity) = self.com.create(objects, pointer, iid, object) else {
            if device != 0 {
                self.com.release::<Object>(objects, device);
            }
            return Err(Error::Exhausted);
        };
        for &iid in bases {
            assert!(self.com.register_interface(objects, identity, iid, pointer));
        }
        Ok(())
    }

    pub fn query(self, objects: &mut Objects, pointer: u32, iid: Iid) -> Result<u32, Error> {
        let interface = self
            .com
            .resolve_interface::<Object>(objects, pointer, iid)?;
        self.add_ref(objects, interface)?;
        Ok(interface)
    }

    pub fn references(self, objects: &Objects, pointer: u32) -> Option<u32> {
        let identity = self.com.identity::<Object>(objects, pointer)?;
        self.com.references(objects, identity)
    }

    pub fn add_ref(self, objects: &mut Objects, pointer: u32) -> Result<u32, Error> {
        let object = self.get(objects, pointer).ok_or(Error::InvalidObject)?;
        let device = object.device;
        let first = device != 0 && self.references(objects, pointer) == Some(0);
        if first {
            self.com
                .add_ref::<Object>(objects, device)
                .ok_or(Error::Exhausted)?;
        }
        match self.com.add_ref::<Object>(objects, pointer) {
            Some(references) => Ok(references),
            None => {
                if first {
                    self.com.release::<Object>(objects, device);
                }
                Err(Error::Exhausted)
            }
        }
    }

    pub fn retain_bound(self, objects: &mut Objects, pointer: u32) -> Result<(), Error> {
        let identity = self
            .com
            .identity::<Object>(objects, pointer)
            .ok_or(Error::InvalidObject)?;
        self.com
            .retain_internal(objects, identity)
            .then_some(())
            .ok_or(Error::Exhausted)
    }

    pub fn release(self, objects: &mut Objects, pointer: u32) -> Release {
        let mut result = Release::default();
        result.references = self.release_public(objects, pointer, &mut result);
        result
    }

    pub fn release_bound(self, objects: &mut Objects, pointer: u32) -> Release {
        let mut result = Release::default();
        self.release_internal(objects, pointer, &mut result);
        result
    }

    fn release_public(self, objects: &mut Objects, pointer: u32, result: &mut Release) -> u32 {
        let Some(object) = self.get(objects, pointer) else {
            return 0;
        };
        let device = object.device;
        let Some(released) = self.com.release::<Object>(objects, pointer) else {
            return 0;
        };
        if let Some(destroyed) = released.destroyed {
            self.destroy(objects, destroyed, result);
        }
        if released.references == 0 && device != 0 {
            self.release_public(objects, device, result);
        }
        released.references
    }

    fn release_internal(self, objects: &mut Objects, pointer: u32, result: &mut Release) {
        let identity = self.com.identity::<Object>(objects, pointer).unwrap();
        if let Some(Some(destroyed)) = self.com.release_internal(objects, identity) {
            self.destroy(objects, destroyed, result);
        }
    }

    fn destroy(self, objects: &mut Objects, destroyed: Destroyed<Object>, result: &mut Release) {
        let Object {
            device,
            kind,
            private,
        } = destroyed.value;
        result.headers.extend(destroyed.interfaces);
        result
            .private_interfaces
            .extend(private.into_values().filter_map(|value| match value {
                PrivateData::Interface(pointer) if pointer != 0 => Some(pointer),
                _ => None,
            }));
        match kind {
            Kind::View { resource, .. } => self.release_internal(objects, resource, result),
            Kind::Pipeline(state) => {
                let (class, _) = state_class(&state);
                if let Some(owner) = self.get_mut(objects, device)
                    && let Kind::Device(owner) = &mut owner.kind
                {
                    owner.states[class].remove(state.descriptor_bytes());
                }
            }
            Kind::Device(_) | Kind::ImmediateContext | Kind::Resource { .. } => (),
        }
    }
}

fn state_class(state: &PipelineState) -> (usize, usize) {
    let (class, constant) = match state {
        PipelineState::Blend(_) => (0, "D3D11_REQ_BLEND_OBJECT_COUNT_PER_DEVICE"),
        PipelineState::DepthStencil(_) => (1, "D3D11_REQ_DEPTH_STENCIL_OBJECT_COUNT_PER_DEVICE"),
        PipelineState::Rasterizer(_) => (2, "D3D11_REQ_RASTERIZER_OBJECT_COUNT_PER_DEVICE"),
        PipelineState::Sampler(_) => (3, "D3D11_REQ_SAMPLER_OBJECT_COUNT_PER_DEVICE"),
    };
    (class, limit(constant) as usize - 1)
}

#[cfg(test)]
mod tests;
