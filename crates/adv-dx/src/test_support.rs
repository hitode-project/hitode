//! Minimal process state for DirectX front tests, without a Windows personality.
#![cfg(test)]
#![allow(dead_code)]
use adv_abi::{Conv, Env, Outcome, Registry};
use adv_cpu::backend::{Backend, GuestExt};
use adv_machine::{Machine, Wait, layout::teb};
use adv_os::{
    Heaps, Modules, OsConfig, Vfs,
    com::{ClassFactories, Iid},
    vfs::FileObj,
};

pub fn install<B: Backend>(machine: &mut Machine<B>) {
    machine.ext.get_or_default::<Modules>();
    let mut heaps = Heaps::default();
    heaps.process_heap = heaps.create(&mut machine.cpu, &mut machine.sys.vm).unwrap();
    machine.ext.insert(heaps);
    machine.ext.insert(Vfs::new(&OsConfig::new(
        ".",
        "fixture.exe",
        "target/dx-tests",
    )));
}

pub fn activate(env: &mut Env<'_>, class: Iid, iid: Iid, outer: u32, output: u32) -> Outcome {
    let factory = env.ext.get::<ClassFactories>().get(&class).unwrap();
    factory(env, iid, outer, output)
}

pub fn register(registry: &mut Registry) {
    for (name, argc, handler) in [
        ("SetLastError", 1, set_error as adv_machine::Handler),
        ("GetLastError", 0, get_error),
        ("Sleep", 1, sleep),
        ("lstrcpyA", 2, copy_a),
        ("lstrcpyW", 2, copy_w),
        ("CreateFileW", 7, open),
    ] {
        registry.add("kernel32.dll", name, Conv::Stdcall { argc }, handler);
    }
}
fn set_error(env: &mut Env<'_>) -> Outcome {
    env.cpu
        .write_u32(env.ctx().teb + teb::LAST_ERROR, env.arg(0))
        .unwrap();
    Outcome::Return(0)
}
fn get_error(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(env.cpu.read_u32(env.ctx().teb + teb::LAST_ERROR).unwrap())
}
fn sleep(env: &mut Env<'_>) -> Outcome {
    Outcome::block_then_return(
        Wait::Until(env.sys.clock.now_us() + u64::from(env.arg(0)) * 1000),
        0,
    )
}
fn copy_a(env: &mut Env<'_>) -> Outcome {
    copy(env, 1)
}
fn copy_w(env: &mut Env<'_>) -> Outcome {
    copy(env, 2)
}
fn copy(env: &mut Env<'_>, unit: u32) -> Outcome {
    let (destination, source) = (env.arg(0), env.arg(1));
    let mut offset = 0;
    loop {
        let value = if unit == 1 {
            u16::from(env.cpu.read_u8(source + offset).unwrap())
        } else {
            env.cpu.read_u16(source + offset).unwrap()
        };
        env.cpu
            .write(destination + offset, &value.to_le_bytes()[..unit as usize])
            .unwrap();
        if value == 0 {
            break;
        }
        offset += unit;
    }
    Outcome::Return(destination)
}
fn open(env: &mut Env<'_>) -> Outcome {
    let mut text = Vec::new();
    let mut address = env.arg(0);
    loop {
        let unit = env.cpu.read_u16(address).unwrap();
        if unit == 0 {
            break;
        }
        text.push(unit);
        address += 2;
    }
    let vfs = env.ext.get::<Vfs>();
    let path = vfs.normalize(&String::from_utf16(&text).unwrap());
    let access = env.arg(1);
    // Source-filter tests open existing files and check the retained read share.
    if env.sys.objects.hosts::<FileObj>().any(|(_, file)| {
        file.guest_path == path && access & 0x4000_0000 != 0 && file.share & 2 == 0
    }) {
        return Outcome::Return(u32::MAX);
    }
    let Some(file) = vfs
        .lookup(&path)
        .and_then(|path| std::fs::File::open(path).ok())
    else {
        return Outcome::Return(u32::MAX);
    };
    let share = env.arg(2);
    Outcome::Return(env.sys.objects.insert_host(FileObj {
        file: file.into(),
        guest_path: path,
        access,
        share,
    }))
}

#[cfg(feature = "gpu")]
pub mod dc {
    use super::*;
    use adv_dx::interop::MemoryDc;
    use std::collections::BTreeMap;

    #[derive(Default)]
    pub struct State {
        next: u32,
        pub pixels: BTreeMap<u32, Vec<u8>>,
    }
    pub fn provider() -> MemoryDc {
        MemoryDc {
            create: |env, surface, then| {
                let mut bytes = vec![0; (surface.pitch * surface.height) as usize];
                env.cpu.read(surface.pixels, &mut bytes).unwrap();
                let state = env.ext.get_or_default::<State>();
                state.next += 1;
                let dc = state.next;
                state.pixels.insert(dc, bytes);
                then(env, Ok(dc))
            },
            flush: |env, dc, surface, then| {
                let bytes = &env.ext.get::<State>().pixels[&dc];
                let result = if env.cpu.accessible(
                    surface.pixels,
                    bytes.len() as u32,
                    adv_cpu::backend::Prot::WRITE,
                ) {
                    env.cpu
                        .write(surface.pixels, bytes)
                        .map_err(|error| error.to_string())
                } else {
                    Err("DC writeback is not writable".into())
                };
                then(env, result)
            },
            release: |env, dc, then| {
                discard(env, dc);
                then(env, Ok(()))
            },
            discard,
        }
    }
    fn discard(env: &mut Env<'_>, dc: u32) {
        env.ext.get_mut::<State>().pixels.remove(&dc).unwrap();
    }
}
