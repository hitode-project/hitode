use super::*;
use crate::ddraw::{IID_DDRAW7, api, front};
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
#[cfg(feature = "gpu")]
use adv_device::gpu::test_support::SurfaceCapture;
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot};
use adv_machine::{Clock, Conv, Machine, Resume, RunResult, Wait, layout::PARK_ADDR};
use std::borrow::Cow;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

#[test]
// spec: GFX-DDRAW-1 GFX-DDRAW-11
fn primary_blits_refresh_window_clipper_geometry_after_moves_without_reattachment() {
    #[derive(Clone, Copy)]
    struct WindowPoint(i32, i32);
    let (mut m, device) = setup(false, false, Registry::default());
    m.ext.insert(WindowPoint(100, 50));
    m.ext.get_mut::<front::State>().provider.window = |ext, _, hwnd| {
        let point = ext.get::<WindowPoint>();
        (hwnd == 0 || hwnd == 7).then_some(front::WindowGeometry {
            hwnd: 7,
            width: if hwnd == 0 { 8 } else { 4 },
            height: if hwnd == 0 { 6 } else { 2 },
            origin: (point.0, point.1),
        })
    };
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| {
        let result = match front::call(&mut env(m), id, args) {
            Outcome::Return(hr) => hr,
            _ => panic!("synchronous authored draw"),
        };
        #[cfg(feature = "gpu")]
        {
            let (capture, surfaces) = m.ext.get2_installed_mut::<SurfaceCapture, SurfaceStore>();
            capture.update(surfaces);
        }
        result
    };
    assert_eq!(call(&mut m, api::CREATE_CLIPPER, &[device, 0, DATA, 0]), 0);
    let clipper = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(call(&mut m, api::CLIPPER_SET_HWND, &[clipper, 0, 7]), 0);
    let mut desc = [0u8; 124];
    desc[..4].copy_from_slice(&124u32.to_le_bytes());
    desc[4..8].copy_from_slice(&1u32.to_le_bytes());
    desc[104..108].copy_from_slice(&0x200u32.to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 64, DATA, 0]),
        0
    );
    let primary = m.cpu.read_u32(DATA).unwrap();
    m.cpu.write_u32(DATA + 8, 0x1234).unwrap();
    assert_eq!(
        call(&mut m, api::GET_CLIPPER, &[primary, DATA + 8]),
        0x88760238
    );
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0x1234);
    assert_eq!(call(&mut m, api::GET_CLIPPER, &[primary, 0]), 0x80070057);
    assert_eq!(call(&mut m, api::SET_CLIPPER, &[primary, clipper]), 0);
    assert_eq!(call(&mut m, api::GET_CLIPPER, &[primary, DATA + 8]), 0);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), clipper);
    assert_eq!(call(&mut m, api::RELEASE, &[clipper]), 2);
    desc[4..8].copy_from_slice(&7u32.to_le_bytes());
    desc[8..12].copy_from_slice(&2u32.to_le_bytes());
    desc[12..16].copy_from_slice(&4u32.to_le_bytes());
    desc[104..108].copy_from_slice(&0x40u32.to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 64, DATA, 0]),
        0
    );
    let source = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        call(&mut m, api::LOCK, &[source, 0, DATA + 64, 0x821, 0]),
        0
    );
    let pixels = m.cpu.read_u32(DATA + 100).unwrap();
    let expected: Vec<u8> = (0..32).collect();
    m.cpu.write(pixels, &expected).unwrap();
    assert_eq!(call(&mut m, api::UNLOCK, &[source, 0]), 0);
    for point in [WindowPoint(100, 50), WindowPoint(130, 81)] {
        m.ext.insert(point);
        for (index, value) in [point.0, point.1, point.0 + 4, point.1 + 2]
            .into_iter()
            .enumerate()
        {
            m.cpu
                .write_u32(DATA + index as u32 * 4, value as u32)
                .unwrap();
        }
        assert_eq!(call(&mut m, api::BLT, &[primary, DATA, source, 0, 0, 0]), 0);
        assert_eq!(m.ext.get::<SurfaceStore>().presenter.bgra, expected);
    }
    // Clearing a clipper must not resolve the fallback window or resize a locked primary.
    assert_eq!(
        call(&mut m, api::LOCK, &[primary, 0, DATA + 64, 0x821, 0]),
        0
    );
    assert_eq!(call(&mut m, api::CLIPPER_SET_HWND, &[clipper, 0, 0]), 0);
    let store = m.ext.get::<SurfaceStore>();
    assert_eq!((store.width, store.height), (4, 2));
    assert_eq!(call(&mut m, api::UNLOCK, &[primary, 0]), 0);
    assert!(
        !m.ext
            .get::<front::State>()
            .draw
            .clipper_origins
            .contains_key(&clipper)
    );
}

#[test]
fn caps_report_nonlocal_video_memory_surfaces_that_create_succeeds_for() {
    let (mut m, device) = setup(false, false, Registry::default());
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    m.cpu.write_u32(DATA + 0x200, 0x17c).unwrap();
    assert_eq!(call(&mut m, api::GET_CAPS, &[device, DATA + 0x200, 0]), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x208).unwrap() & 0x200, 0x200);
    assert_eq!(m.cpu.read_u32(DATA + 0x200 + 60).unwrap(), 64 * 1024 * 1024);
    assert_eq!(m.cpu.read_u32(DATA + 0x200 + 88).unwrap(), 0);
    let surfaces = m.cpu.read_u32(DATA + 0x200 + 0x16c).unwrap();
    assert_eq!(surfaces & 0x3000_5000, 0x3000_5000);
    let mut desc = [0u8; 124];
    desc[..4].copy_from_slice(&124u32.to_le_bytes());
    desc[4..8].copy_from_slice(&7u32.to_le_bytes());
    desc[8..12].copy_from_slice(&2u32.to_le_bytes());
    desc[12..16].copy_from_slice(&4u32.to_le_bytes());
    desc[104..108].copy_from_slice(&(0x1000u32 | 0x4000 | 0x2000_0000).to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 64, DATA, 0]),
        0
    );
}

fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}

fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, addr, len| {
                let mut bytes = vec![0; len];
                cpu.read(addr, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, addr, bytes| cpu.write(addr, bytes).map_err(|e| e.to_string()),
            writable: |cpu, addr, len| {
                adv_abi::guest::check_range(cpu, addr, len as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, src, dst, len| cpu.copy_memory(src, dst, len).map_err(|e| e.to_string()),
        },
    )
}

fn setup(jit: bool, legacy: bool, mut registry: Registry) -> (Machine<JitCpu>, u32) {
    front::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(0x500000, 65536, Prot::RW).unwrap();
    #[cfg(feature = "gpu")]
    m.ext.insert(SurfaceCapture::new().unwrap());
    m.ext
        .insert(SurfaceStore::new(800, 600, 0x500000, 65536, Snapshot::default()).unwrap());
    front::install(
        &mut m,
        front::Provider {
            dc: crate::interop::MemoryDc {
                create: |_, _, _| unreachable!(),
                flush: |_, _, _, _| unreachable!(),
                release: |_, _, _| unreachable!(),
                discard: |_, _| unreachable!(),
            },
            memory,
            window: |_, _, _| None,
            submitted: |_, _, _| {},
            completed: |_, _| None,
            released: |_, _| {},
            vsync: |_| None,
        },
    )
    .unwrap();
    m.cpu.write(DATA + 128, &IID_DDRAW7).unwrap();
    let args = if legacy {
        vec![0, DATA, 0]
    } else {
        vec![0, DATA, DATA + 128, 0]
    };
    assert!(matches!(
        front::call(
            &mut env(&mut m),
            if legacy {
                api::DD_CREATE_OLD
            } else {
                api::DD_CREATE
            },
            &args
        ),
        Outcome::Return(0)
    ));
    let this = m.cpu.read_u32(DATA).unwrap();
    let table = m.cpu.read_u32(this).unwrap();
    let thunk = m
        .resolve_import("ddraw.dll", "IDirectDraw7::EnumDisplayModes")
        .unwrap();
    assert_eq!(m.cpu.read_u32(table + 8 * 4).unwrap(), thunk);
    m.cpu.set_reg(Reg::Esp, DATA + 4096);
    (m, this)
}

#[derive(Default)]
struct Probe {
    rows: Vec<Descriptor>,
    after: bool,
}

#[test]
fn mode_callbacks_stop_continue_and_preserve_descriptors_across_nested_waits() {
    for jit in [false, true] {
        for legacy in [false, true] {
            for stop in [0, 1] {
                let mut registry = Registry::default();
                registry.add("test", "probe", Conv::Stdcall { argc: 2 }, |e| {
                    assert_eq!(e.arg(1), DATA);
                    let mut d = [0; 124];
                    let length = e.cpu.read_u32(e.arg(0)).unwrap() as usize;
                    e.cpu.read(e.arg(0), &mut d[..length]).unwrap();
                    let probe = e.ext.get_mut::<Probe>();
                    if probe.after {
                        assert_eq!(probe.rows.last(), Some(&d));
                    } else {
                        probe.rows.push(d);
                    }
                    probe.after = !probe.after;
                    Outcome::Return(0)
                });
                registry.add("test", "wait", Conv::Stdcall { argc: 0 }, |e| {
                    Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 1000), 0)
                });
                registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
                    Outcome::ExitThread(0)
                });
                let (mut m, this) = setup(jit, legacy, registry);
                m.ext.insert(Probe::default());
                m.cpu.write_u32(DATA + 16, stop).unwrap();
                let api = m
                    .resolve_import("ddraw.dll", "IDirectDraw7::EnumDisplayModes")
                    .unwrap();
                let probe = m.resolve_import("test", "probe").unwrap();
                let wait = m.resolve_import("test", "wait").unwrap();
                let done = m.resolve_import("test", "done").unwrap();
                let callback = CODE + 256;
                let mut main = Asm::new(CODE);
                main.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
                main.mov_mem_eax(DATA + 20)
                    .push_imm(callback)
                    .push_imm(DATA)
                    .push_imm(0)
                    .push_imm(1)
                    .push_imm(this)
                    .call(api)
                    .mov_mem_eax(DATA + 12);
                main.bytes.extend_from_slice(&[0x89, 0xe0]);
                main.mov_mem_eax(DATA + 24).call(done).hlt();
                m.cpu.write(CODE, &main.bytes).unwrap();
                let mut cb = Asm::new(callback);
                cb.mov_eax_esp(8)
                    .push_eax()
                    .mov_eax_esp(8)
                    .push_eax()
                    .call(probe)
                    .push_imm(CODE + 512)
                    .push_imm(0)
                    .push_imm(0)
                    .push_imm(0)
                    .push_imm(this)
                    .call(api)
                    .call(wait)
                    .mov_eax_esp(8)
                    .push_eax()
                    .mov_eax_esp(8)
                    .push_eax()
                    .call(probe)
                    .mov_eax_mem(DATA + 16)
                    .ret_n(8);
                m.cpu.write(callback, &cb.bytes).unwrap();
                let mut inner = Asm::new(CODE + 512);
                inner.call(wait).ret_n(8); // wait returns CANCEL
                m.cpu.write(CODE + 512, &inner.bytes).unwrap();
                m.spawn("modes", CODE, &[], 65536).unwrap();
                loop {
                    match m.run() {
                        RunResult::Exited(_) => break,
                        RunResult::Idle {
                            next_deadline: Some(t),
                        } => m.sys.clock.advance_to(t),
                        other => panic!("unexpected mode result: {other:?}"),
                    }
                }
                assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0);
                assert_eq!(
                    m.cpu.read_u32(DATA + 20).unwrap(),
                    m.cpu.read_u32(DATA + 24).unwrap()
                );
                let rows = &m.ext.get::<Probe>().rows;
                if stop == 0 {
                    assert_eq!(rows.len(), 1);
                } else {
                    for bpp in [16, 32] {
                        assert!(rows.iter().any(|d| word(d, 12) == 800
                            && word(d, 8) == 600
                            && word(d, 84) == bpp));
                    }
                    let mut unique = rows.clone();
                    unique.sort();
                    unique.dedup();
                    assert_eq!(unique.len(), rows.len());
                }
                for d in rows {
                    assert_eq!(word(d, 0), if legacy { 108 } else { 124 });
                    assert_eq!(word(d, 24), if legacy { 0 } else { 60 });
                    assert_eq!(word(d, 36), 0); // modes do not lend pixels
                    assert!(matches!(word(d, 84), 16 | 32));
                }
                let store = m.ext.get::<SurfaceStore>();
                assert_eq!(
                    (store.width, store.height, store.primary_id()),
                    (800, 600, None)
                );
            }
        }
    }
}

fn collect(m: &mut Machine<JitCpu>, this: u32, flags: u32, filter: u32) -> (u32, Vec<Descriptor>) {
    let mut result = enum_modes(
        &mut env(m),
        GuestPtr::new(this),
        flags,
        GuestPtr::new(filter),
        GuestPtr::new(DATA),
        GuestPtr::new(CODE),
    );
    let mut rows = Vec::new();
    loop {
        match result {
            Outcome::Call { args, then, .. } => {
                assert_eq!(args[1], DATA);
                let mut d = [0; 124];
                let length = m.cpu.read_u32(args[0]).unwrap() as usize;
                m.cpu.read(args[0], &mut d[..length]).unwrap();
                rows.push(d);
                result = then(&mut env(m), Resume::Returned(1));
            }
            Outcome::Return(hr) => return (hr, rows),
            _ => panic!("unexpected mode outcome"),
        }
    }
}

#[test]
fn mode_filters_and_invalid_inputs_do_not_create_surfaces_or_call_guest() {
    let (mut m, this) = setup(false, false, Registry::default());
    let mut filter = [0; 124];
    for (offset, value) in [
        (0, 124u32),
        (4, HEIGHT | WIDTH | PIXEL_FORMAT | REFRESH),
        (8, 600),
        (12, 800),
        (24, 60),
        (72, 32),
        (76, 0x40),
        (84, 16),
    ] {
        filter[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    m.cpu.write(DATA + 256, &filter).unwrap();
    let (hr, rows) = collect(&mut m, this, 1, DATA + 256);
    assert_eq!(hr, 0);
    assert_eq!(rows.len(), 1);
    m.cpu.write_u32(DATA + 256 + 76, 0).unwrap();
    m.cpu.write_u32(DATA + 256 + 72, 0).unwrap();
    assert_eq!(collect(&mut m, this, 1, DATA + 256).1, rows);
    assert_eq!(
        (word(&rows[0], 88), word(&rows[0], 92), word(&rows[0], 96)),
        (0xf800, 0x7e0, 0x1f)
    );
    assert_eq!(word(&collect(&mut m, this, 0, DATA + 256).1[0], 24), 0);
    m.cpu.write_u32(DATA + 256 + 84, 8).unwrap();
    assert_eq!(collect(&mut m, this, 0, DATA + 256), (0, vec![]));
    assert_eq!(collect(&mut m, this, 4, 0), (INVALID, vec![]));
    assert_eq!(collect(&mut m, 0, 0, 0), (INVALID_OBJECT, vec![]));
    assert_eq!(collect(&mut m, this, 0, 0xdeadbeef), (INVALID, vec![]));
    m.cpu.write_u32(DATA + 256, 108).unwrap();
    assert_eq!(collect(&mut m, this, 0, DATA + 256), (INVALID, vec![]));
    for callback in [0, DATA] {
        assert!(matches!(
            enum_modes(
                &mut env(&mut m),
                GuestPtr::new(this),
                0,
                GuestPtr::new(0),
                GuestPtr::new(0),
                GuestPtr::new(callback)
            ),
            Outcome::Return(INVALID)
        ));
    }
    m.cpu.set_reg(Reg::Esp, 8);
    assert_eq!(collect(&mut m, this, 0, 0), (INVALID, vec![]));
    assert_eq!(m.ext.get::<SurfaceStore>().primary_id(), None);
    assert!(matches!(
        front::call(&mut env(&mut m), api::RELEASE, &[this]),
        Outcome::Return(0)
    ));
    assert_eq!(collect(&mut m, this, 0, 0), (INVALID_OBJECT, vec![]));
}

#[test]
// spec: GFX-DDRAW-2
fn activated_device_initialization_identity_and_release_share_the_function_owner() {
    let (mut m, created) = setup(false, false, Registry::default());
    assert!(matches!(
        front::call(&mut env(&mut m), api::RELEASE, &[created]),
        Outcome::Return(0)
    ));
    assert!(matches!(
        front::activate(&mut env(&mut m), IID_DDRAW7, 1, DATA),
        Outcome::Return(0x80040110)
    ));
    assert!(matches!(
        front::activate(&mut env(&mut m), [0xff; 16], 0, DATA),
        Outcome::Return(0x80004002)
    ));
    assert!(matches!(
        front::activate(&mut env(&mut m), IID_DDRAW7, 0, DATA),
        Outcome::Return(0)
    ));
    let this = m.cpu.read_u32(DATA).unwrap();
    assert!(matches!(
        front::call(&mut env(&mut m), api::GET_CAPS, &[this, 0, 0]),
        Outcome::Return(0x800401f0)
    ));
    assert!(matches!(
        front::call(&mut env(&mut m), api::INITIALIZE, &[this, 0]),
        Outcome::Return(0)
    ));
    assert!(matches!(
        front::call(&mut env(&mut m), api::INITIALIZE, &[this, 0]),
        Outcome::Return(0x88760005)
    ));
    m.cpu.write(DATA + 128, &adv_abi::com::IUNKNOWN).unwrap();
    assert!(matches!(
        front::call(&mut env(&mut m), api::QUERY, &[this, DATA + 128, DATA + 4]),
        Outcome::Return(0)
    ));
    let identity = m.cpu.read_u32(DATA + 4).unwrap();
    assert_eq!(identity, this);
    m.cpu.write_u32(DATA + 1200, 0xabcdef01).unwrap();
    assert!(matches!(
        front::get_device_identifier(
            &mut env(&mut m),
            GuestPtr::new(this),
            GuestPtr::new(DATA + 128),
            0
        ),
        Outcome::Return(0)
    ));
    assert_eq!(m.cpu.read_u32(DATA + 1200).unwrap(), 0xabcdef01);
    assert_eq!(m.cpu.read_u32(DATA + 128 + 1064).unwrap(), 0);
    assert_ne!(m.cpu.read_u32(DATA + 128 + 1048).unwrap(), 0);
    assert!(matches!(
        front::call(&mut env(&mut m), api::RELEASE, &[identity]),
        Outcome::Return(1)
    ));
    assert!(matches!(
        front::call(&mut env(&mut m), api::RELEASE, &[this]),
        Outcome::Return(0)
    ));
    assert!(matches!(
        front::call(&mut env(&mut m), api::INITIALIZE, &[this, 0]),
        Outcome::Return(INVALID)
    ));
}

#[test]
// spec: GFX-DDRAW-6
fn exclusive_mode_of_a_new_device_loses_a_primary_left_by_a_released_one() {
    let (mut m, first) = setup(false, false, Registry::default());
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    let mut desc = [0u8; 124];
    desc[..4].copy_from_slice(&124u32.to_le_bytes());
    desc[4..8].copy_from_slice(&1u32.to_le_bytes());
    desc[104..108].copy_from_slice(&0x200u32.to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(call(&mut m, api::COOPERATIVE, &[first, 0, 8]), 0);
    assert_eq!(call(&mut m, api::DISPLAY_MODE, &[first, 8, 8, 32, 0, 0]), 0);
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[first, DATA + 64, DATA, 0]),
        0
    );
    let leaked = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(call(&mut m, api::RELEASE, &[first]), 0);
    assert_eq!(call(&mut m, api::IS_LOST, &[leaked]), 0);

    m.cpu.write(DATA + 256, &IID_DDRAW7).unwrap();
    assert_eq!(call(&mut m, api::DD_CREATE, &[0, DATA, DATA + 256, 0]), 0);
    let second = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(call(&mut m, api::COOPERATIVE, &[second, 7, 0x11]), 0);
    assert_eq!(call(&mut m, api::IS_LOST, &[leaked]), 0x887601c2);
    assert_eq!(
        call(&mut m, api::DISPLAY_MODE, &[second, 16, 8, 32, 0, 0]),
        0
    );
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[second, DATA + 64, DATA, 0]),
        0
    );
    let primary = m.cpu.read_u32(DATA).unwrap();
    assert_ne!(primary, leaked);
    assert_eq!(call(&mut m, api::IS_LOST, &[primary]), 0);
    // The owner entering exclusive mode again keeps its own primary.
    assert_eq!(call(&mut m, api::COOPERATIVE, &[second, 7, 0x11]), 0);
    assert_eq!(call(&mut m, api::IS_LOST, &[primary]), 0);
    assert_eq!(call(&mut m, api::RELEASE, &[leaked]), 0);
}

#[test]
// spec: GFX-DDRAW-7
fn clip_lists_limit_blits_and_fills_like_windows() {
    let (mut m, device) = setup(false, false, Registry::default());
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    let surface = |m: &mut Machine<JitCpu>, size: u32| {
        let mut desc = [0u8; 124];
        desc[..4].copy_from_slice(&124u32.to_le_bytes());
        desc[4..8].copy_from_slice(&7u32.to_le_bytes());
        desc[8..12].copy_from_slice(&size.to_le_bytes());
        desc[12..16].copy_from_slice(&size.to_le_bytes());
        desc[104..108].copy_from_slice(&0x40u32.to_le_bytes());
        m.cpu.write(DATA + 0x400, &desc).unwrap();
        assert_eq!(
            call(m, api::CREATE_SURFACE, &[device, DATA + 0x400, DATA, 0]),
            0
        );
        m.cpu.read_u32(DATA).unwrap()
    };
    let fill = |m: &mut Machine<JitCpu>, target: u32, rect: u32, color: u32| {
        let mut fx = [0u8; 100];
        fx[..4].copy_from_slice(&100u32.to_le_bytes());
        fx[80..84].copy_from_slice(&color.to_le_bytes());
        m.cpu.write(DATA + 0x500, &fx).unwrap();
        call(m, api::BLT, &[target, rect, 0, 0, 0x400, DATA + 0x500])
    };
    let rows = |m: &mut Machine<JitCpu>, target: u32| -> Vec<String> {
        m.cpu.write_u32(DATA + 0x400, 124).unwrap();
        assert_eq!(call(m, api::LOCK, &[target, 0, DATA + 0x400, 1, 0]), 0);
        let (pitch, pixels) = (
            m.cpu.read_u32(DATA + 0x400 + 16).unwrap(),
            m.cpu.read_u32(DATA + 0x400 + 36).unwrap(),
        );
        let rows = (0..8)
            .map(|y| {
                (0..8)
                    .map(
                        |x| match m.cpu.read_u32(pixels + y * pitch + x * 4).unwrap() & 0xffffff {
                            0 => '.',
                            0xff => 'B',
                            0xff0000 => 'R',
                            _ => '?',
                        },
                    )
                    .collect()
            })
            .collect();
        assert_eq!(call(m, api::UNLOCK, &[target, 0]), 0);
        rows
    };
    let rect = |m: &mut Machine<JitCpu>, at: u32, r: [i32; 4]| {
        for (i, v) in r.into_iter().enumerate() {
            m.cpu.write_u32(at + i as u32 * 4, v as u32).unwrap();
        }
    };
    let (dst, src, half) = (surface(&mut m, 8), surface(&mut m, 8), surface(&mut m, 4));
    assert_eq!(fill(&mut m, src, 0, 0xff), 0);
    assert_eq!(fill(&mut m, half, 0, 0xff0000), 0);
    assert_eq!(call(&mut m, api::CREATE_CLIPPER, &[device, 0, DATA, 0]), 0);
    let clipper = m.cpu.read_u32(DATA).unwrap();

    // RGNDATA with two rectangles.
    let region = DATA + 0x600;
    for (i, v) in [32u32, 1, 2, 32, 0, 0, 0, 0].into_iter().enumerate() {
        m.cpu.write_u32(region + i as u32 * 4, v).unwrap();
    }
    rect(&mut m, region + 32, [0, 0, 3, 8]);
    rect(&mut m, region + 48, [5, 2, 8, 4]);
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, region, 1]),
        0x80070057
    );
    m.cpu.write_u32(region + 4, 2).unwrap();
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, region, 0]),
        0x8876006e
    );
    m.cpu.write_u32(region + 4, 1).unwrap();
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, region, 0]),
        0
    );
    assert_eq!(call(&mut m, api::SET_CLIPPER, &[dst, clipper]), 0);

    let two = [
        "BBB.....", "BBB.....", "BBB..BBB", "BBB..BBB", "BBB.....", "BBB.....", "BBB.....",
        "BBB.....",
    ];
    assert_eq!(fill(&mut m, dst, 0, 0xff), 0);
    assert_eq!(rows(&mut m, dst), two);
    for (source, color) in [(src, 'B'), (half, 'R')] {
        let mut clear = [0u8; 100];
        clear[..4].copy_from_slice(&100u32.to_le_bytes());
        m.cpu.write(DATA + 0x500, &clear).unwrap();
        assert_eq!(call(&mut m, api::SET_CLIPPER, &[dst, 0]), 0);
        assert_eq!(fill(&mut m, dst, 0, 0), 0);
        assert_eq!(call(&mut m, api::SET_CLIPPER, &[dst, clipper]), 0);
        assert_eq!(call(&mut m, api::BLT, &[dst, 0, source, 0, 0, 0]), 0);
        let expected: Vec<String> = two
            .iter()
            .map(|row| row.replace('B', &color.to_string()))
            .collect();
        assert_eq!(rows(&mut m, dst), expected);
    }
    assert_eq!(
        call(&mut m, api::BLT_FAST, &[dst, 0, 0, src, 0, 0]),
        0x8876023e
    );
    for (flags, hr) in [(0, 0x80070057), (1, 0), (2, 0), (3, 0x80070057)] {
        assert_eq!(call(&mut m, api::GET_BLT_STATUS, &[dst, flags]), hr);
        assert_eq!(call(&mut m, api::GET_FLIP_STATUS, &[dst, flags]), hr);
    }

    // With a full-surface list a destination may extend past the surface.
    m.cpu.write_u32(region + 8, 1).unwrap();
    rect(&mut m, region + 32, [0, 0, 8, 8]);
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, region, 0]),
        0
    );
    assert_eq!(call(&mut m, api::SET_CLIPPER, &[dst, 0]), 0);
    assert_eq!(fill(&mut m, dst, 0, 0), 0);
    rect(&mut m, DATA + 0x700, [4, 4, 12, 12]);
    assert_eq!(
        call(&mut m, api::BLT, &[dst, DATA + 0x700, src, 0, 0, 0]),
        0x80070057
    );
    assert_eq!(call(&mut m, api::SET_CLIPPER, &[dst, clipper]), 0);
    assert_eq!(
        call(&mut m, api::BLT, &[dst, DATA + 0x700, src, 0, 0, 0]),
        0
    );
    assert_eq!(
        rows(&mut m, dst),
        [
            "........", "........", "........", "........", "....BBBB", "....BBBB", "....BBBB",
            "....BBBB"
        ]
    );

    // A window clipper cannot take a clip list.
    assert_eq!(call(&mut m, api::CLIPPER_SET_HWND, &[clipper, 0, 7]), 0);
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, region, 0]),
        0x88760237
    );
    assert_eq!(
        call(&mut m, api::CLIPPER_SET_CLIP_LIST, &[clipper, 0, 0]),
        0x88760237
    );
}

#[test]
// spec: GFX-DDRAW-8
fn flip_to_gdi_surface_shows_the_primary_memory_again() {
    let (mut m, device) = setup(false, false, Registry::default());
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    // The address of the surface's current memory, as Lock reports it.
    let lock = |m: &mut Machine<JitCpu>, surface: u32| {
        m.cpu.write_u32(DATA + 64, 124).unwrap();
        assert_eq!(call(m, api::LOCK, &[surface, 0, DATA + 64, 0x1, 0]), 0);
        let pixels = m.cpu.read_u32(DATA + 100).unwrap();
        assert_eq!(call(m, api::UNLOCK, &[surface, 0]), 0);
        pixels
    };
    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[device]), 0x887600ff);
    assert_eq!(call(&mut m, api::COOPERATIVE, &[device, 0, 0x11]), 0);
    assert_eq!(
        call(&mut m, api::DISPLAY_MODE, &[device, 8, 8, 32, 0, 0]),
        0
    );
    let mut desc = [0u8; 124];
    desc[..4].copy_from_slice(&124u32.to_le_bytes());
    // DDSD_CAPS | DDSD_BACKBUFFERCOUNT; primary, flip, complex.
    desc[4..8].copy_from_slice(&0x21u32.to_le_bytes());
    desc[20..24].copy_from_slice(&1u32.to_le_bytes());
    desc[104..108].copy_from_slice(&0x218u32.to_le_bytes());
    m.cpu.write(DATA + 256, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 256, DATA, 0]),
        0
    );
    let primary = m.cpu.read_u32(DATA).unwrap();
    m.cpu.write_u32(DATA + 4, 4).unwrap();
    assert_eq!(
        call(&mut m, api::GET_ATTACHED, &[primary, DATA + 4, DATA + 8]),
        0
    );
    let back = m.cpu.read_u32(DATA + 8).unwrap();
    let gdi = lock(&mut m, primary);
    let other = lock(&mut m, back);
    assert_ne!(gdi, other);

    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[device]), 0);
    assert_eq!(lock(&mut m, primary), gdi);
    assert_eq!(call(&mut m, api::FLIP, &[primary, 0, 1]), 0);
    assert_eq!(lock(&mut m, primary), other);
    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[device]), 0);
    assert_eq!(lock(&mut m, primary), gdi);
    assert_eq!(lock(&mut m, back), other);
    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[device]), 0);
    assert_eq!(lock(&mut m, primary), gdi);
    // Two flips leave GDI's memory in front.
    assert_eq!(call(&mut m, api::FLIP, &[primary, 0, 1]), 0);
    assert_eq!(call(&mut m, api::FLIP, &[primary, back, 1]), 0);
    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[device]), 0);
    assert_eq!(lock(&mut m, primary), gdi);

    // Only the object that created the primary has a GDI surface.
    m.cpu.write(DATA + 512, &IID_DDRAW7).unwrap();
    assert_eq!(call(&mut m, api::DD_CREATE, &[0, DATA, DATA + 512, 0]), 0);
    let second = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(call(&mut m, api::FLIP_TO_GDI, &[second]), 0x887600ff);
}

#[test]
// spec: GFX-DDRAW-9
fn scan_line_guest_vtable_reports_progress_blank_and_invalid_outputs() {
    for jit in [false, true] {
        for legacy in [false, true] {
            for (time, output, expected_hr, expected_line) in [
                (0, DATA + 32, 0, Some(0)),
                (8_333, DATA + 32, 0, Some(300)),
                (16_650, DATA + 32, 0x88760219, None),
                (16_667, DATA + 32, 0, Some(0)),
                (0, 0, 0x80070057, None),
                (0, DATA + 4094, 0x80070057, None),
                (0, CODE, 0x80070057, None),
            ] {
                let mut registry = Registry::default();
                registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
                    Outcome::ExitThread(0)
                });
                let (mut m, this) = setup(jit, legacy, registry);
                m.sys.clock.advance_to(m.sys.clock.now_us() + time);
                let done = m.resolve_import("test", "done").unwrap();
                let table = m.cpu.read_u32(this).unwrap();
                let method = m.cpu.read_u32(table + 16 * 4).unwrap();
                m.cpu.write_u32(DATA + 32, 0xdeadbeef).unwrap();
                let mut code = Asm::new(CODE);
                code.bytes.extend_from_slice(&[0x89, 0xe0]);
                code.mov_mem_eax(DATA + 40)
                    .push_imm(output)
                    .push_imm(this)
                    .call(method)
                    .mov_mem_eax(DATA + 36);
                code.bytes.extend_from_slice(&[0x89, 0xe0]);
                code.mov_mem_eax(DATA + 44).call(done).hlt();
                m.cpu.write(CODE, &code.bytes).unwrap();
                m.spawn("scan line caller", CODE, &[], 65536).unwrap();
                let result = m.run();
                assert!(
                    matches!(result, RunResult::Exited(_)),
                    "scan-line guest call failed: {result:?}"
                );
                assert_eq!(m.cpu.read_u32(DATA + 36).unwrap(), expected_hr);
                assert_eq!(
                    m.cpu.read_u32(DATA + 32).unwrap(),
                    expected_line.unwrap_or(0xdeadbeef)
                );
                assert_eq!(
                    m.cpu.read_u32(DATA + 40).unwrap(),
                    m.cpu.read_u32(DATA + 44).unwrap()
                );
            }
        }
    }
}

#[test]
// spec: GFX-DDRAW-9
fn scan_line_polling_progresses_and_checks_device_state() {
    let (mut m, this) = setup(false, false, Registry::default());
    let query = |m: &mut Machine<JitCpu>, device| match front::get_scan_line(
        &mut env(m),
        GuestPtr::new(device),
        GuestPtr::new(DATA + 32),
    ) {
        Outcome::Return(hr) => hr,
        _ => panic!("scan line must return synchronously"),
    };
    m.cpu.write_u32(DATA + 32, 0xdeadbeef).unwrap();
    assert_eq!(query(&mut m, 0), 0x88760082);
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0xdeadbeef);
    let com = m.ext.get::<front::State>().draw.com.unwrap();
    com.get_mut::<ComState>(&mut m.sys.objects, this)
        .unwrap()
        .initialized = false;
    assert_eq!(query(&mut m, this), 0x800401f0);
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0xdeadbeef);
    com.get_mut::<ComState>(&mut m.sys.objects, this)
        .unwrap()
        .initialized = true;
    assert_eq!(query(&mut m, this), 0);
    let first = m.cpu.read_u32(DATA + 32).unwrap();
    for _ in 0..100 {
        assert_eq!(query(&mut m, this), 0);
    }
    assert!(m.cpu.read_u32(DATA + 32).unwrap() > first);
    m.ext.get_mut::<SurfaceStore>().height = 480;
    m.sys.clock.advance_to(m.sys.clock.epoch_us + 8_333);
    assert_eq!(query(&mut m, this), 0);
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 240);
}

#[test]
// spec: GFX-DDRAW-10
fn windowed_primary_without_clipper_translates_screen_rectangles() {
    windowed_primary_screen_rectangles(7);
}

#[test]
// spec: GFX-DDRAW-10 GFX-DDRAW-11
fn windowed_primary_without_hwnd_uses_presented_window() {
    windowed_primary_screen_rectangles(0);
}

fn windowed_primary_screen_rectangles(cooperative_window: u32) {
    #[derive(Clone, Copy)]
    struct ClientOrigin(i32, i32);
    let (mut m, device) = setup(false, false, Registry::default());
    m.ext.insert(ClientOrigin(100, 50));
    m.ext.insert(std::cell::Cell::new(0u32));
    m.ext.get_mut::<front::State>().provider.submitted = |ext, hwnd, _| {
        ext.get::<std::cell::Cell<u32>>().set(hwnd);
    };
    m.ext.get_mut::<front::State>().provider.window = |ext, _, hwnd| {
        let origin = ext.get::<ClientOrigin>();
        (hwnd == 0 || hwnd == 7).then_some(front::WindowGeometry {
            hwnd: 7,
            width: 4,
            height: 2,
            origin: (origin.0, origin.1),
        })
    };
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| {
        let hr = match front::call(&mut env(m), id, args) {
            Outcome::Return(hr) => hr,
            _ => panic!("synchronous authored call"),
        };
        #[cfg(feature = "gpu")]
        {
            let (capture, surfaces) = m.ext.get2_installed_mut::<SurfaceCapture, SurfaceStore>();
            capture.update(surfaces);
        }
        hr
    };
    assert_eq!(
        call(&mut m, api::COOPERATIVE, &[device, cooperative_window, 8]),
        0
    );
    let mut desc = [0u8; 124];
    for (offset, value) in [(0, 124u32), (4, 7), (8, 2), (12, 4), (104, 0x200)] {
        desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 64, DATA, 0]),
        0
    );
    let primary = m.cpu.read_u32(DATA).unwrap();
    desc[104..108].copy_from_slice(&0x40u32.to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[device, DATA + 64, DATA, 0]),
        0
    );
    let source = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        call(&mut m, api::LOCK, &[source, 0, DATA + 64, 0x821, 0]),
        0
    );
    let pixels = m.cpu.read_u32(DATA + 100).unwrap();
    assert_eq!(call(&mut m, api::UNLOCK, &[source, 0]), 0);
    // Match the optional DDBLTFX accepted by both ASYNC and WAIT calls.
    m.cpu.write(DATA + 256, &[0u8; 100]).unwrap();
    m.cpu.write_u32(DATA + 256, 100).unwrap();
    for (index, (origin, flags)) in [
        (ClientOrigin(100, 50), 0x200),
        (ClientOrigin(140, 75), 0x0100_0000),
        (ClientOrigin(-20, -10), 0x200),
    ]
    .into_iter()
    .enumerate()
    {
        m.ext.insert(origin);
        let mut expected = vec![0u8; 32];
        for pixel in expected.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[index as u8 + 1, 20, 30, 255]);
        }
        m.cpu.write(pixels, &expected).unwrap();
        for (offset, value) in [origin.0, origin.1, origin.0 + 4, origin.1 + 2]
            .into_iter()
            .enumerate()
        {
            m.cpu
                .write_u32(DATA + offset as u32 * 4, value as u32)
                .unwrap();
        }
        let before = m.ext.get::<SurfaceStore>().stats.presentations;
        assert_eq!(
            call(
                &mut m,
                api::BLT,
                &[primary, DATA, source, 0, flags, DATA + 256]
            ),
            0
        );
        assert!(m.ext.get::<SurfaceStore>().stats.presentations > before);
        assert_eq!(m.ext.get::<SurfaceStore>().presenter.bgra, expected);
        assert_eq!(m.ext.get::<std::cell::Cell<u32>>().get(), 7);
    }
    // Offscreen destinations and exclusive-mode primaries retain local coordinates.
    for (offset, value) in [0u32, 0, 4, 2].into_iter().enumerate() {
        m.cpu.write_u32(DATA + offset as u32 * 4, value).unwrap();
    }
    assert_eq!(call(&mut m, api::BLT, &[source, DATA, primary, 0, 0, 0]), 0);
    assert_eq!(call(&mut m, api::COOPERATIVE, &[device, 7, 0x11]), 0);
    assert_eq!(call(&mut m, api::BLT, &[primary, DATA, source, 0, 0, 0]), 0);
}

#[test]
// spec: GFX-DDRAW-12
fn timing_memory_and_caps_match_virtual_display_profile() {
    let (mut m, device) = setup(false, false, Registry::default());
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    m.cpu.write_u32(DATA + 0x200, 0x17c).unwrap();
    assert_eq!(call(&mut m, api::GET_CAPS, &[device, DATA + 0x200, 0]), 0);
    assert_eq!(
        m.cpu.read_u32(DATA + 0x204).unwrap() & 0x0002_0000,
        0x0002_0000
    );

    // GetMonitorFrequency validation and output.
    m.cpu.write_u32(DATA + 16, 0xdeadbeef).unwrap();
    assert!(matches!(
        front::get_monitor_frequency(&mut env(&mut m), GuestPtr::new(0), GuestPtr::new(DATA + 16)),
        Outcome::Return(0x88760082)
    ));
    assert!(matches!(
        front::get_monitor_frequency(&mut env(&mut m), GuestPtr::new(device), GuestPtr::new(0),),
        Outcome::Return(0x80070057)
    ));
    assert!(matches!(
        front::get_monitor_frequency(&mut env(&mut m), GuestPtr::new(device), GuestPtr::new(CODE),),
        Outcome::Return(0x80070057)
    ));
    assert!(matches!(
        front::get_monitor_frequency(
            &mut env(&mut m),
            GuestPtr::new(device),
            GuestPtr::new(DATA + 16),
        ),
        Outcome::Return(0)
    ));
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 60);

    // GetVerticalBlankStatus shares the virtual scan window and host vsync hook.
    let vblank = |m: &mut Machine<JitCpu>, out| match front::get_vertical_blank_status(
        &mut env(m),
        GuestPtr::new(device),
        GuestPtr::new(out),
    ) {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    assert_eq!(vblank(&mut m, 0), 0x80070057);
    m.sys.clock.advance_to(m.sys.clock.epoch_us + 8_333);
    assert_eq!(vblank(&mut m, DATA + 16), 0);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    m.sys.clock.advance_to(m.sys.clock.epoch_us + 16_650);
    assert_eq!(vblank(&mut m, DATA + 16), 0);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 1);
    m.ext.get_mut::<front::State>().provider.vsync = |_| {
        Some((
            std::time::Duration::from_millis(8),
            std::time::Duration::from_millis(16),
        ))
    };
    assert_eq!(vblank(&mut m, DATA + 16), 0);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);

    // GetAvailableVidMem validates caps and optional output pointers.
    let vidmem = |m: &mut Machine<JitCpu>, caps, total, free| match front::get_available_vid_mem(
        &mut env(m),
        GuestPtr::new(device),
        GuestPtr::new(caps),
        GuestPtr::new(total),
        GuestPtr::new(free),
    ) {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };
    m.cpu.write(DATA + 64, &[0u8; 16]).unwrap();
    assert_eq!(vidmem(&mut m, 0, DATA + 16, DATA + 20), 0x80070057);
    assert_eq!(vidmem(&mut m, DATA + 64, 0, 0), 0x80070057);
    m.cpu.write_u32(DATA + 64, 0x800).unwrap();
    assert_eq!(vidmem(&mut m, DATA + 64, DATA + 16, DATA + 20), 0x80070057);
    m.cpu.write_u32(DATA + 64, 0x4000).unwrap();
    assert_eq!(vidmem(&mut m, DATA + 64, DATA + 16, 0), 0);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 64 * 1024 * 1024);
    m.cpu.write_u32(DATA + 20, 0).unwrap();
    assert_eq!(vidmem(&mut m, DATA + 64, 0, DATA + 20), 0);
    assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 32 * 1024 * 1024);
}

#[test]
// spec: GFX-DDRAW-12
fn cooperative_status_compact_and_restore_all_surfaces_follow_exclusive_ownership() {
    let mut registry = Registry::default();
    registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    let (mut m, first) = setup(false, false, registry);
    let call = |m: &mut Machine<JitCpu>, id, args: &[u32]| match front::call(&mut env(m), id, args)
    {
        Outcome::Return(hr) => hr,
        _ => panic!("synchronous authored call"),
    };

    // Normal mode rejects Compact and reports DD_OK cooperative status.
    assert_eq!(call(&mut m, api::COMPACT, &[first]), 0x887600e1);
    assert_eq!(call(&mut m, api::TEST_COOPERATIVE_LEVEL, &[first]), 0);
    assert_eq!(call(&mut m, api::RESTORE_ALL_SURFACES, &[first]), 0);

    // Invoke vtable slots 3 (Compact), 25 (RestoreAllSurfaces), 26 (TestCooperativeLevel) as guest.
    assert_eq!(call(&mut m, api::COOPERATIVE, &[first, 7, 0x11]), 0);
    let table = m.cpu.read_u32(first).unwrap();
    let compact = m.cpu.read_u32(table + 3 * 4).unwrap();
    let restore_all = m.cpu.read_u32(table + 25 * 4).unwrap();
    let test_coop = m.cpu.read_u32(table + 26 * 4).unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(first)
        .call(compact)
        .mov_mem_eax(DATA + 32)
        .push_imm(first)
        .call(restore_all)
        .mov_mem_eax(DATA + 36)
        .push_imm(first)
        .call(test_coop)
        .mov_mem_eax(DATA + 40)
        .call(done)
        .hlt();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("ddraw status caller", CODE, &[], 65536).unwrap();
    assert!(matches!(m.run(), RunResult::Exited(_)));
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 36).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 40).unwrap(), 0);

    // Create a primary on first, then create a second DirectDraw object.
    assert_eq!(call(&mut m, api::DISPLAY_MODE, &[first, 8, 8, 32, 0, 0]), 0);
    let mut desc = [0u8; 124];
    desc[..4].copy_from_slice(&124u32.to_le_bytes());
    desc[4..8].copy_from_slice(&1u32.to_le_bytes());
    desc[104..108].copy_from_slice(&0x200u32.to_le_bytes());
    m.cpu.write(DATA + 64, &desc).unwrap();
    assert_eq!(
        call(&mut m, api::CREATE_SURFACE, &[first, DATA + 64, DATA, 0]),
        0
    );
    let first_primary = m.cpu.read_u32(DATA).unwrap();
    m.cpu.write(DATA + 128, &IID_DDRAW7).unwrap();
    assert_eq!(call(&mut m, api::DD_CREATE, &[0, DATA, DATA + 128, 0]), 0);
    let second = m.cpu.read_u32(DATA).unwrap();

    // Non-owner sees DDERR_EXCLUSIVEMODEALREADYSET while first owns exclusive mode.
    assert_eq!(
        call(&mut m, api::TEST_COOPERATIVE_LEVEL, &[second]),
        0x88760245
    );
    assert_eq!(
        call(&mut m, api::RESTORE_ALL_SURFACES, &[second]),
        0x88760245
    );

    // Second preempting exclusive mode loses first's exclusive status and primary.
    assert_eq!(call(&mut m, api::COOPERATIVE, &[second, 7, 0x11]), 0);
    assert_eq!(
        call(&mut m, api::TEST_COOPERATIVE_LEVEL, &[first]),
        0x887600e1
    );
    assert_eq!(call(&mut m, api::COMPACT, &[first]), 0x887600e1);
    assert_eq!(call(&mut m, api::IS_LOST, &[first_primary]), 0x887601c2);
    assert_eq!(
        call(&mut m, api::RESTORE_ALL_SURFACES, &[first]),
        0x88760245
    );
    assert_eq!(call(&mut m, api::RESTORE_ALL_SURFACES, &[second]), 0);
    assert_eq!(call(&mut m, api::IS_LOST, &[first_primary]), 0x887601c2);

    // Returning second to normal mode allows first to restore its own surfaces.
    assert_eq!(call(&mut m, api::COOPERATIVE, &[second, 0, 8]), 0);
    assert_eq!(call(&mut m, api::TEST_COOPERATIVE_LEVEL, &[second]), 0);
    assert_eq!(call(&mut m, api::RESTORE_ALL_SURFACES, &[first]), 0);
    assert_eq!(call(&mut m, api::IS_LOST, &[first_primary]), 0);
}
