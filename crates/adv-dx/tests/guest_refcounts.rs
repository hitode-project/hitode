//! Every registered reference thunk executes against the same host COM owner.
//! Compare original handlers, guest vtables, internal bindings and guest patches.
use adv_abi::{Conv, Env, GuestCode, Outcome, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::com::Com;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TABLE: u32 = DATA + 0x100;
const OUT: u32 = DATA + 0x200;
struct State {
    com: Com,
    destroyed: u32,
}
fn handler(env: &mut Env<'_>) -> Outcome {
    let com = env.ext.get::<State>().com;
    let pointer = env.arg(0);
    if env.sys.thunks.get(env.thunk).name.ends_with("::AddRef") {
        Outcome::Return(
            com.add_ref::<u32>(&mut env.sys.objects, pointer)
                .unwrap_or(0),
        )
    } else {
        let Some(result) = com.release::<u32>(&mut env.sys.objects, pointer) else {
            return Outcome::Return(0);
        };
        if result.destroyed.is_some() {
            env.ext.get_mut::<State>().destroyed += 1;
        }
        Outcome::Return(result.references)
    }
}
fn call(code: &mut Asm, slot: u8, output: u32) {
    call_pointer(code, slot, output, DATA);
}
fn call_pointer(code: &mut Asm, slot: u8, output: u32, pointer: u32) {
    code.push_imm(pointer);
    // mov eax,[DATA]; call dword ptr [eax+slot]
    code.bytes.push(0xa1);
    code.bytes.extend_from_slice(&pointer.to_le_bytes());
    code.bytes.extend_from_slice(&[0xff, 0x50, slot]);
    code.mov_mem_eax(output);
}
fn run(
    module: &str,
    add: &str,
    release: &str,
    bodies: [&'static GuestCode; 2],
    guest: bool,
    internal: bool,
    native: bool,
) -> (Vec<u32>, u64) {
    let mut registry = Registry::default();
    for (name, body) in [(add, bodies[0]), (release, bodies[1])] {
        registry.add(module, name, Conv::Stdcall { argc: 1 }, handler);
        registry.set_guest(module, name, body);
    }
    let mut cpu = JitCpu::new(PARK_ADDR, native).unwrap();
    cpu.set_async_compilation(false);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.sys.thunks.set_guest_code(&mut m.cpu, guest).unwrap();
    let add_target = m.resolve_import(module, add).unwrap();
    let release_target = m.resolve_import(module, release).unwrap();
    m.cpu.write_u32(DATA, TABLE).unwrap();
    m.cpu.write_u32(DATA + 16, TABLE).unwrap();
    m.cpu.write_u32(TABLE + 4, add_target).unwrap();
    m.cpu.write_u32(TABLE + 8, release_target).unwrap();
    let context = m.spawn("reference caller", CODE, &[], 65536).unwrap();
    m.sys.current = context;
    let com = Com::new(&mut m.sys.objects).unwrap();
    let id = com.create(&mut m.sys.objects, DATA, [1; 16], 7u32).unwrap();
    assert!(com.register_interface(&mut m.sys.objects, id, [2; 16], DATA + 16));
    m.ext.insert(State { com, destroyed: 0 });
    if guest {
        let mut env = Env {
            context,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        assert!(com.enable_guest(&mut env, id));
    }
    assert_eq!(
        com.query::<u32>(&mut m.sys.objects, DATA, [2; 16]).unwrap(),
        DATA + 16
    );
    assert_eq!(com.references(&m.sys.objects, id), Some(2));
    assert_eq!(
        com.release::<u32>(&mut m.sys.objects, DATA + 16)
            .unwrap()
            .references,
        1
    );
    if internal {
        assert!(com.retain_internal(&mut m.sys.objects, id));
    }
    let mut code = Asm::new(CODE);
    code.bytes
        .extend_from_slice(&[0x64, 0xc7, 0x05, 0x34, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]);
    code.mov_ecx_imm(300);
    let again = code.here();
    code.push_ecx();
    call_pointer(&mut code, 4, OUT, DATA + 16);
    call(&mut code, 8, OUT + 4);
    code.pop_ecx().dec_ecx().jnz(again);
    // A caller-owned vtable patch must remain observable.
    code.bytes.extend_from_slice(&[0xc7, 0x05]);
    code.bytes.extend_from_slice(&(TABLE + 4).to_le_bytes());
    code.bytes.extend_from_slice(&(CODE + 0x800).to_le_bytes());
    call(&mut code, 4, OUT + 8);
    code.bytes.extend_from_slice(&[0xc7, 0x05]);
    code.bytes.extend_from_slice(&(TABLE + 4).to_le_bytes());
    code.bytes.extend_from_slice(&add_target.to_le_bytes());
    call(&mut code, 8, OUT + 12);
    if internal {
        call(&mut code, 4, OUT + 16); // Public zero-to-one edge must reach the host.
        call(&mut code, 8, OUT + 20);
    }
    code.bytes.extend_from_slice(&[0x64, 0xa1, 0x34, 0, 0, 0]);
    code.mov_mem_eax(OUT + 24);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu
        .write(CODE + 0x800, &[0xb8, 0x5a, 0, 0, 0, 0xc2, 4, 0])
        .unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    if native {
        assert!(m.cpu.stats().native_entries > 0);
    }
    let values: Vec<_> = (0..7)
        .map(|i| m.cpu.read_u32(OUT + i * 4).unwrap())
        .collect();
    assert_eq!(&values[..4], &[2, 1, 0x5a, 0]);
    assert_eq!(values[6], 0x12345678);
    if internal {
        assert_eq!(&values[4..6], &[1, 0]);
        assert_eq!(m.ext.get::<State>().destroyed, 0);
        assert_eq!(com.references(&m.sys.objects, id), Some(0));
        assert_eq!(
            com.release_internal(&mut m.sys.objects, id)
                .unwrap()
                .unwrap()
                .value,
            7
        );
    } else {
        assert_eq!(m.ext.get::<State>().destroyed, 1);
    }
    assert!(com.get::<u32>(&m.sys.objects, DATA).is_none());
    if guest && ["dsound.dll", "quartz.dll"].contains(&module) {
        assert!(m.sys.audio_called_after(context, u64::MAX));
    }
    (values, m.sys.host_calls)
}

// spec: PROC-MACH-8, PERS-EXPORT-7
#[test]
fn every_reference_vtable_matches_host_and_keeps_internal_owners_and_live_patches() {
    let mut registry = Registry::default();
    adv_dx::ddraw::register(&mut registry);
    adv_dx::dsound::register(&mut registry);
    adv_dx::dshow::register(&mut registry);
    #[cfg(feature = "gpu")]
    adv_dx::d3d9::register(&mut registry);
    for native in [false, true] {
        for (module, name, binding) in registry
            .registered()
            .filter(|(_, n, _)| n.ends_with("::AddRef"))
        {
            let release = name.replace("::AddRef", "::Release");
            let bodies = [
                binding.guest.expect("AddRef guest"),
                registry
                    .get(module, &release)
                    .unwrap()
                    .guest
                    .expect("Release guest"),
            ];
            for internal in [false, true] {
                let (actual, calls) = run(module, name, &release, bodies, true, internal, native);
                let (expected, ordinary) =
                    run(module, name, &release, bodies, false, internal, native);
                assert_eq!(actual, expected, "{module}!{name}");
                assert_eq!(
                    ordinary - calls,
                    600,
                    "all above-zero calls stay in guest code"
                );
            }
        }
    }
}

// spec: PROC-MACH-8, PERS-SCOPE-4
#[test]
fn shared_public_count_limits_refuse_extra_owners_and_aliases_without_mutation() {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let context = m.spawn("count limit", PARK_ADDR, &[], 65536).unwrap();
    let com = Com::new(&mut m.sys.objects).unwrap();
    let id = com.create(&mut m.sys.objects, DATA, [1; 16], 7u32).unwrap();
    let mut env = Env {
        context,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    assert!(com.enable_guest(&mut env, id));
    let address = m.cpu.read_u32(DATA + 4).unwrap();
    let limit = m.cpu.read_u32(address + 4).unwrap();
    m.cpu.write_u32(address, limit).unwrap();
    let owners = m.sys.objects.strong_count(id);
    assert_eq!(com.add_ref::<u32>(&mut m.sys.objects, DATA), None);
    assert!(!com.retain_internal(&mut m.sys.objects, id));
    assert!(!com.register_interface(&mut m.sys.objects, id, [2; 16], DATA + 16));
    assert_eq!(com.references(&m.sys.objects, id), Some(limit));
    assert_eq!(m.sys.objects.strong_count(id), owners);
    assert_eq!(
        com.release::<u32>(&mut m.sys.objects, DATA)
            .unwrap()
            .references,
        limit - 1
    );
    assert_eq!(m.cpu.read_u32(address).unwrap(), limit - 1);
    m.cpu.write_u32(address, 1).unwrap();
    assert_eq!(
        com.release::<u32>(&mut m.sys.objects, DATA)
            .unwrap()
            .destroyed
            .unwrap()
            .value,
        7
    );
}
