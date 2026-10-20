//! Guest shader identities and bounded device constant register files.
//! Translation is performed at creation; valid but untranslated programs remain
//! queryable objects and carry a diagnostic for the draw frontend.
use super::*;
use crate::shader::Stage;
use adv_device::gpu::compile::{Compile, CompileCounters};
use std::sync::Arc;
pub(super) mod stateblocks;
#[cfg(test)]
mod tests;

const IID_VERTEX: Iid = [
    0x7e, 0x55, 0xc5, 0xef, 0x65, 0x62, 0x13, 0x46, 0x8a, 0x94, 0x43, 0x85, 0x78, 0x89, 0xeb, 0x36,
];
const IID_PIXEL: Iid = [
    0xdc, 0xdb, 0x3b, 0x6d, 0x02, 0x5b, 0x15, 0x44, 0xb8, 0x52, 0xce, 0x5e, 0x8b, 0xcc, 0xb2, 0x89,
];

#[derive(Clone)]
pub(super) struct TranslationJob(pub Compile<Arc<crate::shader::Translation>>);
impl TranslationJob {
    pub fn as_ref(&self) -> Result<&Arc<crate::shader::Translation>, &String> {
        self.0.wait().as_ref()
    }
}
#[derive(Default)]
pub(super) struct TranslationCache {
    entries: BTreeMap<Vec<u32>, TranslationJob>,
    bytes: usize,
    pub counters: CompileCounters,
    failures: Arc<std::sync::atomic::AtomicU64>,
}
impl TranslationCache {
    fn start(
        &mut self,
        shader: &crate::shader::Shader,
        directory: Option<&std::path::Path>,
    ) -> TranslationJob {
        if let Some(translation) = self.entries.get(shader.bytecode()) {
            return translation.clone();
        }
        self.bytes = self
            .entries
            .iter()
            .map(|(tokens, job)| {
                tokens.len() * 4
                    + job.0.ready().and_then(|r| r.as_ref().ok()).map_or(
                        crate::shader::MAX_TRANSLATION_BYTES,
                        |t| {
                            t.source.len()
                                + t.samplers.len()
                                    * std::mem::size_of::<crate::shader::SamplerBinding>()
                        },
                    )
            })
            .sum();
        let bytes = shader.bytecode().len() * 4 + crate::shader::MAX_TRANSLATION_BYTES;
        let parsed = shader.clone();
        let directory = directory.map(std::path::Path::to_owned);
        let failures = self.failures.clone();
        let job = TranslationJob(Compile::spawn(bytes, self.counters.clone(), move || {
            let result = match directory {
                Some(directory) => parsed.translate_cached(&directory),
                None => parsed.translate(),
            }
            .map(Arc::new)
            .map_err(|e| e.to_string());
            if result.is_err() {
                failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            result
        }));
        const MAX_BYTES: usize = 8 * 1024 * 1024;
        if bytes <= MAX_BYTES {
            if self.entries.len() == 256 || self.bytes + bytes > MAX_BYTES {
                self.entries.clear();
                self.bytes = 0;
            }
            self.entries.insert(shader.bytecode().to_vec(), job.clone());
            self.bytes += bytes;
        }
        job
    }
    #[cfg(test)]
    fn translate(
        &mut self,
        shader: &crate::shader::Shader,
        directory: Option<&std::path::Path>,
    ) -> Result<Arc<crate::shader::Translation>, String> {
        let job = self.start(shader, directory);
        job.0.wait().clone()
    }
    pub fn failures(&self) -> u64 {
        self.failures.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub(super) struct Shader {
    pub pointer: u32,
    pub device: Id,
    pub parsed: Arc<crate::shader::Shader>,
    pub translation: TranslationJob,
}
#[derive(Clone)]
pub(super) struct Constants {
    pub floats: [[f32; 4]; 256],
    pub integers: [[i32; 4]; 16],
    pub booleans: [i32; 16],
}
impl Default for Constants {
    fn default() -> Self {
        Self {
            floats: [[0.; 4]; 256],
            integers: [[0; 4]; 16],
            booleans: [0; 16],
        }
    }
}
#[derive(Clone, Default)]
pub(super) struct Bindings {
    pub vertex: Option<Id>,
    pub pixel: Option<Id>,
    pub vertex_constants: Constants,
    pub pixel_constants: Constants,
    /// Extra pixel/vertex sampler texture slots. Slot zero is represented by
    /// Device::texture so existing fixed-function bindings share one reference.
    pub textures: [Option<Id>; 20],
}
impl Bindings {
    fn binding(&self, stage: Stage) -> Option<Id> {
        match stage {
            Stage::Vertex => self.vertex,
            Stage::Pixel => self.pixel,
        }
    }
    pub(super) fn constants(&self, stage: Stage) -> &Constants {
        match stage {
            Stage::Vertex => &self.vertex_constants,
            Stage::Pixel => &self.pixel_constants,
        }
    }
    fn constants_mut(&mut self, stage: Stage) -> &mut Constants {
        match stage {
            Stage::Vertex => &mut self.vertex_constants,
            Stage::Pixel => &mut self.pixel_constants,
        }
    }
}
pub(super) fn register(registry: &mut Registry) {
    for api in [
        vertex_query_ABI,
        vertex_add_ref_ABI,
        vertex_release_ABI,
        pixel_query_ABI,
        pixel_add_ref_ABI,
        pixel_release_ABI,
        vertex_device_ABI,
        pixel_device_ABI,
        vertex_function_ABI,
        pixel_function_ABI,
        create_vertex_ABI,
        create_pixel_ABI,
        adv_abi::Api {
            env_leaf: Some(super::leaf_calls::binding),
            ..set_vertex_ABI
        },
        get_vertex_ABI,
        adv_abi::Api {
            env_leaf: Some(super::leaf_calls::binding),
            ..set_pixel_ABI
        },
        get_pixel_ABI,
        set_vertex_f_ABI,
        get_vertex_f_ABI,
        set_vertex_i_ABI,
        get_vertex_i_ABI,
        set_vertex_b_ABI,
        get_vertex_b_ABI,
        set_pixel_f_ABI,
        get_pixel_f_ABI,
        set_pixel_i_ABI,
        get_pixel_i_ABI,
        set_pixel_b_ABI,
        get_pixel_b_ABI,
    ] {
        registry.register(api);
    }
    stateblocks::register(registry);
}
unknown!(
    vertex_query,
    vertex_add_ref,
    vertex_release,
    "IDirect3DVertexShader9::QueryInterface",
    "IDirect3DVertexShader9::AddRef",
    "IDirect3DVertexShader9::Release"
);
unknown!(
    pixel_query,
    pixel_add_ref,
    pixel_release,
    "IDirect3DPixelShader9::QueryInterface",
    "IDirect3DPixelShader9::AddRef",
    "IDirect3DPixelShader9::Release"
);
pub(super) fn bound<'a>(env: &'a Env<'_>, id: Id) -> Option<&'a Shader> {
    match com(env).get_identity(&env.sys.objects, id)? {
        Payload::Shader(s) => Some(s),
        _ => None,
    }
}
fn shader<'a>(env: &'a Env<'_>, pointer: u32) -> Option<&'a Shader> {
    bound(
        env,
        com(env).identity::<Payload>(&env.sys.objects, pointer)?,
    )
}
pub(super) fn parent_on_last_public_release(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let s = shader(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 1).then_some(s.device)
}
pub(super) fn parent_on_first_public_retain(env: &Env<'_>, pointer: u32) -> Option<Id> {
    let s = shader(env, pointer)?;
    let id = com(env).identity::<Payload>(&env.sys.objects, pointer)?;
    (com(env).references(&env.sys.objects, id)? == 0).then_some(s.device)
}
fn device_id(env: &Env<'_>, pointer: u32) -> Option<Id> {
    device(env, pointer)?;
    com(env).identity::<Payload>(&env.sys.objects, pointer)
}
fn read_tokens(env: &Env<'_>, input: GuestPtr<u32>) -> Result<Vec<u32>, u32> {
    // The frontend owns instruction framing so END-valued immediate/comment
    // words are never mistaken for the end of a shader.
    crate::shader::read_bytecode(|offset| input.offset(offset as u32)?.read(env.cpu))
        .map_err(|_| INVALID)
}
fn create_shader(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    input: GuestPtr<u32>,
    output: GuestPtr<u32>,
    stage: Stage,
) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(device) = device_id(env, this.addr()) else {
        return INVALID;
    };
    let tokens = match read_tokens(env, input) {
        Ok(v) => v,
        Err(e) => {
            env.ext.get_mut::<State>().shader_stats.malformed += 1;
            return e;
        }
    };
    let parsed = match crate::shader::Shader::parse(&tokens) {
        Ok(s) if s.stage == stage => s,
        Ok(_) => {
            env.ext.get_mut::<State>().shader_stats.malformed += 1;
            return INVALID;
        }
        Err(error) => {
            env.ext.get_mut::<State>().shader_stats.malformed += 1;
            log::warn!("D3D9 rejected {:?} shader bytecode: {error}", stage);
            return INVALID;
        }
    };
    let state = env.ext.get_mut::<State>();
    let translation = state
        .shader_translations
        .start(&parsed, state.shader_cache.as_deref());
    let model = match (parsed.version.major, parsed.version.minor) {
        (1, minor) => minor as usize - 1,
        (2, minor) => 4 + minor as usize,
        (3, 0) => 6,
        _ => return INVALID,
    };
    log::debug!(
        "D3D9 created {:?} shader {}.{} ({} tokens)",
        stage,
        parsed.version.major,
        parsed.version.minor,
        parsed.bytecode().len()
    );
    if com(env)
        .add_ref_identity(&mut env.sys.objects, device)
        .is_none()
    {
        return OUT_OF_MEMORY;
    }
    let (interface, iid) = match stage {
        Stage::Vertex => ("IDirect3DVertexShader9", IID_VERTEX),
        Stage::Pixel => ("IDirect3DPixelShader9", IID_PIXEL),
    };
    let Some((pointer, id)) = object(
        env,
        interface,
        iid,
        Payload::Shader(Shader {
            pointer: 0,
            device,
            parsed: Arc::new(parsed),
            translation,
        }),
    ) else {
        let _ = com(env).release_identity(&mut env.sys.objects, device);
        return OUT_OF_MEMORY;
    };
    if let Some(Payload::Shader(s)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        s.pointer = pointer;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    let stats = &mut env.ext.get_mut::<State>().shader_stats;
    match stage {
        Stage::Vertex => stats.vertex[model] += 1,
        Stage::Pixel => stats.pixel[model] += 1,
    };
    super::programmable::warm(env, this.addr(), Some((stage, id)));
    0
}
#[win32(d3d9,name="IDirect3DDevice9::CreateVertexShader",level=implement)]
pub(super) fn create_vertex(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    input: GuestPtr<u32>,
    output: GuestPtr<u32>,
) -> u32 {
    create_shader(env, this, input, output, Stage::Vertex)
}
#[win32(d3d9,name="IDirect3DDevice9::CreatePixelShader",level=implement)]
pub(super) fn create_pixel(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    input: GuestPtr<u32>,
    output: GuestPtr<u32>,
) -> u32 {
    create_shader(env, this, input, output, Stage::Pixel)
}
fn get_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>, stage: Stage) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(s) = shader(env, this.addr()).filter(|s| s.parsed.stage == stage) else {
        return INVALID;
    };
    let Some(Payload::Device(d)) = com(env).get_identity(&env.sys.objects, s.device) else {
        return INVALID;
    };
    let pointer = d.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DVertexShader9::GetDevice",level=implement)]
fn vertex_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_device(env, this, output, Stage::Vertex)
}
#[win32(d3d9,name="IDirect3DPixelShader9::GetDevice",level=implement)]
fn pixel_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_device(env, this, output, Stage::Pixel)
}
fn function(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u8>,
    size: GuestPtr<u32>,
    stage: Stage,
) -> u32 {
    let Some(s) = shader(env, this.addr()).filter(|s| s.parsed.stage == stage) else {
        return INVALID;
    };
    let required = s.parsed.bytecode().len() as u32 * 4;
    if adv_abi::guest::check_range(env.cpu, size.addr(), 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    if output.is_null() {
        return size.write(env.cpu, required).map_or(INVALID, |()| 0);
    }
    // Native D3D9 writes the complete function and updates pSizeOfData even
    // when its incoming value is smaller. Validate the actual guest span.
    if adv_abi::guest::check_range(env.cpu, output.addr(), required, Prot::WRITE).is_err() {
        return INVALID;
    }
    let bytes: Vec<u8> = s
        .parsed
        .bytecode()
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    if env.cpu.write(output.addr(), &bytes).is_err() {
        return INVALID;
    }
    size.write(env.cpu, required).map_or(INVALID, |()| 0)
}
#[win32(d3d9,name="IDirect3DVertexShader9::GetFunction",level=implement)]
fn vertex_function(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u8>,
    size: GuestPtr<u32>,
) -> u32 {
    function(env, this, output, size, Stage::Vertex)
}
#[win32(d3d9,name="IDirect3DPixelShader9::GetFunction",level=implement)]
fn pixel_function(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<u8>,
    size: GuestPtr<u32>,
) -> u32 {
    function(env, this, output, size, Stage::Pixel)
}
fn set_shader(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>, stage: Stage) -> u32 {
    let Some(id) = device_id(env, this.addr()) else {
        return INVALID;
    };
    let new = if pointer.is_null() {
        None
    } else {
        let Some(new) = com(env).identity::<Payload>(&env.sys.objects, pointer.addr()) else {
            return INVALID;
        };
        Some(new)
    };
    let d = device(env, this.addr()).unwrap();
    if d.recording.is_none() && d.shaders.binding(stage) == new {
        return 0;
    }
    if new.is_some_and(|new| {
        bound(env, new).is_none_or(|s| s.device != id || s.parsed.stage != stage)
    }) {
        return INVALID;
    }
    if let Some(code) = stateblocks::record_shader(env, id, stage, new) {
        return code;
    }
    let old = device(env, this.addr()).unwrap().shaders.binding(stage);
    if new != old && new.is_some_and(|new| !com(env).retain_internal(&mut env.sys.objects, new)) {
        return OUT_OF_MEMORY;
    }
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    d.versions[8 + usize::from(stage == Stage::Pixel)] += 1;
    match stage {
        Stage::Vertex => d.shaders.vertex = new,
        Stage::Pixel => d.shaders.pixel = new,
    };
    if new != old {
        release_binding(env, old);
    }
    0
}
fn get_shader(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>, stage: Stage) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(id) = d.shaders.binding(stage) else {
        return 0;
    };
    let Some(s) = bound(env, id) else {
        return INVALID;
    };
    let pointer = s.pointer;
    if add_ref(env, pointer).is_none() {
        return OUT_OF_MEMORY;
    }
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9,name="IDirect3DDevice9::SetVertexShader",level=state)]
pub(super) fn set_vertex(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> u32 {
    set_shader(env, this, pointer, Stage::Vertex)
}
#[win32(d3d9,name="IDirect3DDevice9::GetVertexShader",level=implement,leaf)]
pub(super) fn get_vertex(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_shader(env, this, output, Stage::Vertex)
}
#[win32(d3d9,name="IDirect3DDevice9::SetPixelShader",level=state)]
pub(super) fn set_pixel(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> u32 {
    set_shader(env, this, pointer, Stage::Pixel)
}
#[win32(d3d9,name="IDirect3DDevice9::GetPixelShader",level=implement,leaf)]
pub(super) fn get_pixel(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_shader(env, this, output, Stage::Pixel)
}
pub(super) fn release_binding(env: &mut Env<'_>, id: Option<Id>) {
    if let Some(id) = id
        && let Some(Some(dead)) = com(env).release_internal(&mut env.sys.objects, id)
    {
        destroy(env, dead);
    }
}
pub(super) fn release_bindings(env: &mut Env<'_>, bindings: Bindings) {
    release_binding(env, bindings.vertex);
    release_binding(env, bindings.pixel);
    for id in bindings.textures {
        release_binding(env, id);
    }
}
pub(super) fn reset_bindings(env: &mut Env<'_>, id: Id) {
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return;
    };
    for v in &mut d.versions {
        *v += 1;
    }
    let old = std::mem::take(&mut d.shaders);
    let recording = d.recording.take();
    release_bindings(env, old);
    if let Some(recording) = recording {
        stateblocks::release_snapshot(env, recording);
    }
}
#[derive(Clone, Copy)]
pub(super) enum ConstantKind {
    Float,
    Integer,
    Boolean,
}
fn constant_limit(stage: Stage, kind: ConstantKind) -> u32 {
    match kind {
        ConstantKind::Float => match stage {
            Stage::Vertex => 256,
            Stage::Pixel => 224,
        },
        _ => 16,
    }
}
fn constant_range(start: u32, count: u32, limit: u32) -> Option<std::ops::Range<usize>> {
    let end = start.checked_add(count)?;
    (start <= limit && end <= limit).then_some(start as usize..end as usize)
}
fn constants_set(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    start: u32,
    input: GuestPtr<u8>,
    count: u32,
    stage: Stage,
    kind: ConstantKind,
) -> u32 {
    let Some(id) = device_id(env, this.addr()) else {
        return INVALID;
    };
    let Some(range) = constant_range(start, count, constant_limit(stage, kind)) else {
        return INVALID;
    };
    let width = match kind {
        ConstantKind::Boolean => 4,
        _ => 16,
    };
    let bytes = count * width;
    if adv_abi::guest::check_range(env.cpu, input.addr(), bytes, Prot::READ).is_err() {
        return INVALID;
    }
    let mut data = vec![0u8; bytes as usize];
    if bytes != 0 && env.cpu.read(input.addr(), &mut data).is_err() {
        return INVALID;
    }
    store_constants(env, id, range, &data, stage, kind)
}
/// Host-side constant upload for a D3DX Effect pass (GFX-EFFECT-3): the same
/// validation and storage as the API without staging data in guest memory.
pub(super) fn set_constants_direct(
    env: &mut Env<'_>,
    this: u32,
    slot: u32,
    start: u32,
    data: &[u8],
    count: u32,
) -> u32 {
    let (stage, kind) = match slot {
        94 => (Stage::Vertex, ConstantKind::Float),
        96 => (Stage::Vertex, ConstantKind::Integer),
        98 => (Stage::Vertex, ConstantKind::Boolean),
        109 => (Stage::Pixel, ConstantKind::Float),
        111 => (Stage::Pixel, ConstantKind::Integer),
        _ => (Stage::Pixel, ConstantKind::Boolean),
    };
    let Some(id) = device_id(env, this) else {
        return INVALID;
    };
    let Some(range) = constant_range(start, count, constant_limit(stage, kind)) else {
        return INVALID;
    };
    let width = match kind {
        ConstantKind::Boolean => 4,
        _ => 16,
    };
    if data.len() < (count * width) as usize {
        return INVALID;
    }
    store_constants(
        env,
        id,
        range,
        &data[..(count * width) as usize],
        stage,
        kind,
    )
}
fn store_constants(
    env: &mut Env<'_>,
    id: Id,
    range: std::ops::Range<usize>,
    data: &[u8],
    stage: Stage,
    kind: ConstantKind,
) -> u32 {
    let width = match kind {
        ConstantKind::Boolean => 4,
        _ => 16,
    };
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    let constants = if let Some(recording) = d.recording.as_mut() {
        recording.mark_constants(stage, kind, range.clone());
        recording.shaders.constants_mut(stage)
    } else {
        d.versions[13 + usize::from(stage == Stage::Pixel)] += 1;
        d.shaders.constants_mut(stage)
    };
    for (register, bytes) in range.zip(data.chunks_exact(width as usize)) {
        match kind {
            ConstantKind::Float => {
                constants.floats[register] = std::array::from_fn(|i| {
                    f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                })
            }
            ConstantKind::Integer => {
                constants.integers[register] = std::array::from_fn(|i| {
                    i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                })
            }
            ConstantKind::Boolean => {
                constants.booleans[register] = i32::from_le_bytes(bytes.try_into().unwrap())
            }
        }
    }
    0
}
fn constants_get(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    start: u32,
    output: GuestPtr<u8>,
    count: u32,
    stage: Stage,
    kind: ConstantKind,
) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let Some(range) = constant_range(start, count, constant_limit(stage, kind)) else {
        return INVALID;
    };
    let constants = d.shaders.constants(stage);
    let bytes: Vec<u8> = match kind {
        ConstantKind::Float => constants.floats[range]
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        ConstantKind::Integer => constants.integers[range]
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
        ConstantKind::Boolean => constants.booleans[range]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
    };
    if adv_abi::guest::check_range(env.cpu, output.addr(), bytes.len() as u32, Prot::WRITE).is_err()
    {
        return INVALID;
    }
    if bytes.is_empty() {
        return 0;
    }
    env.cpu.write(output.addr(), &bytes).map_or(INVALID, |()| 0)
}
macro_rules! constant_api {
    ($set:ident,$get:ident,$set_name:tt,$get_name:tt,$stage:ident,$kind:ident,$ty:ty) => {
        #[win32(d3d9,name=$set_name,level=state,leaf)]
        pub(super) fn $set(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            start: u32,
            input: GuestPtr<$ty>,
            count: u32,
        ) -> u32 {
            constants_set(
                env,
                this,
                start,
                input.cast(),
                count,
                Stage::$stage,
                ConstantKind::$kind,
            )
        }
        #[win32(d3d9,name=$get_name,level=implement,leaf)]
        pub(super) fn $get(
            env: &mut Env<'_>,
            this: GuestPtr<()>,
            start: u32,
            output: GuestPtr<$ty>,
            count: u32,
        ) -> u32 {
            constants_get(
                env,
                this,
                start,
                output.cast(),
                count,
                Stage::$stage,
                ConstantKind::$kind,
            )
        }
    };
}
constant_api!(
    set_vertex_f,
    get_vertex_f,
    "IDirect3DDevice9::SetVertexShaderConstantF",
    "IDirect3DDevice9::GetVertexShaderConstantF",
    Vertex,
    Float,
    f32
);
constant_api!(
    set_vertex_i,
    get_vertex_i,
    "IDirect3DDevice9::SetVertexShaderConstantI",
    "IDirect3DDevice9::GetVertexShaderConstantI",
    Vertex,
    Integer,
    i32
);
constant_api!(
    set_vertex_b,
    get_vertex_b,
    "IDirect3DDevice9::SetVertexShaderConstantB",
    "IDirect3DDevice9::GetVertexShaderConstantB",
    Vertex,
    Boolean,
    i32
);
constant_api!(
    set_pixel_f,
    get_pixel_f,
    "IDirect3DDevice9::SetPixelShaderConstantF",
    "IDirect3DDevice9::GetPixelShaderConstantF",
    Pixel,
    Float,
    f32
);
constant_api!(
    set_pixel_i,
    get_pixel_i,
    "IDirect3DDevice9::SetPixelShaderConstantI",
    "IDirect3DDevice9::GetPixelShaderConstantI",
    Pixel,
    Integer,
    i32
);
constant_api!(
    set_pixel_b,
    get_pixel_b,
    "IDirect3DDevice9::SetPixelShaderConstantB",
    "IDirect3DDevice9::GetPixelShaderConstantB",
    Pixel,
    Boolean,
    i32
);
