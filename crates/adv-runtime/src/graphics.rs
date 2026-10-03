//! Assembly of the graphics fronts: GDI raster calls (adv-win32) and DirectDraw (adv-dx)
//! over one surface store (adv-device) and one device-context table (adv-os).
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use std::time::{Duration, Instant};

use adv_abi::Registry;
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_dx::ddraw::DirectDraw;
use adv_machine::{Conv, Env, Machine, Outcome};
use adv_os::gdi::{DeviceContexts, WINDOW_DC};
use adv_win32::Win32;
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
        objects: &mut adv_machine::Objects,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        if id < 200 {
            self.raster
                .call(id, args, &mut self.surfaces, &mut self.dcs, objects, memory)
        } else {
            self.ddraw
                .call(id, args, &mut self.surfaces, &mut self.dcs, objects, memory)
        }
    }

    pub(crate) fn owns_dc(&self, objects: &adv_machine::Objects, dc: u32) -> bool {
        dc == WINDOW_DC || self.dcs.contains(objects, dc)
    }

    pub(crate) fn dc_bitmap(&self, objects: &adv_machine::Objects, dc: u32) -> Option<u32> {
        if dc == WINDOW_DC {
            self.surfaces.primary_id()
        } else {
            self.dcs.get(objects, dc)
        }
    }

    pub(crate) fn owns_bitmap(&self, object: u32) -> bool {
        self.surfaces.contains(object)
    }

    pub(crate) fn configure_clipper(&mut self, clipper: u32, origin: (i32, i32)) {
        self.ddraw.configure_clipper(clipper, origin);
    }
}

#[derive(Default)]
/// Frame accounting of a run, updated by every presentation.
pub(crate) struct RunControl {
    pub(crate) frame_limit: Option<u64>,
    pub(crate) first_frame: Option<Instant>,
    pub(crate) last_frame: Option<Instant>,
    pub(crate) frame_timing: bool,
    pub(crate) frame_intervals: Vec<Duration>,
}

/// Graphics registrations replace the Win32 state-only drawing handlers.
pub fn register_graphics(registry: &mut Registry) {
    registry.add(
        "user32.dll",
        "BeginPaint",
        Conv::Stdcall { argc: 2 },
        begin_paint,
    );
    for (interface, methods) in [
        ("IDirectDraw7", DEVICE_METHODS),
        ("IDirectDrawSurface7", SURFACE_METHODS),
    ] {
        for method in methods {
            let signature = adv_abi::signature::lookup_com(interface, method)
                .expect("DirectDraw vtable slot is in generated Win32Metadata");
            registry.add(
                "ddraw.dll",
                &format!("{interface}::{method}"),
                signature
                    .conv
                    .expect("DirectDraw method has a supported x86 convention"),
                unsupported_graphics,
            );
        }
    }
    for &(id, module, name, argc) in api_specs() {
        // GetLastError follows each context's TEB, including file and heap calls.
        if id != api::LAST_ERROR {
            assert!(
                usize::from(argc) <= MAX_GRAPHICS_ARGS,
                "graphics ABI exceeds argument buffer"
            );
            registry.add_with_data(
                module,
                name,
                Conv::Stdcall { argc },
                graphics_call,
                u64::from(id) | (u64::from(argc) << 32),
            );
        }
    }
}

/// Allocate shared surfaces and bind their vtables to the registry's actual thunks.
pub fn install_graphics<B: Backend>(
    machine: &mut Machine<B>,
    width: u32,
    height: u32,
) -> Result<BTreeMap<u32, u32>, String> {
    machine.ext.insert(RunControl::default());
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err("invalid surface dimensions".into());
    }
    let arena_len = 64 * 1024 * 1024;
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, arena_len, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut entries = BTreeMap::new();
    for (base, interface, methods) in [
        (400, "IDirectDraw7", DEVICE_METHODS),
        (500, "IDirectDrawSurface7", SURFACE_METHODS),
    ] {
        for (slot, method) in methods.iter().enumerate() {
            entries.insert(
                base + slot as u32,
                machine
                    .resolve_import("ddraw.dll", &format!("{interface}::{method}"))
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    for &(id, module, name, _) in api_specs() {
        if id != api::LAST_ERROR {
            entries.insert(
                id,
                machine
                    .resolve_import(module, name)
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    let snapshot = Snapshot {
        width,
        height,
        bgra: vec![0; width as usize * height as usize * 4],
        frames: 0,
    };
    let mut graphics = Graphics::new(width, height, arena, arena_len, entries.clone(), snapshot)?;
    if let Some(win32) = machine.ext.try_get::<Win32>() {
        graphics.dcs = DeviceContexts::new(win32.gdi.clone());
    }
    machine.ext.insert(graphics);
    Ok(entries)
}

const MAX_GRAPHICS_ARGS: usize = 16;

pub(crate) fn graphics_call(env: &mut Env<'_>) -> Outcome {
    let binding = env.sys.thunks.get(env.thunk).f.data;
    let id = binding as u32;
    let argc = (binding >> 32) as usize;
    if argc > MAX_GRAPHICS_ARGS {
        return Outcome::Fail("invalid graphics ABI binding".into());
    }
    let mut words = [0; MAX_GRAPHICS_ARGS];
    for (index, word) in words[..argc].iter_mut().enumerate() {
        *word = match env.cpu.arg(index as u32) {
            Ok(value) => value,
            Err(error) => return Outcome::Fail(format!("graphics argument {index}: {error}")),
        };
    }
    let args = &words[..argc];
    let hwnd = match id {
        api::GET_DC => Some(args[0]),
        api::COOPERATIVE if args[2] == 8 => Some(args[1]),
        api::CLIPPER_SET_HWND => Some(args[2]),
        _ => None,
    };
    let geometry = hwnd
        .and_then(|hwnd| {
            env.ext.try_get_mut::<Win32>().and_then(|win32| {
                win32.user.windows.get(&env.sys.objects, &hwnd).map(|window| {
                    let (w, h) = window.client_size();
                    (w, h, win32.user.client_origin(&env.sys.objects, hwnd))
                })
            })
        })
        .filter(|&(w, h, _)| w > 0 && h > 0);
    let graphics = env.ext.get_mut::<Graphics<Snapshot>>();
    if let Some((width, height, origin)) = geometry {
        if let Err(error) =
            graphics.configure_window(width as u32, height as u32, &mut GuestMemory(env.cpu))
        {
            return Outcome::Fail(format!("window graphics: {error}"));
        }
        if id == api::CLIPPER_SET_HWND {
            graphics.configure_clipper(args[0], origin);
        }
    }
    let before = graphics.stats.presentations;
    let out = graphics.handle(id, args, &mut env.sys.objects, &mut GuestMemory(env.cpu));
    let presented = graphics.stats.presentations;
    if let Ok(value) = &out {
        // Raster storage stays in Graphics; USER32/GDI object metadata must
        // refer to the same HDC so fonts and palettes share its real lifetime.
        let added = match id {
            api::GET_DC if *value != 0 => Some((*value, args[0])),
            api::CREATE_DC if *value != 0 => Some((*value, 0)),
            api::SURFACE_DC if *value == 0 => env.cpu.read_u32(args[1]).ok().map(|dc| (dc, 0)),
            _ => None,
        };
        let removed = match id {
            api::DELETE_DC if *value != 0 => Some(args[0]),
            api::RELEASE_DC if *value != 0 => Some(args[1]),
            api::SURFACE_RELEASE_DC if *value == 0 => Some(args[1]),
            _ => None,
        };
        let selection = added
            .map(|(dc, _)| dc)
            .or_else(|| {
                if id == api::SELECT && *value != 0 {
                    Some(args[0])
                } else {
                    None
                }
            })
            .and_then(|dc| {
                graphics
                    .dc_bitmap(&env.sys.objects, dc)
                    .map(|bitmap| (dc, bitmap))
            });
        if (added.is_some() || removed.is_some() || selection.is_some())
            && let Some(win32) = env.ext.try_get_mut::<Win32>()
        {
            if let Some((dc, hwnd)) = added
                && !win32
                    .gdi
                    .register_external_dc(&mut env.sys.objects, dc, hwnd)
            {
                return rollback_dc(env, id, args, dc);
            }
            if let Some(dc) = removed {
                win32.gdi.remove_external_dc(&mut env.sys.objects, dc);
            }
            if let Some((dc, bitmap)) = selection {
                win32
                    .gdi
                    .set_external_bitmap(&mut env.sys.objects, dc, bitmap);
            }
        }
        if let Some(dc) = removed {
            adv_win32::gdi32::font::retire_dc(env, dc);
        }
    }
    if presented != before {
        env.sys.frame = presented as u32;
        if out.is_ok() {
            env.sys.complete_frame();
        }
        let control = env.ext.get_mut::<RunControl>();
        let now = Instant::now();
        control.first_frame.get_or_insert(now);
        if control.frame_timing
            && let Some(last) = control.last_frame
        {
            control.frame_intervals.push(now.duration_since(last));
        }
        control.last_frame = Some(now);
        if out.is_ok() && control.frame_limit.is_some_and(|limit| presented >= limit) {
            return Outcome::ExitProcess(0);
        }
    }
    match out {
        Ok(value) => Outcome::Return(value),
        Err(error) => Outcome::Fail(format!("graphics: {error}")),
    }
}

fn rollback_dc(env: &mut Env<'_>, id: u32, args: &[u32], dc: u32) -> Outcome {
    let (release, words) = match id {
        api::SURFACE_DC => (api::SURFACE_RELEASE_DC, [args[0], dc]),
        api::CREATE_DC => (api::DELETE_DC, [dc, 0]),
        _ => (api::RELEASE_DC, [args[0], dc]),
    };
    if let Err(error) = env.ext.get_mut::<Graphics<Snapshot>>().handle(
        release,
        &words,
        &mut env.sys.objects,
        &mut GuestMemory(env.cpu),
    ) {
        return Outcome::Fail(format!("GDI context allocation rollback: {error}"));
    }
    if id == api::SURFACE_DC {
        let _ = env.cpu.write_u32(args[1], 0);
        Outcome::Return(0x8007000e) // E_OUTOFMEMORY
    } else {
        adv_win32::util::set_last_error(env, 8);
        Outcome::Return(0)
    }
}

pub(crate) fn unsupported_graphics(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    Outcome::Fail(format!("unimplemented: {}!{}", thunk.module, thunk.name))
}

fn begin_paint(env: &mut Env<'_>) -> Outcome {
    let (hwnd, paint) = (env.arg(0), env.arg(1));
    let Some(window) = env.ext.get::<Win32>().user.windows.get(&env.sys.objects, &hwnd)
    else {
        return Outcome::Return(0);
    };
    let (width, height) = window.client_size();
    let graphics = env.ext.get_mut::<Graphics<Snapshot>>();
    if width > 0
        && height > 0
        && let Err(error) =
            graphics.configure_window(width as u32, height as u32, &mut GuestMemory(env.cpu))
    {
        return Outcome::Fail(format!("BeginPaint resize: {error}"));
    }
    let dc = match graphics.handle(
        api::GET_DC,
        &[hwnd],
        &mut env.sys.objects,
        &mut GuestMemory(env.cpu),
    ) {
        Ok(dc) => dc,
        Err(error) => return Outcome::Fail(format!("BeginPaint: {error}")),
    };
    if !env
        .ext
        .get_mut::<Win32>()
        .gdi
        .register_external_dc(&mut env.sys.objects, dc, hwnd)
    {
        return rollback_dc(env, api::GET_DC, &[hwnd], dc);
    }
    let mut bytes = [0u8; 64];
    bytes[..4].copy_from_slice(&dc.to_le_bytes());
    bytes[16..20].copy_from_slice(&width.to_le_bytes());
    bytes[20..24].copy_from_slice(&height.to_le_bytes());
    if let Err(error) = env.cpu.write(paint, &bytes) {
        return Outcome::Fail(format!("BeginPaint: {error}"));
    }
    adv_win32::user32::message::validate(env, hwnd);
    Outcome::Return(dc)
}

#[cfg(test)]
pub(crate) fn test_machine() -> Machine<adv_cpu::JitCpu> {
    let mut registry = Registry::default();
    register_graphics(&mut registry);
    let cpu = adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, adv_machine::Clock::virtual_time()).unwrap();
    install_graphics(&mut machine, 4, 2).unwrap();
    machine
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::Guest;
    use adv_machine::RunResult;
    use iced_x86::code_asm::*;

    fn graphics(machine: &mut Machine<JitCpu>, id: u32, args: &[u32]) -> u32 {
        machine
            .ext
            .get_mut::<Graphics<Snapshot>>()
            .handle(
                id,
                args,
                &mut machine.sys.objects,
                &mut GuestMemory(&mut machine.cpu),
            )
            .unwrap()
    }

    #[test]
    fn pal_trap_presents_pixels_from_the_shared_guest_store() {
        let mut m = test_machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let mut desc = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 7), (8, 2), (12, 4), (104, 0x40)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let source = m.cpu.read_u32(data).unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(
            graphics(&mut m, api::LOCK, &[source, 0, data + 16, 0, 0]),
            0
        );
        let pixels = m.cpu.read_u32(data + 52).unwrap();
        let expected: Vec<u8> = (0..32u8).collect();
        m.cpu.write(pixels, &expected).unwrap();
        assert_eq!(graphics(&mut m, api::UNLOCK, &[source, 0]), 0);
        desc.fill(0);
        for (offset, value) in [(0, 108u32), (4, 1), (104, 0x200)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let primary = m.cpu.read_u32(data).unwrap();
        let table = m.cpu.read_u32(primary).unwrap();
        let blt = m.cpu.read_u32(table + 7 * 4).unwrap();
        let code = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        for value in [0, 0, source, 0, 0, primary] {
            assembler.push(value).unwrap();
        }
        assembler.call(u64::from(blt)).unwrap();
        assembler.ret().unwrap();
        m.cpu
            .write(code, &assembler.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("graphics", code, &[], 65536).unwrap();
        assert!(matches!(m.run(), RunResult::Exited(0)));
        let graphics = m.ext.get::<Graphics<Snapshot>>();
        assert_eq!(graphics.presenter.bgra, expected);
        assert_eq!(graphics.presenter.frames, 1);
        assert_eq!(
            m.sys.completed_frames, 1,
            "the real graphics trap did not publish its completed frame"
        );
    }

    #[test]
    fn unsupported_com_slot_fails_by_method_name() {
        let mut m = test_machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let table = m.cpu.read_u32(device).unwrap();
        let compact = m.cpu.read_u32(table + 3 * 4).unwrap();
        let code = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RX).unwrap();
        let mut assembler = CodeAssembler::new(32).unwrap();
        assembler.push(device).unwrap();
        assembler.call(u64::from(compact)).unwrap();
        assembler.ret().unwrap();
        m.cpu
            .write(code, &assembler.assemble(u64::from(code)).unwrap())
            .unwrap();
        m.spawn("missing graphics", code, &[], 65536).unwrap();
        let RunResult::Failed(error) = m.run() else {
            panic!("missing COM implementation must fail")
        };
        assert_eq!(
            error.message,
            "unimplemented: ddraw.dll!IDirectDraw7::Compact"
        );
    }

    #[test]
    fn clipper_translates_screen_rectangles_and_resize_preserves_surface_identity() {
        let mut m = test_machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let mut desc = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 7), (8, 2), (12, 4), (104, 0x40)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let source = m.cpu.read_u32(data).unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(
            graphics(&mut m, api::LOCK, &[source, 0, data + 16, 0, 0]),
            0
        );
        let pixels = m.cpu.read_u32(data + 52).unwrap();
        let expected: Vec<u8> = (0..32u8).collect();
        m.cpu.write(pixels, &expected).unwrap();
        assert_eq!(graphics(&mut m, api::UNLOCK, &[source, 0]), 0);
        desc.fill(0);
        for (offset, value) in [(0, 108u32), (4, 1), (104, 0x200)] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 16, &desc).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 16, data, 0]),
            0
        );
        let primary = m.cpu.read_u32(data).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_CLIPPER, &[device, 0, data, 0]),
            0
        );
        let clipper = m.cpu.read_u32(data).unwrap();
        assert_eq!(graphics(&mut m, api::SET_CLIPPER, &[primary, clipper]), 0);
        m.ext
            .get_mut::<Graphics<Snapshot>>()
            .configure_clipper(clipper, (100, 50));
        m.cpu
            .write(
                data + 16,
                &[99u32, 50, 103, 52]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert_eq!(
            graphics(&mut m, api::BLT, &[primary, data + 16, source, 0, 0, 0]),
            0
        );
        let frame = &m.ext.get::<Graphics<Snapshot>>().presenter;
        assert_eq!(&frame.bgra[..12], &expected[4..16]);
        assert_eq!(&frame.bgra[16..28], &expected[20..32]);
        assert_eq!(&frame.bgra[12..16], &[0; 4]);
        m.ext
            .get_mut::<Graphics<Snapshot>>()
            .configure_window(6, 3, &mut GuestMemory(&mut m.cpu))
            .unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(graphics(&mut m, api::GET_DESC, &[primary, data + 16]), 0);
        assert_eq!(m.cpu.read_u32(data + 28).unwrap(), 6);
        assert_eq!(m.cpu.read_u32(data + 24).unwrap(), 3);
        assert_eq!(graphics(&mut m, api::SURFACE_DC, &[primary, data]), 0);
        let dc = m.cpu.read_u32(data).unwrap();
        assert_ne!(graphics(&mut m, api::SURFACE_RELEASE_DC, &[source, dc]), 0);
        assert!(
            m.ext
                .get::<Graphics<Snapshot>>()
                .owns_dc(&m.sys.objects, dc),
            "a rejected ReleaseDC must retain the original surface DC"
        );
        assert_eq!(graphics(&mut m, api::SURFACE_RELEASE_DC, &[primary, dc]), 0);
        let frame = &m.ext.get::<Graphics<Snapshot>>().presenter;
        assert_eq!((frame.width, frame.height), (6, 3));
        assert_eq!(&frame.bgra[..12], &expected[4..16]);
        assert_eq!(&frame.bgra[24..36], &expected[20..32]);
        assert!(frame.bgra[48..].iter().all(|&byte| byte == 0));
    }
}
