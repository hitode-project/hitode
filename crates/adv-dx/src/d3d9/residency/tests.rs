//! Managed eviction follows the documented CPU-copy retention contract:
//! https://learn.microsoft.com/en-us/windows/win32/api/d3d9/nf-d3d9-idirect3ddevice9-evictmanagedresources
//! One isolated GPU fixture, without a window, shader or game.
use super::*;
use adv_abi::{Conv, types::D3DLOCKED_RECT};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Resume, WakeReason};

const STACK: u32 = 0x550000;
#[derive(Default)]
struct Surfaces {
    next: u32,
    targets: BTreeMap<u32, Target>,
    fail_remove: Option<u32>,
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn machine() -> (Machine<JitCpu>, u32) {
    let mut machine = super::super::tests::machine();
    // The ownership-limit fixture deliberately caps refs at three. This test
    // instead retains several independent textures and a mip alias together.
    machine.sys.objects = Objects::default();
    machine.ext.get_mut::<State>().com = Com::new(&mut machine.sys.objects).unwrap();
    machine.cpu.map(STACK, 4096, Prot::RW).unwrap();
    machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
    machine
        .spawn("managed residency fixture", 0x400000, &[], 65536)
        .unwrap();
    machine.ext.insert(Surfaces::default());
    let state = machine.ext.get_mut::<State>();
    state.gpu = Some(
        pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap(),
    );
    state.tables.insert("IDirect3DTexture9", 0);
    state.provider.insert = |ext, target| {
        let surfaces = ext.get_mut::<Surfaces>();
        surfaces.next += 1;
        surfaces.targets.insert(surfaces.next, target);
        Ok(surfaces.next)
    };
    state.provider.target = |ext, surface| ext.get::<Surfaces>().targets.get(&surface).cloned();
    state.provider.remove = |ext, _, surface| {
        let surfaces = ext.get_mut::<Surfaces>();
        if surfaces.fail_remove == Some(surface) {
            surfaces.fail_remove = None;
            return Err("authored retirement failure".into());
        }
        surfaces
            .targets
            .remove(&surface)
            .map(|_| ())
            .ok_or_else(|| "missing owned GPU view".into())
    };
    let (device, _, _, unused) = super::super::tests::setup(&mut env(&mut machine));
    {
        let mut env = env(&mut machine);
        if let Some(Some(dead)) = com(&env).release_internal(&mut env.sys.objects, unused) {
            super::super::destroy(&mut env, dead);
        }
    }
    (machine, device)
}
fn call(machine: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> Outcome {
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    let api = registry.get("d3d9.dll", name).unwrap();
    if name == "IDirect3DDevice9::EvictManagedResources" {
        assert_eq!(api.conv, Conv::Stdcall { argc: 1 });
        let signature =
            adv_abi::signature::lookup_com("IDirect3DDevice9", "EvictManagedResources").unwrap();
        assert_eq!(signature.conv, Some(api.conv));
        assert_eq!(signature.com_slot, Some(5));
    }
    machine.cpu.set_reg(Reg::Esp, STACK);
    machine.cpu.write_u32(STACK, 0x12345678).unwrap();
    for (index, &argument) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(STACK + 4 + index as u32 * 4, argument)
            .unwrap();
    }
    (api.handler)(&mut env(machine))
}
fn okay(outcome: Outcome) {
    assert!(matches!(outcome, Outcome::Return(0)));
}
fn packet<T: bytemuck::Pod>(machine: &mut Machine<JitCpu>, value: T) -> u32 {
    let mut env = env(machine);
    let address = alloc(&mut env, std::mem::size_of::<T>() as u32).unwrap();
    GuestPtr::new(address).write(env.cpu, value).unwrap();
    address
}
fn texture(machine: &mut Machine<JitCpu>, device: u32, levels: u32, pool: u32) -> u32 {
    let output = packet(machine, 0u32);
    okay(call(
        machine,
        "IDirect3DDevice9::CreateTexture",
        &[device, 4, 4, levels, 0, 21, pool, output, 0],
    ));
    machine.cpu.read_u32(output).unwrap()
}
fn views(machine: &mut Machine<JitCpu>, texture: u32) -> Vec<Option<u32>> {
    resources::resource(&env(machine), texture)
        .unwrap()
        .levels
        .iter()
        .map(|level| level.surface)
        .collect()
}
fn lock(machine: &mut Machine<JitCpu>, texture: u32, level: u32) -> D3DLOCKED_RECT {
    let output = packet(machine, D3DLOCKED_RECT::default());
    okay(call(
        machine,
        "IDirect3DTexture9::LockRect",
        &[texture, level, output, 0, 0],
    ));
    GuestPtr::new(output).read(&machine.cpu).unwrap()
}
fn readback(machine: &mut Machine<JitCpu>, surface: u32) -> Vec<u8> {
    super::super::tests::flush_recordings(&mut env(machine));
    let image = machine.ext.get::<Surfaces>().targets[&surface]
        .image
        .clone();
    let mut transfer = machine
        .ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .readback(&image, None)
        .unwrap();
    transfer.take_completion_job().unwrap()();
    transfer.poll().unwrap().unwrap()
}

#[test]
fn managed_eviction_retains_cpu_and_mip_aliases_then_restores_real_pixels_and_cancels_uploads() {
    let (mut machine, device) = machine();
    okay(call(
        &mut machine,
        "IDirect3DDevice9::EvictManagedResources",
        &[device],
    )); // Empty cache.
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::EvictManagedResources",
            &[0]
        ),
        Outcome::Return(INVALID)
    ));
    let first = texture(&mut machine, device, 3, 1);
    let second = texture(&mut machine, device, 1, 1);
    let ordinary = texture(&mut machine, device, 1, 0);
    let ordinary_view = views(&mut machine, ordinary);
    let expected = [
        [0x11, 0x22, 0x33, 0x80],
        [0x44, 0x55, 0x66, 0xc0],
        [0x77, 0x88, 0x99, 0xff],
    ];
    for (level, pixel) in expected.into_iter().enumerate() {
        let locked = lock(&mut machine, first, level as u32);
        let size = 4 >> level;
        machine
            .cpu
            .write(locked.pBits, &pixel.repeat(size * size))
            .unwrap();
        okay(call(
            &mut machine,
            "IDirect3DTexture9::UnlockRect",
            &[first, level as u32],
        ));
    }
    let alias_cell = packet(&mut machine, 0u32);
    okay(call(
        &mut machine,
        "IDirect3DTexture9::GetSurfaceLevel",
        &[first, 1, alias_cell],
    ));
    let alias = machine.cpu.read_u32(alias_cell).unwrap();
    let (storage, references, bytes) = {
        let env = env(&mut machine);
        let r = resources::resource(&env, first).unwrap();
        let id = com(&env)
            .identity::<Payload>(&env.sys.objects, first)
            .unwrap();
        let mut bytes = vec![0; 84];
        env.cpu.read(r.storage, &mut bytes).unwrap();
        (r.storage, com(&env).references(&env.sys.objects, id), bytes)
    };
    let original = views(&mut machine, first);
    let all_views = machine.ext.get::<Surfaces>().targets.len();
    // A later candidate's lock rejects the whole batch, leaving earlier views.
    lock(&mut machine, second, 0);
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::EvictManagedResources",
            &[device]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(views(&mut machine, first), original);
    assert_eq!(machine.ext.get::<Surfaces>().targets.len(), all_views);
    okay(call(
        &mut machine,
        "IDirect3DTexture9::UnlockRect",
        &[second, 0],
    ));
    // An unexpected provider error rolls removed views back without losing GPU
    // pixels or leaving borrowed interfaces pointing at a removed id.
    machine.ext.get_mut::<Surfaces>().fail_remove = original[2];
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::EvictManagedResources",
            &[device]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(machine.ext.get::<Surfaces>().targets.len(), all_views);
    let rollback = views(&mut machine, first);
    assert_eq!(
        resources::resource(&env(&mut machine), alias)
            .unwrap()
            .surface,
        rollback[1]
    );
    for (level, surface) in rollback.iter().enumerate() {
        assert_eq!(
            readback(&mut machine, surface.unwrap()),
            expected[level].repeat((4 >> level) * (4 >> level))
        );
    }
    okay(call(
        &mut machine,
        "IDirect3DDevice9::EvictManagedResources",
        &[device],
    ));
    assert_eq!(views(&mut machine, first), [None; 3]);
    assert_eq!(views(&mut machine, second), [None]);
    assert_eq!(views(&mut machine, ordinary), ordinary_view);
    assert_eq!(machine.ext.get::<Surfaces>().targets.len(), 1);
    {
        let env = env(&mut machine);
        let r = resources::resource(&env, first).unwrap();
        assert_eq!(r.storage, storage);
        assert_eq!(
            resources::storage(&env, resources::resource(&env, alias).unwrap()),
            Some(storage + 64)
        );
        assert!(resources::resource(&env, alias).unwrap().surface.is_none());
        let id = com(&env)
            .identity::<Payload>(&env.sys.objects, first)
            .unwrap();
        assert_eq!(com(&env).references(&env.sys.objects, id), references);
        let mut after = vec![0; 84];
        env.cpu.read(storage, &mut after).unwrap();
        assert_eq!(after, bytes);
    }
    // Cold borrowed aliases still return the existing guest CPU pointer.
    let locked_cell = packet(&mut machine, D3DLOCKED_RECT::default());
    okay(call(
        &mut machine,
        "IDirect3DSurface9::LockRect",
        &[alias, locked_cell, 0, 0x10],
    ));
    let locked: D3DLOCKED_RECT = GuestPtr::new(locked_cell).read(&machine.cpu).unwrap();
    assert_eq!(locked.pBits, storage + 64);
    okay(call(
        &mut machine,
        "IDirect3DSurface9::UnlockRect",
        &[alias],
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, first],
    ));
    for (level, surface) in views(&mut machine, first).iter().enumerate() {
        assert_eq!(
            readback(&mut machine, surface.unwrap()),
            expected[level].repeat((4 >> level) * (4 >> level))
        );
    }
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::EvictManagedResources",
            &[device]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, 0],
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::EvictManagedResources",
        &[device],
    ));
    // Full CPU guards run before allocating even the first replacement view.
    machine
        .cpu
        .protect(storage & !4095, 4096, Prot::WRITE)
        .unwrap();
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::SetTexture",
            &[device, 0, first]
        ),
        Outcome::Return(INVALID)
    ));
    assert_eq!(views(&mut machine, first), [None; 3]);
    assert_eq!(machine.ext.get::<Surfaces>().targets.len(), 1);
    machine
        .cpu
        .protect(storage & !4095, 4096, Prot::RW)
        .unwrap();
    super::super::tests::force_recording_limit(&mut env(&mut machine), device);
    let pause = machine
        .ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .context()
        .pause_submissions()
        .unwrap();
    let Outcome::Block { then, .. } = call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, first],
    ) else {
        panic!("paused upload must wait")
    };
    assert!(
        super::super::device(&env(&mut machine), device)
            .unwrap()
            .texture
            .is_none()
    );
    assert!(matches!(
        call(
            &mut machine,
            "IDirect3DDevice9::EvictManagedResources",
            &[device]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    drop(pause);
    assert!(super::super::pump(&mut machine).unwrap());
    okay(then(
        &mut env(&mut machine),
        Resume::Woke(WakeReason::Signaled),
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, 0],
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::EvictManagedResources",
        &[device],
    ));
    super::super::tests::force_recording_limit(&mut env(&mut machine), device);
    let pause = machine
        .ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .context()
        .pause_submissions()
        .unwrap();
    let pending = call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, first],
    );
    assert!(matches!(pending, Outcome::Block { .. }));
    super::super::cancel_pending(&mut machine);
    drop(pending);
    drop(pause);
    assert!(machine.ext.get::<State>().pending.is_empty());
    assert!(
        super::super::device(&env(&mut machine), device)
            .unwrap()
            .texture
            .is_none()
    );
    assert!(
        resources::resource(&env(&mut machine), first)
            .unwrap()
            .levels
            .iter()
            .all(|level| level.lock.is_none() && level.dirty.is_some())
    );
    okay(call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, first],
    ));
    okay(call(
        &mut machine,
        "IDirect3DDevice9::SetTexture",
        &[device, 0, 0],
    ));
    assert_eq!(release(&mut env(&mut machine), alias), 1);
    assert_eq!(release(&mut env(&mut machine), first), 0);
    assert_eq!(release(&mut env(&mut machine), second), 0);
    assert_eq!(release(&mut env(&mut machine), ordinary), 0);
    assert!(machine.ext.get::<Surfaces>().targets.is_empty());
    assert_eq!(release(&mut env(&mut machine), device), 0);
}
