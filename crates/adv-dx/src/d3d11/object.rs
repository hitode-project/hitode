//! Object identities and ownership. Bindings and views own internal references.
pub mod map;
pub(super) mod query;

use super::{
    abi::limit,
    caps::Report,
    desc::{PipelineState, Resource, Shader, TextureKind, View, ViewKind},
    state::ContextState,
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

#[cfg(feature = "gpu")]
pub(super) struct Shared {
    pub handle: u32,
    pub identity: (u64, u64),
    pub resource: Resource,
    pub adapter: crate::dxgi::caps::Adapter,
    pub backing: super::resource::Backing,
}

enum Kind {
    Device(Device),
    ImmediateContext(Box<ContextState>),
    Shader {
        desc: Box<Shader>,
        linkage: u32,
    },
    ClassLinkage(Vec<Arc<crate::shader::dxbc::linkage::Linkage>>),
    ClassInstance(Box<ClassInstance>),
    InputLayout(Box<super::desc::InputLayout>),
    Query(Box<query::Query>),
    Resource {
        desc: Resource,
        priority: u32,
        min_lod: f32,
        mapping: Option<map::Storage>,
        initial: Vec<Vec<u8>>,
        #[cfg(feature = "gpu")]
        pub_dc: u32,
        #[cfg(feature = "gpu")]
        backing: Option<super::resource::Backing>,
        #[cfg(feature = "gpu")]
        stream_cursor: Option<adv_gpu::Buffer>,
        #[cfg(feature = "gpu")]
        shared: Option<Arc<Shared>>,
        #[cfg(feature = "gpu")]
        pending: BTreeMap<u32, std::sync::mpsc::Receiver<()>>,
    },
    View {
        desc: Arc<View>,
        resource: u32,
        #[cfg(feature = "gpu")]
        counter: Option<adv_gpu::Buffer>,
    },
    Pipeline(Box<PipelineState>),
}

pub(super) struct ClassInstance {
    pub linkage: u32,
    pub class: Vec<u8>,
    pub name: Vec<u8>,
    pub desc: adv_abi::types::D3D11_CLASS_INSTANCE_DESC,
}

pub(super) struct Device {
    report: Arc<Report>,
    pub(crate) flags: u32,
    pub(crate) exception_mode: u32,
    pub(crate) immediate: u32,
    pub(crate) adapter: u32,
    pub(crate) null_device: bool,
    pub(crate) priority: i32,
    pub(crate) latency: u32,
    pub(crate) thread_lock: u32,
    pub(crate) multithreaded: bool,
    states: [BTreeMap<Vec<u8>, u32>; 4],
}

impl Kind {
    fn interfaces(&self) -> (Iid, &'static [Iid]) {
        const DEVICE_CHILD: &[Iid] = &[IID_ID3D11DEVICECHILD];
        const RESOURCE: &[Iid] = &[IID_ID3D11DEVICECHILD, IID_ID3D11RESOURCE];
        const VIEW: &[Iid] = &[IID_ID3D11DEVICECHILD, IID_ID3D11VIEW];
        match self {
            Self::Device(_) => (IID_ID3D11DEVICE1, &[IID_ID3D11DEVICE]),
            Self::ImmediateContext(_) => (
                IID_ID3D11DEVICECONTEXT1,
                &[IID_ID3D11DEVICECONTEXT, IID_ID3D11DEVICECHILD],
            ),
            Self::Query(query) if query.predicate => (
                IID_ID3D11PREDICATE,
                &[
                    IID_ID3D11DEVICECHILD,
                    IID_ID3D11ASYNCHRONOUS,
                    IID_ID3D11QUERY,
                ],
            ),
            Self::Query(_) => (
                IID_ID3D11QUERY,
                &[IID_ID3D11DEVICECHILD, IID_ID3D11ASYNCHRONOUS],
            ),
            Self::InputLayout(_) => (IID_ID3D11INPUTLAYOUT, DEVICE_CHILD),
            Self::ClassLinkage(_) => (IID_ID3D11CLASSLINKAGE, DEVICE_CHILD),
            Self::ClassInstance(_) => (IID_ID3D11CLASSINSTANCE, DEVICE_CHILD),
            Self::Shader { desc: shader, .. } => {
                use crate::shader::ir::Stage;
                let iid = match shader.stage {
                    Stage::Vertex => IID_ID3D11VERTEXSHADER,
                    Stage::Pixel => IID_ID3D11PIXELSHADER,
                    Stage::Compute => IID_ID3D11COMPUTESHADER,
                    Stage::Geometry => IID_ID3D11GEOMETRYSHADER,
                    Stage::Hull | Stage::Domain => unreachable!(),
                };
                (iid, DEVICE_CHILD)
            }
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
    #[cfg(feature = "gpu")]
    pub(super) fn shared(&self) -> Option<&Arc<Shared>> {
        match &self.kind {
            Kind::Resource { shared, .. } => shared.as_ref(),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn share(&mut self, alias: Arc<Shared>) {
        let Kind::Resource {
            backing, shared, ..
        } = &mut self.kind
        else {
            unreachable!()
        };
        *backing = Some(alias.backing.clone());
        *shared = Some(alias);
    }

    pub fn resource(&self) -> Option<Resource> {
        match &self.kind {
            Kind::Resource { desc, .. } => Some(*desc),
            _ => None,
        }
    }

    pub fn view(&self) -> Option<(u32, &Arc<View>)> {
        match &self.kind {
            Kind::View { resource, desc, .. } => Some((*resource, desc)),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn dc(&mut self) -> Option<&mut u32> {
        match &mut self.kind {
            Kind::Resource { pub_dc, .. } => Some(pub_dc),
            _ => None,
        }
    }

    pub fn mapping(&mut self) -> Option<&mut map::Storage> {
        match &mut self.kind {
            Kind::Resource { mapping, .. } => mapping.as_mut(),
            _ => None,
        }
    }

    pub fn initial_data(&self, subresource: u32) -> Option<&[u8]> {
        match &self.kind {
            Kind::Resource { initial, .. } => initial.get(subresource as usize).map(Vec::as_slice),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn backing(&mut self) -> Option<&mut Option<super::resource::Backing>> {
        match &mut self.kind {
            Kind::Resource { backing, .. } => Some(backing),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn pending(&mut self) -> &mut BTreeMap<u32, std::sync::mpsc::Receiver<()>> {
        match &mut self.kind {
            Kind::Resource { pending, .. } => pending,
            _ => unreachable!(),
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn take_initial_data(&mut self) -> Vec<Vec<u8>> {
        match &mut self.kind {
            Kind::Resource { initial, .. } => std::mem::take(initial),
            _ => Vec::new(),
        }
    }

    pub(super) fn set_initial_data(&mut self, data: Vec<Vec<u8>>) {
        if let Kind::Resource { initial, .. } = &mut self.kind {
            *initial = data;
        }
    }

    pub(super) fn query(&self) -> Option<&query::Query> {
        match &self.kind {
            Kind::Query(query) => Some(query),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn query_mut(&mut self) -> Option<&mut query::Query> {
        match &mut self.kind {
            Kind::Query(query) => Some(query),
            _ => None,
        }
    }

    pub fn input_layout(&self) -> Option<&super::desc::InputLayout> {
        match &self.kind {
            Kind::InputLayout(layout) => Some(layout),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn counter(&mut self) -> Option<&mut Option<adv_gpu::Buffer>> {
        match &mut self.kind {
            Kind::View { counter, .. } => Some(counter),
            _ => None,
        }
    }

    pub fn pipeline(&self) -> Option<&PipelineState> {
        match &self.kind {
            Kind::Pipeline(pipeline) => Some(pipeline),
            _ => None,
        }
    }

    pub(super) fn min_lod(&self) -> Option<f32> {
        match self.kind {
            Kind::Resource { min_lod, .. } => Some(min_lod),
            _ => None,
        }
    }

    pub(super) fn set_min_lod(&mut self, value: f32) {
        if let Kind::Resource { min_lod, .. } = &mut self.kind {
            *min_lod = value;
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

    pub(super) fn device_state(&self) -> Option<&Device> {
        match &self.kind {
            Kind::Device(device) => Some(device),
            _ => None,
        }
    }

    pub(super) fn device_state_mut(&mut self) -> Option<&mut Device> {
        match &mut self.kind {
            Kind::Device(device) => Some(device),
            _ => None,
        }
    }

    pub fn immediate(&self) -> Option<u32> {
        self.device_state().map(|device| device.immediate)
    }

    pub fn context(&self) -> Option<&ContextState> {
        match &self.kind {
            Kind::ImmediateContext(state) => Some(state),
            _ => None,
        }
    }

    pub fn context_mut(&mut self) -> Option<&mut ContextState> {
        match &mut self.kind {
            Kind::ImmediateContext(state) => Some(state),
            _ => None,
        }
    }

    #[cfg(feature = "gpu")]
    pub(super) fn stream_cursor(&mut self) -> Option<&mut Option<adv_gpu::Buffer>> {
        match &mut self.kind {
            Kind::Resource { stream_cursor, .. } => Some(stream_cursor),
            _ => None,
        }
    }

    pub fn shader(&self) -> Option<&Shader> {
        match &self.kind {
            Kind::Shader { desc, .. } => Some(desc),
            _ => None,
        }
    }

    pub(super) fn shader_linkage(&self) -> Option<u32> {
        match &self.kind {
            Kind::Shader { linkage, .. } if *linkage != 0 => Some(*linkage),
            _ => None,
        }
    }

    pub(super) fn class_linkage(&self) -> Option<&[Arc<crate::shader::dxbc::linkage::Linkage>]> {
        match &self.kind {
            Kind::ClassLinkage(shaders) => Some(shaders),
            _ => None,
        }
    }

    pub(super) fn class_instance(&self) -> Option<&ClassInstance> {
        match &self.kind {
            Kind::ClassInstance(instance) => Some(instance),
            _ => None,
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
    pub allocations: Vec<u32>,
    pub private_interfaces: Vec<u32>,
    #[cfg(feature = "gpu")]
    pub(super) shared: Vec<u32>,
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

    pub fn get(self, objects: &Objects, pointer: u32) -> Option<&Object> {
        self.com.get(objects, pointer)
    }

    pub fn get_mut(self, objects: &mut Objects, pointer: u32) -> Option<&mut Object> {
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
                flags: 0,
                exception_mode: 0,
                immediate: 0,
                adapter: 0,
                null_device: false,
                priority: 0,
                latency: 3,
                thread_lock: 0,
                multithreaded: false,
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
        self.attach(
            objects,
            pointer,
            device,
            Kind::ImmediateContext(Box::default()),
        )
    }

    pub(super) fn attach_query(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        query: query::Query,
    ) -> Result<(), Error> {
        self.attach(objects, pointer, device, Kind::Query(Box::new(query)))
    }

    pub(super) fn attach_input_layout(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        layout: super::desc::InputLayout,
    ) -> Result<(), Error> {
        self.attach(
            objects,
            pointer,
            device,
            Kind::InputLayout(Box::new(layout)),
        )
    }

    pub(super) fn register_faces(
        self,
        objects: &mut Objects,
        pointer: u32,
        face: u32,
        interfaces: &[Iid],
    ) {
        let identity = self.com.identity::<Object>(objects, pointer).unwrap();
        for &iid in interfaces {
            assert!(self.com.register_interface(objects, identity, iid, face));
        }
    }

    pub(super) fn attach_class_linkage(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
    ) -> Result<(), Error> {
        self.attach(objects, pointer, device, Kind::ClassLinkage(Vec::new()))
    }

    pub(super) fn attach_class_instance(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        instance: ClassInstance,
    ) -> Result<(), Error> {
        let linkage = instance.linkage;
        self.retain_bound(objects, linkage)?;
        let result = self.attach(
            objects,
            pointer,
            device,
            Kind::ClassInstance(Box::new(instance)),
        );
        if result.is_err() {
            self.release_internal(objects, linkage, &mut Release::default());
        }
        result
    }

    pub fn attach_shader(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        shader: Shader,
        linkage: u32,
    ) -> Result<(), Error> {
        if linkage != 0 {
            self.retain_bound(objects, linkage)?;
        }
        let metadata = shader.linkage.clone();
        let result = self.attach(
            objects,
            pointer,
            device,
            Kind::Shader {
                desc: Box::new(shader),
                linkage,
            },
        );
        if linkage != 0 {
            if result.is_err() {
                self.release_internal(objects, linkage, &mut Release::default());
            } else if let Some(metadata) = metadata
                && let Kind::ClassLinkage(shaders) =
                    &mut self.get_mut(objects, linkage).unwrap().kind
            {
                shaders.push(metadata);
            }
        }
        result
    }

    pub fn attach_resource(
        self,
        objects: &mut Objects,
        pointer: u32,
        device: u32,
        desc: Resource,
        mapping: Option<map::Storage>,
    ) -> Result<(), Error> {
        self.attach(
            objects,
            pointer,
            device,
            Kind::Resource {
                desc,
                priority: 0x78000000,
                min_lod: 0.,
                mapping,
                initial: Vec::new(),
                #[cfg(feature = "gpu")]
                pub_dc: 0,
                #[cfg(feature = "gpu")]
                backing: None,
                #[cfg(feature = "gpu")]
                stream_cursor: None,
                #[cfg(feature = "gpu")]
                shared: None,
                #[cfg(feature = "gpu")]
                pending: BTreeMap::new(),
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
                #[cfg(feature = "gpu")]
                counter: None,
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

    pub(super) fn key(self, objects: &Objects, pointer: u32) -> Option<(u64, u64)> {
        self.com
            .identity::<Object>(objects, pointer)
            .map(|identity| identity.key())
    }

    pub(super) fn allocation_key(self, objects: &Objects, pointer: u32) -> Option<(u64, u64)> {
        #[cfg(feature = "gpu")]
        if let Some(shared) = self.get(objects, pointer).and_then(Object::shared) {
            return Some(shared.identity);
        }
        self.key(objects, pointer)
    }

    pub(super) fn canonical(self, objects: &Objects, pointer: u32) -> Option<u32> {
        self.com
            .canonical(objects, self.com.identity::<Object>(objects, pointer)?)
    }

    #[cfg(feature = "gpu")]
    pub(super) fn buffer_referenced(self, objects: &Objects, pointer: u32) -> bool {
        let identity = self.com.identity::<Object>(objects, pointer).unwrap();
        self.com.references(objects, identity) != Some(0)
            || self.com.internal_references(objects, identity) != Some(1)
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

    pub(super) fn drop_binding(self, objects: &mut Objects, pointer: u32, released: &mut Release) {
        if pointer != 0 {
            self.release_internal(objects, pointer, released);
        }
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
        #[cfg(feature = "gpu")]
        if let Some(shared) = destroyed.value.shared()
            && Arc::strong_count(shared) == 1
        {
            result.shared.push(shared.handle);
        }
        let Object {
            device,
            kind,
            private,
        } = destroyed.value;
        result.allocations.extend(destroyed.interfaces);
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
            Kind::Resource {
                mapping: Some(mapping),
                ..
            } => {
                result.allocations.push(mapping.pointer());
            }
            Kind::Device(owner) => {
                if owner.thread_lock != 0 {
                    objects.close(owner.thread_lock);
                }
                if owner.adapter != 0 {
                    result.private_interfaces.push(owner.adapter);
                }
                if owner.immediate != 0 {
                    self.release_internal(objects, owner.immediate, result);
                }
            }
            Kind::ImmediateContext(state) => {
                state
                    .groups()
                    .for_each_binding(|pointer| self.release_internal(objects, pointer, result));
            }
            Kind::ClassInstance(instance) => {
                self.release_internal(objects, instance.linkage, result)
            }
            Kind::Shader { linkage, .. } if linkage != 0 => {
                self.release_internal(objects, linkage, result)
            }
            Kind::Shader { .. }
            | Kind::ClassLinkage(_)
            | Kind::InputLayout(_)
            | Kind::Query(_)
            | Kind::Resource { mapping: None, .. } => (),
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
