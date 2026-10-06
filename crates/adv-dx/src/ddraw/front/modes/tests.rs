use super::*;
use crate::ddraw::{IID_DDRAW7, api, front};
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot};
use adv_machine::{Clock, Conv, Machine, Resume, RunResult, Wait, layout::PARK_ADDR};
use adv_os::gdi::Gdi;
use std::borrow::Cow;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

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
    m.ext
        .insert(SurfaceStore::new(800, 600, 0x500000, 65536, Snapshot::default()).unwrap());
    front::install(
        &mut m,
        Gdi::default(),
        front::Provider {
            memory,
            window: |_, _, _| None,
            completed: |_, _| None,
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
