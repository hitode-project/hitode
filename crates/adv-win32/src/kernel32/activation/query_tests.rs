//! Authored default-scope query contracts, including manifest refusal.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Module, ModuleKind, module::Lifetime};
use adv_pe::{ResId, ResourceDir, ResourceEntry, ResourceNode};
use adv_unicorn::UnicornCpu;

const DATA: u32 = 0x410000;
const SENTINEL: u32 = 0xcccccccc;
const ERROR: u32 = 0x12345678;

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.dynamic_available("kernel32.dll", "QueryActCtxW"));
    assert_eq!(
        registry.get("kernel32.dll", "QueryActCtxW").unwrap().conv,
        Conv::Stdcall { argc: 7 }
    );
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn reset(env: &mut Env<'_>) {
    env.cpu.write(DATA, &[0xcc; 128]).unwrap();
    crate::util::set_last_error(env, ERROR);
}
fn invoke(env: &mut Env<'_>, flags: u32, class: u32, bytes: u32) -> i32 {
    query(
        env,
        flags,
        Handle(0),
        GuestPtr::new(0),
        class,
        GuestPtr::new(DATA),
        bytes,
        GuestPtr::new(DATA + 120),
    )
}

#[test]
fn default_query_sizes_payload_and_errors_match_native_x86() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("default query", PARK_ADDR, &[], 65536)
        .unwrap();
    let mut env = env(&mut machine);
    for (class, size) in [(1, 8), (2, 48), (5, 12), (6, 0)] {
        for flags in [0, 1, 4, 5, 0x80000000, 0x80000004] {
            reset(&mut env);
            let invalid = class == 2 && flags & 0x80000000 != 0;
            assert_eq!(invoke(&mut env, flags, class, 100), i32::from(!invalid));
            assert_eq!(
                crate::util::last_error(&env),
                if invalid { 87 } else { ERROR }
            );
            assert_eq!(
                env.cpu.read_u32(DATA + 120).unwrap(),
                if invalid { 0 } else { size }
            );
            let mut payload = [0; 100];
            env.cpu.read(DATA, &mut payload).unwrap();
            let prefix = if invalid { 0 } else { size as usize };
            assert!(payload[..prefix].iter().all(|byte| *byte == 0));
            assert!(payload[prefix..].iter().all(|byte| *byte == 0xcc));
        }
        for bytes in [0, 1, 4, 7, 8, 12, 47, 48] {
            reset(&mut env);
            assert_eq!(invoke(&mut env, 0, class, bytes), i32::from(bytes >= size));
            assert_eq!(
                crate::util::last_error(&env),
                if bytes < size { 122 } else { ERROR }
            );
            assert_eq!(
                env.cpu.read_u32(DATA + 120).unwrap(),
                if bytes < size && class == 2 { 0 } else { size }
            );
            if bytes < size || class == 6 {
                assert_eq!(env.cpu.read_u32(DATA).unwrap(), SENTINEL);
            }
        }
        reset(&mut env);
        assert_eq!(
            query(
                &mut env,
                0,
                Handle(0),
                GuestPtr::new(0),
                class,
                GuestPtr::new(0),
                0,
                GuestPtr::new(DATA + 120)
            ),
            i32::from(class == 6)
        );
        assert_eq!(
            query(
                &mut env,
                0,
                Handle(0),
                GuestPtr::new(0),
                class,
                GuestPtr::new(DATA),
                100,
                GuestPtr::new(0)
            ),
            1
        );
    }
    assert!(
        env.ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn invalid_parameters_and_guest_writes_do_not_fabricate_payloads() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("query errors", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.map(0x420000, 4096, Prot::READ).unwrap();
    let mut env = env(&mut machine);
    for (flags, handle, sub, class, output, bytes, required, expected) in [
        (2, 0, 0, 1, DATA, 100, DATA + 120, 87),
        (12, 0, 0, 1, DATA, 100, DATA + 120, 87),
        (4, 1, 0, 1, DATA, 100, DATA + 120, 87),
        (8, 0, 0, 1, DATA, 100, DATA + 120, 87),
        (0, 0, 1, 5, DATA, 100, DATA + 120, 87),
        (0, 0, 1, 6, DATA, 100, DATA + 120, 87),
        (0, 0, 0, 0, DATA, 100, DATA + 120, 87),
        (0, 0, 0, 3, DATA, 100, DATA + 120, 87),
        (0, 0, 0, 7, DATA, 100, DATA + 120, 87),
        (0, 0, 0, 1, 0, 8, DATA + 120, 87),
        (0, 0, 0, 1, 0, 0, 0, 87),
        (0, 0, 0, 1, DATA, 8, 0x420000, 998),
        (0, 0, 0, 1, 0x420000, 8, DATA + 120, 998),
        (0, 0, 0, 2, DATA + 4090, 48, DATA + 120, 998),
    ] {
        reset(&mut env);
        assert_eq!(
            query(
                &mut env,
                flags,
                Handle(handle),
                GuestPtr::new(sub),
                class,
                GuestPtr::new(output),
                bytes,
                GuestPtr::new(required)
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env), expected);
        assert_eq!(env.cpu.read_u32(DATA).unwrap(), SENTINEL);
    }
    // Only the actual structure prefix needs writable guest storage.
    assert_eq!(
        query(
            &mut env,
            0,
            Handle(0),
            GuestPtr::new(1),
            1,
            GuestPtr::new(DATA + 4088),
            u32::MAX,
            GuestPtr::new(0)
        ),
        1
    );
    // The empty compatibility scope has no bytes to write, even to a bad pointer.
    assert_eq!(
        query(
            &mut env,
            0,
            Handle(0),
            GuestPtr::new(0),
            6,
            GuestPtr::new(1),
            100,
            GuestPtr::new(DATA + 120)
        ),
        1
    );
}

fn add_module(machine: &mut Machine<JitCpu>, base: u32, manifest: bool) -> usize {
    let resources = manifest.then(|| ResourceDir {
        entries: vec![ResourceEntry {
            id: ResId::Id(24),
            node: ResourceNode::Dir(ResourceDir::default()),
        }],
    });
    adv_os::modules_mut(&mut machine.ext).add(Module {
        name: format!("authored-{base}.exe"),
        path: "C:\\Game\\authored.exe".into(),
        kind: ModuleKind::Image,
        base,
        size: 4096,
        is_dll: false,
        entry: base,
        stack_reserve: 65536,
        exports_by_name: Default::default(),
        exports_by_ordinal: Default::default(),
        resources,
        data_file: None,
        load_count: 1,
        ldr_entry: 0,
        ldr_size: 0,
        lifetime: Lifetime {
            mapped: true,
            ..Lifetime::default()
        },
    })
}

#[test]
fn module_selectors_and_manifest_scopes_never_invent_sxs_information() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("query module", PARK_ADDR, &[], 65536)
        .unwrap();
    let main = add_module(&mut machine, 0x500000, false);
    let dll = add_module(&mut machine, 0x510000, true);
    machine
        .cpu
        .write_u32(
            adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::IMAGE_BASE,
            0x500000,
        )
        .unwrap();
    for (flags, handle, good) in [
        (0, 0, true),
        (8, 0x500000, true),
        (16, 0x500001, true),
        (8, 0x510000, false),
        (16, 0x510001, false),
    ] {
        let mut env = env(&mut machine);
        reset(&mut env);
        assert_eq!(
            query(
                &mut env,
                flags,
                Handle(handle),
                GuestPtr::new(0),
                2,
                GuestPtr::new(DATA),
                48,
                GuestPtr::new(DATA + 120)
            ),
            i32::from(good)
        );
        assert_eq!(crate::util::last_error(&env), if good { ERROR } else { 50 });
        if !good {
            assert_eq!(env.cpu.read_u32(DATA).unwrap(), SENTINEL);
        }
    }
    let resources = adv_os::modules(&machine.ext).get(dll).resources.clone();
    adv_os::modules_mut(&mut machine.ext)
        .get_mut(main)
        .resources = resources;
    for flags in [0, 1, 4, 5] {
        let mut env = env(&mut machine);
        reset(&mut env);
        assert_eq!(current(&mut env, GuestPtr::new(DATA + 116)), 1);
        assert_eq!(env.cpu.read_u32(DATA + 116).unwrap(), 0);
        assert_eq!(activate(&mut env, Handle(0), GuestPtr::new(DATA + 112)), 1);
        for class in [1, 2, 5, 6] {
            assert_eq!(invoke(&mut env, flags, class, 100), 0);
            assert_eq!(crate::util::last_error(&env), 50);
            assert_eq!(env.cpu.read_u32(DATA).unwrap(), SENTINEL);
            assert_eq!(env.cpu.read_u32(DATA + 120).unwrap(), 0);
        }
    }
    let modules = adv_os::modules(&machine.ext);
    assert_eq!(modules.get(main).lifetime.external_refs, 1);
    assert_eq!(modules.get(dll).lifetime.external_refs, 1);
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.structural_mismatches, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
}

fn public_front<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
    let target = machine
        .resolve_import("kernel32.dll", "QueryActCtxW")
        .unwrap();
    let mut code = Asm::new(0x400000);
    for arg in [DATA + 120, 48, DATA, 2, 0, 0, 0] {
        code.push_imm(arg);
    }
    code.call(target).mov_mem_eax(DATA + 124);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(0x400000, &code.bytes).unwrap();
    machine
        .spawn("public QueryActCtxW", 0x400000, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 124).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 120).unwrap(), 48);
}
#[test]
fn public_query_front_uses_the_seven_argument_stdcall_contract() {
    public_front(JitCpu::new(PARK_ADDR, false).unwrap());
    public_front(UnicornCpu::new(PARK_ADDR).unwrap());
}
