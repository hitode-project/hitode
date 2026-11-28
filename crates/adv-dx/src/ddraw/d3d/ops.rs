//! Direct3D COM bindings. The installed DirectDraw personality does not
//! register these entry points; the test assembly installs their tables.
use super::desc::state as decode;
mod execute;
mod legacy;
mod process;
#[path = "render.rs"]
mod render;
use super::{
    Face, INVALID, NO_INTERFACE, OUT_OF_MEMORY, State, caps,
    object::{self, Object},
};
use crate::fixed::Setting;
use crate::{
    argument::{Arguments, CallError},
    ddraw::{DirectDraw, creation, front},
};
use adv_abi::{Api, Env, ImplementationLevel, Outcome, Registry, com::Iid, gfx_iids::*, signature};
use adv_cpu::backend::Backend;
use adv_device::surface::{SurfaceMemory, SurfaceStore};
use adv_machine::{Machine, Objects};

macro_rules! bind {
    ($registry:ident, $face:ident, $interface:literal, $($method:ident),+ $(,)?) => {
        $(
            let signatures = signature::com_signatures();
            let index = signatures.iter().position(|sig| sig.module == $interface && sig.name == stringify!($method)).unwrap();
            let sig = &signatures[index];
            $registry.register_with_data(Api {
                module: "ddraw.dll",
                name: concat!($interface, "::", stringify!($method)),
                conv: sig.conv.unwrap(),
                handler: dispatch,
                level: ImplementationLevel::Implement,
            }, (Face::$face as u64) << 32 | index as u64);
        )+
    };
}

/// Register the Direct3D front in an isolated registry.
pub fn register(registry: &mut Registry) {
    front::palette::register(registry);
    bind!(
        registry,
        Direct3D,
        "IDirect3D",
        QueryInterface,
        AddRef,
        Release,
        CreateLight,
        CreateMaterial,
        CreateViewport
    );
    bind!(
        registry,
        Direct3D2,
        "IDirect3D2",
        QueryInterface,
        AddRef,
        Release,
        CreateLight,
        CreateMaterial,
        CreateViewport,
        CreateDevice
    );
    bind!(
        registry,
        Direct3D3,
        "IDirect3D3",
        QueryInterface,
        AddRef,
        Release,
        CreateLight,
        CreateMaterial,
        CreateViewport,
        CreateDevice
    );
    bind!(
        registry,
        Direct3D7,
        "IDirect3D7",
        QueryInterface,
        AddRef,
        Release,
        CreateDevice
    );
    bind!(
        registry,
        Device1,
        "IDirect3DDevice",
        QueryInterface,
        AddRef,
        Release,
        AddViewport,
        DeleteViewport,
        GetDirect3D
    );
    bind!(
        registry,
        Device2,
        "IDirect3DDevice2",
        QueryInterface,
        AddRef,
        Release,
        AddViewport,
        DeleteViewport,
        GetDirect3D
    );
    bind!(
        registry,
        Device3,
        "IDirect3DDevice3",
        QueryInterface,
        AddRef,
        Release,
        AddViewport,
        DeleteViewport,
        GetDirect3D
    );
    bind!(
        registry,
        Device7,
        "IDirect3DDevice7",
        QueryInterface,
        AddRef,
        Release,
        GetDirect3D
    );
    bind!(
        registry,
        Material1,
        "IDirect3DMaterial",
        QueryInterface,
        AddRef,
        Release,
        SetMaterial,
        GetMaterial,
        GetHandle
    );
    bind!(
        registry,
        Material2,
        "IDirect3DMaterial2",
        QueryInterface,
        AddRef,
        Release,
        SetMaterial,
        GetMaterial,
        GetHandle
    );
    bind!(
        registry,
        Material3,
        "IDirect3DMaterial3",
        QueryInterface,
        AddRef,
        Release,
        SetMaterial,
        GetMaterial,
        GetHandle
    );
    bind!(
        registry,
        Viewport3,
        "IDirect3DViewport3",
        QueryInterface,
        AddRef,
        Release,
        SetViewport,
        GetViewport,
        AddLight,
        DeleteLight
    );
    bind!(
        registry,
        Light,
        "IDirect3DLight",
        QueryInterface,
        AddRef,
        Release,
        SetLight,
        GetLight
    );
    bind!(
        registry,
        Texture1,
        "IDirect3DTexture",
        QueryInterface,
        AddRef,
        Release,
        GetHandle
    );
    bind!(
        registry,
        Texture2,
        "IDirect3DTexture2",
        QueryInterface,
        AddRef,
        Release,
        GetHandle
    );
    bind!(
        registry,
        Device7,
        "IDirect3DDevice7",
        SetTransform,
        GetTransform,
        MultiplyTransform,
        SetViewport,
        GetViewport,
        SetMaterial,
        GetMaterial,
        SetLight,
        GetLight,
        SetRenderState,
        GetRenderState,
        SetTextureStageState,
        GetTextureStageState,
        LightEnable,
        GetLightEnable,
        SetClipPlane,
        GetClipPlane,
        SetClipStatus,
        GetClipStatus,
        BeginStateBlock,
        EndStateBlock,
        CreateStateBlock,
        CaptureStateBlock,
        ApplyStateBlock,
        DeleteStateBlock
    );
    bind!(registry, Direct3D7, "IDirect3D7", CreateVertexBuffer);
    bind!(
        registry,
        VertexBuffer7,
        "IDirect3DVertexBuffer7",
        QueryInterface,
        AddRef,
        Release,
        Lock,
        Unlock,
        GetVertexBufferDesc,
        Optimize
    );
    bind!(
        registry,
        Device7,
        "IDirect3DDevice7",
        BeginScene,
        EndScene,
        Clear,
        SetRenderTarget,
        GetRenderTarget,
        SetTexture,
        GetTexture,
        PreLoad,
        DrawPrimitive,
        DrawIndexedPrimitive,
        DrawPrimitiveStrided,
        DrawIndexedPrimitiveStrided,
        DrawPrimitiveVB,
        DrawIndexedPrimitiveVB,
        ValidateDevice
    );
    bind!(registry, Direct3D, "IDirect3D", EnumDevices);
    bind!(registry, Direct3D2, "IDirect3D2", EnumDevices);
    bind!(
        registry,
        Direct3D3,
        "IDirect3D3",
        EnumDevices,
        EnumZBufferFormats
    );
    bind!(
        registry,
        Direct3D7,
        "IDirect3D7",
        EnumDevices,
        EnumZBufferFormats
    );
    bind!(
        registry,
        Device7,
        "IDirect3DDevice7",
        GetCaps,
        EnumTextureFormats
    );
    bind!(registry, Direct3D, "IDirect3D", Initialize, FindDevice);
    bind!(registry, Direct3D2, "IDirect3D2", FindDevice);
    bind!(
        registry,
        Direct3D3,
        "IDirect3D3",
        FindDevice,
        CreateVertexBuffer,
        EvictManagedTextures
    );
    bind!(registry, Direct3D7, "IDirect3D7", EvictManagedTextures);
    bind!(
        registry,
        Device1,
        "IDirect3DDevice",
        Initialize,
        GetCaps,
        SwapTextureHandles,
        CreateExecuteBuffer,
        GetStats,
        Execute,
        Pick,
        GetPickRecords,
        NextViewport,
        EnumTextureFormats,
        CreateMatrix,
        SetMatrix,
        GetMatrix,
        DeleteMatrix,
        BeginScene,
        EndScene
    );
    bind!(
        registry,
        Device2,
        "IDirect3DDevice2",
        GetCaps,
        SwapTextureHandles,
        GetStats,
        NextViewport,
        EnumTextureFormats,
        BeginScene,
        EndScene,
        SetCurrentViewport,
        GetCurrentViewport,
        SetRenderTarget,
        GetRenderTarget,
        Begin,
        BeginIndexed,
        Vertex,
        Index,
        End,
        GetRenderState,
        SetRenderState,
        GetLightState,
        SetLightState,
        SetTransform,
        GetTransform,
        MultiplyTransform,
        DrawPrimitive,
        DrawIndexedPrimitive,
        SetClipStatus,
        GetClipStatus
    );
    bind!(
        registry,
        Device3,
        "IDirect3DDevice3",
        GetCaps,
        GetStats,
        NextViewport,
        EnumTextureFormats,
        BeginScene,
        EndScene,
        SetCurrentViewport,
        GetCurrentViewport,
        SetRenderTarget,
        GetRenderTarget,
        Begin,
        BeginIndexed,
        Vertex,
        Index,
        End,
        GetRenderState,
        SetRenderState,
        GetLightState,
        SetLightState,
        SetTransform,
        GetTransform,
        MultiplyTransform,
        DrawPrimitive,
        DrawIndexedPrimitive,
        SetClipStatus,
        GetClipStatus,
        DrawPrimitiveStrided,
        DrawIndexedPrimitiveStrided,
        DrawPrimitiveVB,
        DrawIndexedPrimitiveVB,
        ComputeSphereVisibility,
        GetTexture,
        SetTexture,
        GetTextureStageState,
        SetTextureStageState,
        ValidateDevice
    );
    bind!(
        registry,
        Device7,
        "IDirect3DDevice7",
        ComputeSphereVisibility,
        Load,
        GetInfo
    );
    bind!(
        registry,
        Viewport3,
        "IDirect3DViewport3",
        Initialize,
        GetViewport2,
        SetViewport2,
        TransformVertices,
        LightElements,
        SetBackground,
        GetBackground,
        SetBackgroundDepth,
        GetBackgroundDepth,
        Clear,
        NextLight,
        SetBackgroundDepth2,
        GetBackgroundDepth2,
        Clear2
    );
    bind!(registry, Light, "IDirect3DLight", Initialize);
    bind!(
        registry,
        Material1,
        "IDirect3DMaterial",
        Initialize,
        Reserve,
        Unreserve
    );
    bind!(
        registry,
        Texture1,
        "IDirect3DTexture",
        Initialize,
        PaletteChanged,
        Load,
        Unload
    );
    bind!(
        registry,
        Texture2,
        "IDirect3DTexture2",
        PaletteChanged,
        Load
    );
    bind!(
        registry,
        VertexBuffer,
        "IDirect3DVertexBuffer",
        QueryInterface,
        AddRef,
        Release,
        Lock,
        Unlock,
        GetVertexBufferDesc,
        Optimize,
        ProcessVertices
    );
    bind!(
        registry,
        VertexBuffer7,
        "IDirect3DVertexBuffer7",
        ProcessVertices,
        ProcessVerticesStrided
    );
    bind!(
        registry,
        ExecuteBuffer,
        "IDirect3DExecuteBuffer",
        QueryInterface,
        AddRef,
        Release,
        Initialize,
        Lock,
        Unlock,
        SetExecuteData,
        GetExecuteData,
        Validate,
        Optimize
    );
}

/// Install the Direct3D registry tables after installing DirectDraw and the surface store.
pub fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    if std::env::var_os("ADV_DDRAW_LEGACY").is_some_and(|value| value == "1") {
        return Ok(());
    }
    front::palette::install(machine)?;
    let (state, store) = machine
        .ext
        .get2_installed_mut::<front::State, SurfaceStore>();
    let mut d3d = State {
        tables: Default::default(),
        devices: Vec::new(),
        gpu: store.fork_gpu().map_err(|e| e.to_string())?,
        queue: Default::default(),
        depths: Default::default(),
        programs: Default::default(),
        written: Default::default(),
        versions: Default::default(),
        counters: Default::default(),
    };
    for face in Face::ALL {
        let slots = signature::com_signatures()
            .iter()
            .filter(|sig| sig.module == face.name());
        let mut memory = (state.provider.memory)(&mut machine.cpu);
        let table = store.alloc(slots.clone().count() as u32 * 4, &mut memory)?;
        d3d.tables.insert(face, table);
        for sig in slots {
            let entry = machine
                .sys
                .thunks
                .publish(
                    &mut machine.cpu,
                    "ddraw.dll",
                    &format!("{}::{}", face.name(), sig.name),
                )
                .map_err(|e| e.to_string())?;
            let mut memory = (state.provider.memory)(&mut machine.cpu);
            memory.put_u32(table + u32::from(sig.com_slot.unwrap()) * 4, entry)?;
        }
    }
    let desktop = front::modes::current(store);
    state.draw.display = Some(front::modes::DisplayModes {
        desktop,
        requested: desktop,
        suspended: false,
    });
    state.draw.d3d = Some(d3d);
    Ok(())
}

fn dispatch(env: &mut Env<'_>) -> Outcome {
    if let Err(error) = front::State::sync_activation(env) {
        return error.outcome(env);
    }
    let binding = env.sys.thunks.get(env.thunk).f;
    let face = Face::ALL[(binding.data >> 32) as usize];
    let mut args = [0; 8];
    let args = &mut args[..usize::from(binding.conv.argc())];
    for (i, word) in args.iter_mut().enumerate() {
        *word = env.arg(i as u32);
    }
    let method = signature::com_signatures()[binding.data as u32 as usize].name;
    if matches!(
        method,
        "EnumDevices" | "EnumZBufferFormats" | "EnumTextureFormats"
    ) {
        return super::report::enumerate(env, face, method, args);
    }
    let result = {
        let (state, store) = env.ext.get2_installed_mut::<front::State, SurfaceStore>();
        let mut memory = (state.provider.memory)(env.cpu);
        Calls {
            draw: &mut state.draw,
            objects: &mut env.sys.objects,
            store,
            memory: &mut memory,
        }
        .call(face, method, args)
    };
    match result {
        Ok(hr) => Outcome::Return(hr),
        Err(error) => error.outcome(env),
    }
}

struct Calls<'a> {
    draw: &'a mut DirectDraw,
    objects: &'a mut Objects,
    store: &'a mut SurfaceStore,
    memory: &'a mut dyn SurfaceMemory,
}

impl Calls<'_> {
    fn call(&mut self, face: Face, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let this = args[0];
        if matches!(face, Face::Device2 | Face::Device3 | Face::Device7)
            && matches!(
                method,
                "SetTransform"
                    | "GetTransform"
                    | "MultiplyTransform"
                    | "SetViewport"
                    | "GetViewport"
                    | "SetMaterial"
                    | "GetMaterial"
                    | "SetLight"
                    | "GetLight"
                    | "SetRenderState"
                    | "GetRenderState"
                    | "SetTextureStageState"
                    | "GetTextureStageState"
                    | "LightEnable"
                    | "GetLightEnable"
                    | "SetClipPlane"
                    | "GetClipPlane"
                    | "SetClipStatus"
                    | "GetClipStatus"
                    | "BeginStateBlock"
                    | "EndStateBlock"
                    | "CreateStateBlock"
                    | "CaptureStateBlock"
                    | "ApplyStateBlock"
                    | "DeleteStateBlock"
            )
        {
            return self.device_state(face, method, args);
        }
        Ok(match method {
            "QueryInterface" => {
                self.memory.arg_put_u32(args[2], 0)?;
                match super::query(self.draw, self.objects, this, iid(self.memory, args[1])?) {
                    Ok(pointer) => {
                        self.memory.arg_put_u32(args[2], pointer)?;
                        0
                    }
                    Err(hr) => hr,
                }
            }
            "AddRef" => super::add_ref(self.draw, self.objects, this),
            "Release" => {
                self.draw
                    .d3d
                    .as_mut()
                    .unwrap()
                    .flush(self.store, self.memory)?;
                super::release(self.draw, self.store, self.objects, this)
            }
            "CreateDevice" => return self.create_device(face, args),
            "GetCaps" => return self.get_caps(face, args),
            "CreateVertexBuffer" => return self.create_vertex_buffer(face, args),
            "CreateExecuteBuffer" => return self.create_execute_buffer(args),
            "Execute" => return self.execute(args),
            "Pick" => return self.run_execute(&args[..4], Some(read(self.memory, args[4])?)),
            "GetPickRecords" => return self.pick_records(args),
            "TransformVertices" => return self.transform_vertices(args),
            "LightElements" => 0x80004001, // Retired before the Direct3D 3 interface.

            "Lock" | "Unlock" | "SetExecuteData" | "GetExecuteData" | "Validate" | "Optimize"
                if face == Face::ExecuteBuffer =>
            {
                return self.execute_buffer(method, args);
            }
            "ProcessVertices" | "ProcessVerticesStrided" => {
                return self.process_vertices(method, args);
            }
            "Lock" | "Unlock" | "GetVertexBufferDesc" | "Optimize" => {
                return self.vertex_buffer(face, method, args);
            }
            "BeginScene" | "EndScene" | "Clear" | "SetRenderTarget" | "GetRenderTarget"
            | "SetTexture" | "GetTexture" | "PreLoad" | "ValidateDevice"
                if face != Face::Viewport3 =>
            {
                return self.render_method(face, method, args);
            }
            "DrawPrimitive"
            | "DrawIndexedPrimitive"
            | "DrawPrimitiveStrided"
            | "DrawIndexedPrimitiveStrided"
            | "DrawPrimitiveVB"
            | "DrawIndexedPrimitiveVB" => return self.draw_primitive(face, method, args),
            "CreateLight" | "CreateMaterial" | "CreateViewport" => {
                return self.create_child(face, method, args);
            }
            "AddViewport" | "DeleteViewport" | "AddLight" | "DeleteLight" => object::attachment(
                self.draw,
                self.objects,
                self.store,
                this,
                args[1],
                matches!(method, "AddViewport" | "AddLight"),
                matches!(method, "AddViewport" | "DeleteViewport"),
            ),
            "GetDirect3D" => {
                let Some(device) = super::device_mut(self.draw, self.objects, this) else {
                    return Ok(INVALID);
                };
                let parent = device.parent;
                let iid = match face {
                    Face::Device1 => IID_IDIRECT3D,
                    Face::Device2 => IID_IDIRECT3D2,
                    Face::Device3 => IID_IDIRECT3D3,
                    Face::Device7 => IID_IDIRECT3D7,
                    Face::Direct3D
                    | Face::Direct3D2
                    | Face::Direct3D3
                    | Face::Direct3D7
                    | Face::Material1
                    | Face::Material2
                    | Face::Material3
                    | Face::Viewport3
                    | Face::Light
                    | Face::Texture1
                    | Face::Texture2
                    | Face::VertexBuffer7
                    | Face::VertexBuffer
                    | Face::ExecuteBuffer => return Ok(INVALID),
                };
                match super::query(self.draw, self.objects, parent, iid) {
                    Ok(pointer) => {
                        self.memory.arg_put_u32(args[1], pointer)?;
                        0
                    }
                    Err(hr) => hr,
                }
            }
            "GetHandle" => return self.handle(face, args),
            "SetMaterial" | "GetMaterial" | "SetViewport" | "GetViewport" | "SetLight"
            | "GetLight" | "SetViewport2" | "GetViewport2" => {
                let Some(com) = self.draw.com else {
                    return Ok(INVALID);
                };
                let Some(value) = com.get_mut::<Object>(self.objects, this) else {
                    return Ok(INVALID);
                };
                let words: &mut [u32] = match value {
                    Object::Material(value) => value,
                    Object::Viewport { value, value2, .. } => {
                        let second = method.ends_with('2');
                        if !method.starts_with("Set") && value[0] != 0 && second != *value2 {
                            return Ok(0x88760305);
                        }
                        if method.starts_with("Set") {
                            *value2 = second;
                        }
                        value
                    }
                    Object::Light { value, .. } => {
                        let count = self.memory.arg_u32(args[1])? / 4;
                        if !matches!(count, 19 | 20) {
                            return Ok(INVALID);
                        }
                        &mut value[..count as usize]
                    }
                    Object::Device(_) | Object::VertexBuffer { .. } | Object::ExecuteBuffer(_) => {
                        return Ok(INVALID);
                    }
                };
                if self.memory.arg_u32(args[1])? != words.len() as u32 * 4 {
                    return Ok(INVALID);
                }
                if method.starts_with("Set") {
                    let bytes = self.memory.arg_read(args[1], words.len() * 4)?;
                    for (out, word) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
                        *out = u32::from_le_bytes(*word);
                    }
                } else {
                    words[0] = words.len() as u32 * 4;
                    self.memory
                        .arg_write(args[1], bytemuck::cast_slice(words))?;
                }
                0
            }
            _ => return self.legacy_method(face, method, args),
        })
    }

    fn create_device(&mut self, face: Face, args: &[u32]) -> Result<u32, CallError> {
        const INVALID_OBJECT: u32 = 0x88760082;
        let [parent, kind, target, out, ..] = *args else {
            unreachable!()
        };
        self.memory.arg_put_u32(out, 0)?;
        if face == Face::Direct3D3 && args[4] != 0 {
            return Ok(0x80040110);
        }
        let Some(com) = self.draw.com else {
            return Ok(INVALID);
        };
        if !com
            .get::<crate::ddraw::ComState>(self.objects, parent)
            .is_some_and(|object| matches!(object.kind, crate::ddraw::Kind::Device))
        {
            return Ok(INVALID);
        }
        let Some(sid) = self.draw.surface_id(self.objects, target) else {
            return Ok(INVALID_OBJECT);
        };
        let image = &self.store[sid];
        if self.draw.caps(sid)[0] & creation::THREE_D_DEVICE == 0
            || image.width > 2048
            || image.height > 2048
        {
            return Ok(INVALID_OBJECT);
        }
        let guid = iid(self.memory, kind)?;
        let kind = [caps::Device::Rgb, caps::Device::Hal, caps::Device::TnlHal]
            .into_iter()
            .find(|device| device.guid() == guid);
        let Some(kind) = kind else { return Ok(INVALID) };
        if face != Face::Direct3D7 && kind == caps::Device::TnlHal {
            return Ok(INVALID);
        }
        let faces: &[Face] = match face {
            Face::Direct3D2 => &[Face::Device2, Face::Device1],
            Face::Direct3D3 => &[Face::Device3, Face::Device2, Face::Device1],
            Face::Direct3D7 => &[Face::Device7],
            Face::Direct3D
            | Face::Device1
            | Face::Device2
            | Face::Device3
            | Face::Device7
            | Face::Material1
            | Face::Material2
            | Face::Material3
            | Face::Viewport3
            | Face::Light
            | Face::Texture1
            | Face::Texture2
            | Face::VertexBuffer7
            | Face::VertexBuffer
            | Face::ExecuteBuffer => return Ok(NO_INTERFACE),
        };
        let mut device = object::Device::new(kind, faces[0], parent, target);
        {
            let depth = self
                .draw
                .attached(sid)
                .into_iter()
                .any(|surface| self.draw.caps(surface)[0] & creation::ZBUFFER != 0);
            device.state = Some(super::state::State::new(
                image.width,
                image.height,
                depth,
                (face == Face::Direct3D7).then_some(kind),
            ));
        }
        let pointer = self.make(faces, &[], Object::Device(Box::new(device)))?;
        if pointer == OUT_OF_MEMORY {
            return Ok(pointer);
        }
        super::add_ref(self.draw, self.objects, parent);
        super::add_ref(self.draw, self.objects, target);
        self.draw.d3d.as_mut().unwrap().devices.push(pointer);
        self.memory.arg_put_u32(out, pointer)?;
        Ok(0)
    }

    fn device_state(&mut self, face: Face, method: &str, args: &[u32]) -> Result<u32, CallError> {
        let Some(device) = super::device(self.draw, self.objects, args[0]) else {
            return Ok(INVALID);
        };
        let sid = self.draw.surface_id(self.objects, device.target).unwrap();
        let target = &self.store[sid];
        let size = (target.width, target.height);
        let rgb = device.kind == caps::Device::Rgb;
        let device = super::device_mut(self.draw, self.objects, args[0]).unwrap();
        let Some(state) = device.state.as_mut() else {
            return Ok(INVALID);
        };
        let memory = &mut *self.memory;
        if face != Face::Device7
            && matches!(method, "SetRenderState" | "GetRenderState")
            && decode::render_value(&state.fixed, args[1]).is_err()
        {
            return self.legacy_render_state(method, args);
        }
        let mut references = [(0, 0); 8];
        let result = (|| -> Result<Result<(), u32>, CallError> {
            macro_rules! validated {
                ($value:expr) => {
                    match $value {
                        Ok(value) => value,
                        Err(hr) => return Ok(Err(hr)),
                    }
                };
            }
            match method {
                "SetRenderState" if face == Face::Device7 => {
                    let [slot, value] = [args[1], args[2]];
                    if decode::device7_render(slot) {
                        state.set(crate::fixed::Setting::RenderRegister(slot as i32, value));
                    } else if value != 0 || !decode::retired_zero(rgb, slot) {
                        return Ok(Err(INVALID));
                    }
                }
                "SetRenderState" => state.set(validated!(decode::render(args[1], args[2]))),
                "GetRenderState" => {
                    let value = validated!(if face == Face::Device7 {
                        decode::device7_render_value(&state.fixed, args[1])
                    } else {
                        decode::render_value(&state.fixed, args[1])
                    });
                    memory.arg_put_u32(args[2], value)?;
                }
                "SetTextureStageState" => {
                    for setting in validated!(decode::stage(args[1] as usize, args[2], args[3]))
                        .into_iter()
                        .flatten()
                    {
                        state.set(setting);
                    }
                }
                "GetTextureStageState" => {
                    let value =
                        validated!(decode::stage_value(&state.fixed, args[1] as usize, args[2]));
                    memory.arg_put_u32(args[3], value)?;
                }
                "SetTransform" | "MultiplyTransform" => {
                    let slot = validated!(decode::transform(args[1]));
                    let matrix = read(memory, args[2])?;
                    state.set(if method == "SetTransform" {
                        Setting::Transform(slot, matrix)
                    } else {
                        Setting::MultiplyTransform(slot, matrix)
                    });
                }
                "GetTransform" => {
                    let slot = validated!(decode::transform(args[1]));
                    write(memory, args[2], state.fixed.transform(slot as usize))?;
                }
                "SetViewport" => {
                    let viewport = validated!(decode::viewport(read(memory, args[1])?, size));
                    state.set(Setting::Viewport(viewport));
                }
                "GetViewport" => write(
                    memory,
                    args[1],
                    &decode::viewport_desc(state.fixed.viewport),
                )?,
                "SetMaterial" => state.set(Setting::Material(crate::fixed::material(read(
                    memory, args[1],
                )?))),
                "GetMaterial" => {
                    let material = state.fixed.material;
                    let mut value = [material.power; 17];
                    value[..16].copy_from_slice(material.colors.as_flattened());
                    write(memory, args[1], &value)?;
                }
                "SetLight" => state.set(Setting::Light(
                    args[1],
                    validated!(decode::light(read(memory, args[2])?)),
                )),
                "GetLight" => {
                    let Some(light) = state.fixed.lights.get(&args[1]) else {
                        return Ok(Err(INVALID));
                    };
                    write(memory, args[2], &light.value.words())?;
                }
                "LightEnable" => state.set(Setting::LightEnable(args[1], args[2] != 0)),
                "GetLightEnable" => {
                    let Some(light) = state.fixed.lights.get(&args[1]) else {
                        return Ok(Err(INVALID));
                    };
                    memory.arg_put_u32(args[2], u32::from(light.enabled))?;
                }
                "SetClipPlane" | "GetClipPlane" => {
                    let n = args[1] as usize;
                    if n >= state.fixed.clip_planes.len() {
                        return Ok(Err(INVALID));
                    }
                    if method == "SetClipPlane" {
                        state.set(Setting::ClipPlane(n, read(memory, args[2])?));
                    } else {
                        write(memory, args[2], &state.fixed.clip_planes[n])?;
                    }
                }
                "SetClipStatus" if face == Face::Device7 => {
                    let [flags, status, min_x, max_x, min_y, max_y, _, _] = read(memory, args[1])?;
                    if flags & 4 != 0 {
                        return Ok(Err(INVALID));
                    }
                    if flags & 1 != 0 {
                        state.clip_status[1] = status;
                    }
                    if flags & 2 != 0 {
                        state.clip_status[2..6].copy_from_slice(&[min_x, max_x, min_y, max_y]);
                    }
                }
                "GetClipStatus" if face == Face::Device7 => {
                    memory.arg_write(args[1], bytemuck::cast_slice(&state.clip_status[..6]))?;
                }
                "SetClipStatus" => state.clip_status = read(memory, args[1])?,
                "GetClipStatus" => write(memory, args[1], &state.clip_status)?,
                "BeginStateBlock" => validated!(state.begin()),
                "EndStateBlock" => {
                    let handle = validated!(state.end());
                    memory.arg_put_u32(args[1], handle)?;
                }
                "CreateStateBlock" => {
                    let (handle, changes) = validated!(state.create(args[1]));
                    references = changes;
                    memory.arg_put_u32(args[2], handle)?;
                }
                "CaptureStateBlock" => references = validated!(state.capture(args[1])),
                "ApplyStateBlock" => references = validated!(state.apply(args[1])),
                "DeleteStateBlock" => references = validated!(state.delete(args[1])),
                _ => unreachable!(),
            }
            Ok(Ok(()))
        })();
        let changed = references
            .into_iter()
            .filter(|(previous, next)| previous != next);
        for (_, next) in changed.clone() {
            if next != 0 {
                super::add_ref(self.draw, self.objects, next);
            }
        }
        for (previous, _) in changed {
            if previous != 0 {
                self.draw.release_com(self.objects, self.store, previous);
            }
        }
        Ok(result?.err().unwrap_or(0))
    }

    fn make(&mut self, faces: &[Face], aliases: &[Iid], value: Object) -> Result<u32, CallError> {
        let tables = &self.draw.d3d.as_ref().unwrap().tables;
        let faces: Vec<_> = faces.iter().map(|&face| (face, tables[&face])).collect();
        object::create(
            self.draw.com.unwrap(),
            self.objects,
            self.store,
            self.memory,
            &faces,
            aliases,
            value,
        )
    }

    fn create_child(&mut self, face: Face, method: &str, args: &[u32]) -> Result<u32, CallError> {
        self.memory.arg_put_u32(args[1], 0)?;
        if args[2] != 0 {
            return Ok(0x80040110);
        }
        let Some(com) = self.draw.com else {
            return Ok(INVALID);
        };
        if !com
            .get::<crate::ddraw::ComState>(self.objects, args[0])
            .is_some_and(|object| matches!(object.kind, crate::ddraw::Kind::Device))
        {
            return Ok(INVALID);
        }
        let pointer = match method {
            "CreateLight" => self.make(
                &[Face::Light],
                &[],
                Object::Light {
                    viewport: None,
                    value: [0; 20],
                },
            ),
            "CreateViewport" => self.make(
                &[Face::Viewport3],
                &[IID_IDIRECT3DVIEWPORT, IID_IDIRECT3DVIEWPORT2],
                Object::Viewport {
                    device: None,
                    lights: Vec::new(),
                    value: [0; 11],
                    value2: false,
                    background: 0,
                    depth: 0,
                },
            ),
            "CreateMaterial" => {
                let faces = match face {
                    Face::Direct3D => [Face::Material1, Face::Material2, Face::Material3],
                    Face::Direct3D2 => [Face::Material2, Face::Material1, Face::Material3],
                    Face::Direct3D3 => [Face::Material3, Face::Material1, Face::Material2],
                    Face::Direct3D7
                    | Face::Device1
                    | Face::Device2
                    | Face::Device3
                    | Face::Device7
                    | Face::Material1
                    | Face::Material2
                    | Face::Material3
                    | Face::Viewport3
                    | Face::Light
                    | Face::Texture1
                    | Face::Texture2
                    | Face::VertexBuffer7
                    | Face::VertexBuffer
                    | Face::ExecuteBuffer => return Ok(INVALID),
                };
                self.make(&faces, &[], Object::Material([0; 20]))
            }
            _ => unreachable!(),
        }?;
        if pointer == OUT_OF_MEMORY {
            return Ok(pointer);
        }
        self.memory.arg_put_u32(args[1], pointer)?;
        Ok(0)
    }

    fn handle(&mut self, face: Face, args: &[u32]) -> Result<u32, CallError> {
        let this = args[0];
        let Some(com) = self.draw.com else {
            return Ok(INVALID);
        };
        let material = matches!(face, Face::Material1 | Face::Material2 | Face::Material3);
        let pointer = if material {
            com.identity::<Object>(self.objects, this)
                .and_then(|id| com.canonical(self.objects, id))
        } else {
            com.identity::<crate::ddraw::ComState>(self.objects, this)
                .and_then(|id| com.canonical(self.objects, id))
        };
        let Some(pointer) = pointer else {
            return Ok(INVALID);
        };
        let Some(device) = super::device_mut(self.draw, self.objects, args[1]) else {
            return Ok(INVALID);
        };
        let handle = if material {
            device.material_handle(pointer, self.store, self.memory)?
        } else {
            device.texture_handle(pointer)
        };
        self.memory.arg_put_u32(args[2], handle)?;
        Ok(0)
    }
}

fn iid(memory: &dyn SurfaceMemory, pointer: u32) -> Result<Iid, CallError> {
    Ok(memory.arg_read(pointer, 16)?.as_ref().try_into().unwrap())
}

fn read<T: bytemuck::Pod>(memory: &dyn SurfaceMemory, pointer: u32) -> Result<T, CallError> {
    Ok(bytemuck::pod_read_unaligned(
        &memory.arg_read(pointer, size_of::<T>())?,
    ))
}

fn write<T: bytemuck::Pod>(
    memory: &mut dyn SurfaceMemory,
    pointer: u32,
    value: &T,
) -> Result<(), CallError> {
    memory.arg_write(pointer, bytemuck::bytes_of(value))
}
