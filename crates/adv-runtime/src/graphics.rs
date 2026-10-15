//! Thin assembly of personality fronts over one neutral device SurfaceStore.
use adv_abi::Registry;
use adv_cpu::backend::{Backend, Guest, Prot};
use adv_device::surface::{MemoryOperations, MemoryView, SurfaceStore};
use adv_machine::{Env, Ext, Machine, Objects, Outcome};
use adv_win32::Win32;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub use adv_device::surface::{
    FrameConsumer, FrameProducer, FrameView, GraphicsStats, PendingFrame, Presenter, Snapshot,
    SurfaceMemory, Surfaces, VideoCursorRegion, frame_mailbox,
};
pub use adv_dx::ddraw::{IID_DDRAW7, IID_SURFACE7};
/// Compatibility name; ownership is the device store, with no API frontend payload.
pub type Graphics<P = Snapshot> = Surfaces<P>;
pub mod api {
    pub use adv_dx::ddraw::api::*;
    pub use adv_win32::gdi32::raster::api::*;
}
/// One byte-access adapter, borrowed on the stack by both personality fronts.
pub(crate) fn guest_memory<'a>(cpu: &'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, addr, len| {
                if let Some(bytes) = cpu.shared_bytes(addr, len) {
                    return Ok(Cow::Borrowed(bytes));
                }
                let mut bytes = vec![0; len];
                cpu.read(addr, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, addr, bytes| cpu.write(addr, bytes).map_err(|e| e.to_string()),
            writable: |cpu, addr, len| {
                let len32 =
                    u32::try_from(len).map_err(|_| "surface memory length overflow".to_owned())?;
                if cpu.accessible(addr, len32, Prot::WRITE) {
                    Ok(())
                } else {
                    Err(format!(
                        "surface memory is not writable at {addr:#x} (+{len})"
                    ))
                }
            },
            copy: |cpu, src, dst, len| cpu.copy_memory(src, dst, len).map_err(|e| e.to_string()),
        },
    )
}
fn window_geometry(
    ext: &Ext,
    objects: &Objects,
    hwnd: u32,
) -> Option<adv_dx::ddraw::WindowGeometry> {
    let win32 = ext.get::<Win32>();
    let window = win32.user.windows.get(objects, &hwnd)?;
    let (width, height) = window.client_size();
    (width > 0 && height > 0).then(|| adv_dx::ddraw::WindowGeometry {
        width: width as u32,
        height: height as u32,
        origin: win32.user.client_origin(objects, hwnd),
    })
}
fn video_window_geometry(
    ext: &Ext,
    objects: &Objects,
    hwnd: u32,
) -> Option<adv_dx::dshow::VideoWindowGeometry> {
    let user = &ext.get::<Win32>().user;
    let window = user.windows.get(objects, &hwnd)?;
    if window.destroying {
        return None;
    }
    let (width, height) = window.client_size();
    let mut visible = width > 0 && height > 0;
    let mut ancestor = hwnd;
    let mut root = hwnd;
    let mut valid_chain = false;
    for _ in 0..=user.windows.len() {
        let parent = user.windows.get(objects, &ancestor)?;
        if parent.destroying {
            return None;
        }
        visible &= parent.visible() && parent.style & 0x2000_0000 == 0;
        root = ancestor;
        if parent.parent == 0 {
            valid_chain = true;
            break;
        }
        ancestor = parent.parent;
    }
    if !valid_chain {
        return None;
    }
    // Use the same displayed top-level canvas as host input coordinates.
    let top = user
        .top
        .iter()
        .copied()
        .find(|id| {
            user.windows
                .get(objects, id)
                .is_some_and(|window| window.visible() && !window.destroying)
        })
        .unwrap_or(root);
    let origin = user.client_origin(objects, hwnd);
    let canvas_origin = user.client_origin(objects, top);
    visible &= root == top;
    // USER coordinates describe this top-level client, while a D3D backbuffer
    // may have a different size. The neutral compositor maps the whole layer.
    let canvas = user.windows.get(objects, &top)?.client_size();
    Some(adv_dx::dshow::VideoWindowGeometry {
        width: width as u32,
        height: height as u32,
        origin: (
            origin.0.saturating_sub(canvas_origin.0),
            origin.1.saturating_sub(canvas_origin.1),
        ),
        canvas: (canvas.0 as u32, canvas.1 as u32),
        visible,
    })
}
pub fn register_graphics(registry: &mut Registry) {
    adv_win32::gdi32::raster::register(registry);
    adv_dx::ddraw::register(registry);
}
pub fn install_graphics<B: Backend>(
    machine: &mut Machine<B>,
    width: u32,
    height: u32,
) -> Result<BTreeMap<u32, u32>, String> {
    adv_device::surface::size(width, height)?;
    if machine.ext.try_get::<SurfaceStore>().is_some() {
        return Err("the process surface store is already installed".into());
    }
    if machine.ext.try_get::<Win32>().is_none()
        || machine
            .ext
            .try_get::<adv_win32::gdi32::raster::front::State>()
            .is_none()
    {
        return Err("Win32 and its GDI frontend must be installed before graphics".into());
    }
    let arena_len = 64 * 1024 * 1024;
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, arena_len, Prot::RW)
        .map_err(|e| e.to_string())?;
    let snapshot = Snapshot {
        width,
        height,
        bgra: vec![0; width as usize * height as usize * 4],
        frames: 0,
        ..Default::default()
    };
    machine.ext.insert(SurfaceStore::new(
        width, height, arena, arena_len, snapshot,
    )?);
    machine.ext.insert(RunControl::default());
    adv_dx::dshow::set_video_provider(
        &mut machine.ext,
        adv_dx::dshow::VideoProvider {
            update: |ext, cpu, key, layer| {
                ext.get_mut::<SurfaceStore>()
                    .set_video_layer(key, layer, &guest_memory(cpu))
            },
            window: video_window_geometry,
            completed: frame_completed,
        },
    );
    adv_win32::gdi32::raster::set_provider(
        &mut machine.ext,
        adv_win32::gdi32::raster::DrawingProvider {
            memory: guest_memory,
            completed: frame_completed,
        },
    );
    let gdi = machine.ext.get::<Win32>().gdi.clone();
    let mut entries = adv_dx::ddraw::install(
        machine,
        gdi,
        adv_dx::ddraw::Provider {
            memory: guest_memory,
            window: window_geometry,
            completed: frame_completed,
        },
    )?;
    for &(id, api) in adv_win32::gdi32::raster::front::BINDINGS {
        entries.insert(
            id,
            machine
                .sys
                .thunks
                .publish(&mut machine.cpu, api.module, api.name)
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(entries)
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

fn frame_completed(env: &mut Env<'_>, serial: u64) -> Option<Outcome> {
    env.sys.frame = serial as u32;
    env.sys.complete_frame();
    let control = env.ext.get_mut::<RunControl>();
    let now = Instant::now();
    control.first_frame.get_or_insert(now);
    if control.frame_timing
        && let Some(last) = control.last_frame
    {
        control.frame_intervals.push(now.duration_since(last));
    }
    control.last_frame = Some(now);
    control
        .frame_limit
        .filter(|&limit| serial >= limit)
        .map(|_| Outcome::ExitProcess(0))
}
#[cfg(feature = "gpu")]
pub(crate) fn d3d9_provider() -> adv_dx::d3d9::SurfaceProvider {
    adv_dx::d3d9::SurfaceProvider {
        acquire_dc: adv_win32::gdi32::raster::front::surface_dc::acquire,
        writeback_dc: adv_win32::gdi32::raster::front::surface_dc::writeback,
        release_dc: adv_win32::gdi32::raster::front::surface_dc::release,
        mode_changed: |env, device, parameters| {
            if parameters.Windowed == 0 {
                adv_win32::user32::display::fullscreen(
                    env,
                    device,
                    parameters.hDeviceWindow,
                    parameters.BackBufferWidth,
                    parameters.BackBufferHeight,
                )
            } else {
                adv_win32::user32::display::windowed(env, device)
            }
        },
        device_released: adv_win32::user32::display::release,
        display: |ext| {
            let (width, height) = ext.get::<Win32>().user.screen;
            (width.max(1) as u32, height.max(1) as u32, 60)
        },
        insert: |ext, target| ext.get_mut::<Graphics<Snapshot>>().insert_native(target),
        target: |ext, id| ext.get::<Graphics<Snapshot>>().native_target(id).cloned(),
        remove: |ext, _, id| {
            if ext.get_mut::<Graphics<Snapshot>>().delete(id) {
                Ok(())
            } else {
                Err("D3D9 surface identity no longer exists".into())
            }
        },
        replace: |ext, id, target| {
            ext.get_mut::<Graphics<Snapshot>>()
                .replace_native(id, target)
                .map(|_| ())
        },
        present: |ext, id, gpu| {
            ext.get_mut::<Graphics<Snapshot>>()
                .present_native_rotating(id, gpu)
        },
        present_copy: |ext, id, gpu| {
            use adv_device::gpu::Error;
            let graphics = ext.get_mut::<Graphics<Snapshot>>();
            let target = graphics
                .native_target(id)
                .ok_or_else(|| Error::Invalid("COPY front surface no longer exists".into()))?;
            let frame = gpu.present(&target.image, 0)?;
            graphics
                .publish_native_frame(id, frame)
                .map_err(Error::Invalid)
        },
        repeat: |ext, id| ext.get_mut::<Graphics<Snapshot>>().repeat_native_frame(id),
        presentation_serial: |ext| ext.get::<Graphics<Snapshot>>().output_serial(),
        window: |ext, objects, hwnd| {
            let window = ext.get::<Win32>().user.windows.get(objects, &hwnd)?;
            let (width, height) = window.client_size();
            (width > 0 && height > 0).then_some((width as u32, height as u32))
        },
        visible: |ext, objects, hwnd| {
            ext.get::<Win32>()
                .user
                .windows
                .get(objects, &hwnd)
                .map(|w| w.visible())
        },
        fast_forward: |ext| {
            let user = &ext.get::<Win32>().user;
            [0x11, 0xa2, 0xa3].into_iter().any(|key| {
                user.host_keys
                    .as_ref()
                    .map_or(user.keys[key], |keys| keys.get(key as u32))
                    & 0x80
                    != 0
            })
        },
        completed: frame_completed,
    }
}

#[cfg(test)]
pub(crate) fn test_machine() -> Machine<adv_cpu::JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    register_graphics(&mut registry);
    let cpu = adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, adv_machine::Clock::virtual_time()).unwrap();
    let mut config =
        adv_win32::Win32Config::new(".", "graphics-fixture.exe", "target/graphics-fixture");
    config.screen = (4, 2);
    Win32::install(&mut machine, config).unwrap();
    install_graphics(&mut machine, 4, 2).unwrap();
    machine
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, GuestExt};
    use adv_machine::RunResult;
    use iced_x86::code_asm::*;

    #[cfg(feature = "gpu")]
    #[test]
    fn d3d9_present_boost_observes_live_physical_control_and_release() {
        let mut machine = test_machine();
        let provider = d3d9_provider();
        assert!(!(provider.fast_forward)(&machine.ext));
        machine.ext.get_mut::<Win32>().user.keys[0x11] = 0x80;
        assert!(
            (provider.fast_forward)(&machine.ext),
            "fallback guest keys without a host observer"
        );
        let keys = std::sync::Arc::new(adv_sync::input::KeyState::default());
        machine.ext.get_mut::<Win32>().user.host_keys = Some(keys.clone());
        assert!(
            !(provider.fast_forward)(&machine.ext),
            "physical release overrides stale guest message state"
        );
        for key in [0x11, 0xa2, 0xa3] {
            keys.set(key, true);
            assert!((provider.fast_forward)(&machine.ext));
            keys.set(key, false);
            assert!(!(provider.fast_forward)(&machine.ext));
        }
    }

    fn graphics(machine: &mut Machine<JitCpu>, id: u32, args: &[u32]) -> u32 {
        let api = adv_win32::gdi32::raster::front::BINDINGS
            .iter()
            .chain(adv_dx::ddraw::front::BINDINGS)
            .find(|(candidate, _)| *candidate == id)
            .unwrap()
            .1;
        invoke(machine, api.module, api.name, args)
    }
    fn invoke(machine: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> u32 {
        use adv_cpu::backend::Reg;
        if machine.sys.contexts.is_empty() {
            // Host-only ABI calls still need a TEB for LastError. Finish this
            // owner before any later fixture thread is run by the scheduler.
            let code = machine
                .sys
                .vm
                .alloc(&mut machine.cpu, 4096, Prot::RX)
                .unwrap();
            machine.cpu.write(code, &[0x31, 0xc0, 0xc3]).unwrap();
            machine.spawn("ABI test owner", code, &[], 65536).unwrap();
            assert!(matches!(machine.run(), RunResult::Exited(0)));
        }
        let address = machine.resolve_import(module, name).unwrap();
        let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        let stack = machine
            .sys
            .vm
            .alloc(&mut machine.cpu, 128, Prot::RW)
            .unwrap();
        machine.cpu.set_reg(Reg::Esp, stack);
        for (index, arg) in args.iter().enumerate() {
            machine
                .cpu
                .write_u32(stack + 4 + index as u32 * 4, *arg)
                .unwrap();
        }
        let result = handler(&mut Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: index,
        });
        machine.sys.vm.free(&mut machine.cpu, stack).unwrap();
        let Outcome::Return(value) = result else {
            panic!("graphics must return; {module}!{name}")
        };
        value
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn d3d9_provider_lends_a_real_gdi_canvas_without_presenting() {
        let mut m = test_machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        let original: Vec<u8> = (0..32).collect();
        m.cpu.write(data, &original).unwrap();
        let provider = d3d9_provider();
        let dc = (provider.acquire_dc)(
            &mut Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            },
            4,
            2,
            data,
            16,
        )
        .unwrap();
        assert!(matches!(
            m.ext.get::<Win32>().gdi.get(&m.sys.objects, dc),
            Some(adv_os::gdi::GdiObj::Dc { raster: true, .. })
        ));
        assert_eq!(
            invoke(&mut m, "gdi32.dll", "SetPixel", &[dc, 2, 1, 0x00554433]),
            0x00554433
        );
        assert_eq!(graphics(&mut m, api::DELETE_DC, &[dc]), 0);
        (provider.writeback_dc)(
            &mut Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            },
            dc,
            data + 64,
            16,
        )
        .unwrap();
        let mut actual = [0; 32];
        m.cpu.read(data + 64, &mut actual).unwrap();
        assert_eq!(&actual[..24], &original[..24]);
        assert_eq!(&actual[24..27], &[0x55, 0x44, 0x33]);
        assert_eq!(&actual[28..], &original[28..]);
        (provider.release_dc)(
            &mut Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            },
            dc,
        );
        assert_eq!(m.ext.get::<SurfaceStore>().live_surfaces(), 0);
        assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 0);
        assert!(m.ext.get::<Win32>().gdi.get(&m.sys.objects, dc).is_none());
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn gdi_directdraw_and_d3d9_share_surface_identity_and_dc_ownership() {
        // Initialization only: device futures wake this owner without polling
        // intervals. Guest execution and presentation never run this executor.
        fn initialize<T>(future: impl std::future::Future<Output = T>) -> T {
            use std::task::{Context, Poll, Wake, Waker};
            struct Owner(std::thread::Thread);
            impl Wake for Owner {
                fn wake(self: std::sync::Arc<Self>) {
                    self.0.unpark();
                }
                fn wake_by_ref(self: &std::sync::Arc<Self>) {
                    self.0.unpark();
                }
            }
            let waker = Waker::from(std::sync::Arc::new(Owner(std::thread::current())));
            let mut context = Context::from_waker(&waker);
            let mut future = std::pin::pin!(future);
            loop {
                match future.as_mut().poll(&mut context) {
                    Poll::Ready(result) => return result,
                    Poll::Pending => std::thread::park(),
                }
            }
        }
        let mut m = test_machine();
        let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
        let mut header = [0; 40];
        for (offset, value) in [(0, 40u32), (4, 4), (8, 2), (12, 0x200001)] {
            header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 64, &header).unwrap();
        let bitmap = graphics(&mut m, api::CREATE_DIB, &[0, data + 64, 0, data, 0, 0]);
        assert_ne!(bitmap, 0);
        let memory_dc = graphics(&mut m, api::CREATE_DC, &[0]);
        assert_eq!(
            graphics(&mut m, api::SELECT, &[memory_dc, bitmap]),
            adv_os::gdi::STOCK_BITMAP
        );

        assert_eq!(graphics(&mut m, api::DD_CREATE_OLD, &[0, data, 0]), 0);
        let device = m.cpu.read_u32(data).unwrap();
        let mut description = [0u8; 108];
        for (offset, value) in [(0, 108u32), (4, 7), (8, 2), (12, 4), (104, 0x40)] {
            description[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        m.cpu.write(data + 64, &description).unwrap();
        assert_eq!(
            graphics(&mut m, api::CREATE_SURFACE, &[device, data + 64, data, 0]),
            0
        );
        let surface = m.cpu.read_u32(data).unwrap();
        assert_eq!(graphics(&mut m, api::SURFACE_DC, &[surface, data]), 0);
        let surface_dc = m.cpu.read_u32(data).unwrap();
        let gdi = &m.ext.get::<Win32>().gdi;
        let Some(adv_os::gdi::GdiObj::Dc { selected, .. }) = gdi.get(&m.sys.objects, surface_dc)
        else {
            panic!("DirectDraw must lend the same public GDI payload")
        };
        let dd_surface = selected[3];
        assert_ne!(bitmap, dd_surface);
        assert_eq!(
            invoke(&mut m, "gdi32.dll", "SetPolyFillMode", &[surface_dc, 2]),
            1
        );
        assert_eq!(
            invoke(&mut m, "gdi32.dll", "GetPolyFillMode", &[surface_dc]),
            2
        );
        assert_eq!(
            graphics(&mut m, api::SELECT, &[surface_dc, 0x0190_0021]),
            0x0190_001d
        );

        let gpu = initialize(adv_device::gpu::Gpu::request(false)).unwrap();
        let target = gpu.target(4, 2, false).unwrap();
        let provider = d3d9_provider();
        let native = (provider.insert)(&mut m.ext, target).unwrap();
        assert_ne!(native, bitmap);
        assert_ne!(native, dd_surface);
        let store = m.ext.get::<SurfaceStore>();
        assert_eq!(store.live_surfaces(), 3);
        for id in [bitmap, dd_surface, native] {
            assert!(store.contains(id));
        }
        assert!((provider.target)(&m.ext, native).is_some());
        assert_eq!(store.get(native).unwrap().pixels, 0);
        assert!(
            m.ext
                .get_mut::<SurfaceStore>()
                .fill_raw(
                    native,
                    adv_device::surface::Rect {
                        x: 0,
                        y: 0,
                        w: 4,
                        h: 2
                    },
                    0,
                    &mut guest_memory(&mut m.cpu)
                )
                .is_err(),
            "native pixels require the GPU command path"
        );
        assert_eq!(
            gpu.stats().readbacks,
            0,
            "sharing identities must not read native pixels"
        );
        assert!(
            m.sys.objects.close(surface_dc).is_none(),
            "kernel close cannot retire a GDI alias"
        );
        assert_ne!(
            graphics(&mut m, api::SURFACE_RELEASE_DC, &[surface, memory_dc]),
            0
        );
        assert!(matches!(
            m.ext.get::<Win32>().gdi.get(&m.sys.objects, surface_dc),
            Some(adv_os::gdi::GdiObj::Dc { .. })
        ));
        assert_eq!(
            graphics(&mut m, api::SURFACE_RELEASE_DC, &[surface, surface_dc]),
            0
        );
        assert!(
            m.ext
                .get::<Win32>()
                .gdi
                .get(&m.sys.objects, surface_dc)
                .is_none()
        );
        assert_eq!(
            invoke(&mut m, "gdi32.dll", "GetPolyFillMode", &[surface_dc]),
            0
        );
        assert_eq!(graphics(&mut m, api::DELETE_DC, &[memory_dc]), 1);
        assert_eq!(graphics(&mut m, api::DELETE_OBJECT, &[bitmap]), 1);
        assert_eq!(graphics(&mut m, api::RELEASE, &[surface]), 0);
        assert_eq!(graphics(&mut m, api::RELEASE, &[device]), 0);
        (provider.remove)(&mut m.ext, &mut m.cpu, native).unwrap();
        assert_eq!(m.ext.get::<SurfaceStore>().live_surfaces(), 0);
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
            .get_mut::<adv_dx::ddraw::front::State>()
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
            .configure_window(6, 3, &mut guest_memory(&mut m.cpu))
            .unwrap();
        m.cpu.write_u32(data + 16, 108).unwrap();
        assert_eq!(graphics(&mut m, api::GET_DESC, &[primary, data + 16]), 0);
        assert_eq!(m.cpu.read_u32(data + 28).unwrap(), 6);
        assert_eq!(m.cpu.read_u32(data + 24).unwrap(), 3);
        assert_eq!(graphics(&mut m, api::SURFACE_DC, &[primary, data]), 0);
        let dc = m.cpu.read_u32(data).unwrap();
        assert_ne!(graphics(&mut m, api::SURFACE_RELEASE_DC, &[source, dc]), 0);
        assert!(
            matches!(
                m.ext.get::<Win32>().gdi.get(&m.sys.objects, dc),
                Some(adv_os::gdi::GdiObj::Dc { .. })
            ),
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
