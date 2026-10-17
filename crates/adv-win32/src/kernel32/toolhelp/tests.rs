use super::*;
use adv_abi::{AnsiStr, Conv, WideStr};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_os::{Module, ModuleId, ModuleKind, module::Lifetime};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const BAD: u32 = 0x440000;
const MAIN: u32 = 0x500000;
const DLL: u32 = 0x510000;
const MAIN_PATH: &str = "C:\\before\\Main.Exe";
const DLL_PATH: &str = "C:\\before\\資材\\MiXeD日本.Dll";

fn add_module(
    cpu: &mut dyn Guest,
    ext: &mut adv_machine::Ext,
    index: u32,
    kind: ModuleKind,
    published: bool,
) -> ModuleId {
    let base = MAIN + index * 0x10000;
    let ldr = 0x600000 + index * 4096;
    cpu.map(base, 4096, Prot::RX).unwrap();
    let path = match index {
        0 => MAIN_PATH.to_owned(),
        1 => DLL_PATH.to_owned(),
        _ => format!("C:\\before\\Late{index}.Dll"),
    };
    if published {
        cpu.map(ldr, 4096, Prot::RW).unwrap();
        let units: Vec<_> = path.encode_utf16().collect();
        let length = units.len() as u16 * 2;
        cpu.write(ldr + 0x24, &length.to_le_bytes()).unwrap();
        cpu.write(ldr + 0x26, &(length + 2).to_le_bytes()).unwrap();
        cpu.write_u32(ldr + 0x28, ldr + 0x80).unwrap();
        let bytes: Vec<_> = units
            .into_iter()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        cpu.write(ldr + 0x80, &bytes).unwrap();
    }
    adv_os::modules_mut(ext).add(Module {
        name: path.rsplit('\\').next().unwrap().to_lowercase(),
        path: path.rsplit('\\').next().unwrap().to_owned(), // relative load request
        kind,
        base,
        size: 4096,
        is_dll: index != 0,
        entry: 0,
        stack_reserve: 65536,
        exports_by_name: Default::default(),
        exports_by_ordinal: Default::default(),
        resources: None,
        data_file: None,
        load_count: 7,
        ldr_entry: if published { ldr } else { 0 },
        ldr_size: if published { 4096 } else { 0 },
        lifetime: Lifetime {
            mapped: true,
            ..Lifetime::default()
        },
    })
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in [
        "CreateToolhelp32Snapshot",
        "Module32First",
        "Module32Next",
        "Module32FirstW",
        "Module32NextW",
        "Process32First",
        "Process32Next",
        "Process32FirstW",
        "Process32NextW",
        "Heap32ListFirst",
        "Heap32ListNext",
    ] {
        assert_eq!(
            registry.get(K32, name).unwrap().conv,
            Conv::Stdcall { argc: 2 }
        );
    }
    registry.add(
        "toolhelp-fixture.dll",
        "ChangeLoader",
        Conv::Stdcall { argc: 0 },
        |env| {
            env.ext.get_mut::<Win32>().vfs.cwd = "C:\\after".to_owned();
            add_module(env.cpu, env.ext, 5, ModuleKind::Image, true);
            let id = adv_os::modules(env.ext).by_handle(DLL).unwrap();
            adv_os::modules_mut(env.ext).get_mut(id).size = 8192;
            Outcome::Return(0)
        },
    );
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/toolhelp-module-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&dir, "fixture.exe", &dir)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    add_module(&mut m.cpu, &mut m.ext, 0, ModuleKind::Image, true);
    add_module(&mut m.cpu, &mut m.ext, 1, ModuleKind::Image, true);
    add_module(&mut m.cpu, &mut m.ext, 2, ModuleKind::DataFile, true);
    add_module(&mut m.cpu, &mut m.ext, 3, ModuleKind::Image, false);
    // A hand-mapped MZ-looking region has no Loader owner and is never a module.
    m.cpu.map(0x590000, 4096, Prot::RX).unwrap();
    m.cpu.write(0x590000, b"MZ").unwrap();
    m
}

#[test]
fn process_and_heap_snapshots_freeze_owned_state_and_share_aw_cursor_after_write_failures() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    m.spawn("main", CODE, &[], 65536).unwrap();
    let worker = m.spawn("worker", CODE, &[], 65536).unwrap();
    let heap = m
        .ext
        .get_mut::<adv_os::Heaps>()
        .create(&mut m.cpu, &mut m.sys.vm)
        .unwrap();
    let entry = m.resolve_import(K32, "CreateToolhelp32Snapshot").unwrap();
    let handle = returned(create_snapshot(&mut env(&mut m, entry), 15, 0));
    assert_ne!(handle, u32::MAX);
    m.sys.terminate_context_stopped(worker, 0);
    m.ext.get_mut::<Win32>().cfg.os.exe_name = "changed.exe".into();
    assert!(
        m.ext
            .get_mut::<adv_os::Heaps>()
            .destroy(&mut m.cpu, &mut m.sys.vm, heap)
    );
    let snapshot = m.sys.objects.host_mut::<ThreadSnapshot>(handle).unwrap();
    assert_eq!(snapshot.rows.len(), 2);
    assert_eq!(snapshot.processes[0].threads, 2);
    assert_eq!(snapshot.processes[0].name, "fixture.exe");
    assert_eq!(snapshot.heaps.len(), 2);
    snapshot.processes.push(SnapshotProcess {
        pid: 77,
        parent: 12,
        threads: 1,
        name: "other.exe".into(),
    });
    let out = DATA + 256;
    m.cpu.write(out, &[0xcc; PROCESS_W_BYTES]).unwrap();
    m.cpu.write_u32(out, PROCESS_A_BYTES as u32).unwrap();
    assert_eq!(
        returned(process_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            true,
            false
        )),
        1
    );
    assert_eq!(m.cpu.read_u32(out + 20).unwrap(), 2);
    assert_eq!(
        m.cpu.read_u8(out + PROCESS_A_BYTES as u32 - 1).unwrap(),
        0xcc
    );
    m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    assert_eq!(
        returned(process_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            false,
            false
        )),
        0
    );
    assert_eq!(
        m.sys
            .objects
            .host::<ThreadSnapshot>(handle)
            .unwrap()
            .process_cursor,
        0
    );
    m.cpu.protect(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write_u32(out, PROCESS_W_BYTES as u32).unwrap();
    assert_eq!(
        returned(process_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            false,
            true
        )),
        1
    );
    assert_eq!(m.cpu.read_u32(out + 8).unwrap(), 77);
    assert_eq!(
        returned(process_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            true,
            true
        )),
        1
    );
    assert_eq!(m.cpu.read_u32(out + 8).unwrap(), m.sys.pid);
    m.cpu.write_u32(out, 16).unwrap();
    assert_eq!(
        returned(heap_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            true
        )),
        1
    );
    assert_eq!(
        returned(heap_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            false
        )),
        1
    );
    assert_eq!(m.cpu.read_u32(out + 8).unwrap(), heap);
    let fresh = returned(create_snapshot(&mut env(&mut m, entry), 3, 0));
    let snapshot = m.sys.objects.host::<ThreadSnapshot>(fresh).unwrap();
    assert_eq!(snapshot.heaps.len(), 1);
    assert_eq!(snapshot.processes[0].threads, 1);
    assert_eq!(snapshot.processes[0].name, "changed.exe");
}
fn env<B: Backend>(m: &mut Machine<B>, entry: u32) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
    }
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("synchronous Toolhelp return expected"),
    }
}
fn invoke<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import(K32, name).unwrap())
        .mov_mem_eax(result);
}
fn with_handle<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    name: &str,
    stored_handle: u32,
    out: u32,
    result: u32,
) {
    code.push_imm(out).mov_eax_mem(stored_handle).push_eax();
    code.call(m.resolve_import(K32, name).unwrap())
        .mov_mem_eax(result);
}
fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm, peer: bool) {
    code.push_imm(0)
        .call(m.resolve_import(K32, "ExitProcess").unwrap());
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("Toolhelp guest", CODE, &[], 65536).unwrap();
    if peer {
        m.spawn("snapshot peer", PARK_ADDR, &[], 65536).unwrap();
    }
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
fn prefix<B: Backend>(m: &Machine<B>, out: u32) -> [u32; 8] {
    core::array::from_fn(|i| m.cpu.read_u32(out + i as u32 * 4).unwrap())
}
fn astring<B: Backend>(m: &Machine<B>, address: u32, count: usize) -> Vec<u8> {
    AnsiStr(GuestPtr::new(address)).read(&m.cpu, count).unwrap()
}
fn wstring<B: Backend>(m: &Machine<B>, address: u32, count: usize) -> String {
    String::from_utf16(&WideStr(GuestPtr::new(address)).read(&m.cpu, count).unwrap()).unwrap()
}

fn frozen_guest<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let a = DATA + 1024;
    let w = DATA + 2048;
    m.cpu.write(a, &[0xcc; MODULE_A_BYTES + 4]).unwrap();
    m.cpu.write_u32(a, 549).unwrap();
    m.cpu.write(w, &[0xcc; MODULE_W_BYTES]).unwrap();
    m.cpu.write_u32(w, MODULE_W_BYTES as u32).unwrap();
    let mut code = Asm::new(CODE);
    invoke(&mut m, &mut code, "SetLastError", &[0x12345678], DATA + 64);
    let pid = m.sys.pid;
    invoke(
        &mut m,
        &mut code,
        "CreateToolhelp32Snapshot",
        &[8, pid],
        DATA,
    );
    code.call(
        m.resolve_import("toolhelp-fixture.dll", "ChangeLoader")
            .unwrap(),
    );
    // The current getter shares the fixed load-time FullDllName source.
    invoke(
        &mut m,
        &mut code,
        "GetModuleFileNameW",
        &[DLL, DATA + 4096, 260],
        DATA + 4,
    );
    with_handle(&mut m, &mut code, "Module32First", DATA, a, DATA + 8);
    with_handle(&mut m, &mut code, "Module32FirstW", DATA, w, DATA + 12);
    with_handle(&mut m, &mut code, "Module32Next", DATA, a, DATA + 16);
    with_handle(&mut m, &mut code, "Module32NextW", DATA, w, DATA + 20);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 24);
    // FirstW resets only the shared module cursor; NextW then produces DLL row.
    with_handle(&mut m, &mut code, "Module32FirstW", DATA, w, DATA + 28);
    with_handle(&mut m, &mut code, "Module32NextW", DATA, w, DATA + 32);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 36);
    code.mov_eax_mem(DATA)
        .push_eax()
        .call(m.resolve_import(K32, "CloseHandle").unwrap())
        .mov_mem_eax(DATA + 40);
    with_handle(&mut m, &mut code, "Module32FirstW", DATA, w, DATA + 44);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 48);
    run(&mut m, code, false);
    for (at, wanted) in [
        (8, 1),
        (12, 1),
        (16, 1),
        (20, 0),
        (24, 18),
        (28, 1),
        (32, 1),
        (36, 18),
        (40, 1),
        (44, 0),
        (48, 6),
    ] {
        assert_eq!(m.cpu.read_u32(DATA + at).unwrap(), wanted);
    }
    assert_eq!(wstring(&m, DATA + 4096, 260), DLL_PATH);
    assert_eq!(prefix(&m, a), [549, 1, pid, 0xffff, 0xffff, DLL, 4096, DLL]);
    assert_eq!(
        prefix(&m, w),
        [1064, 1, pid, 0xffff, 0xffff, DLL, 4096, DLL]
    );
    let codec = &m.ext.get::<Win32>().codec;
    assert_eq!(
        astring(&m, a + 32, NAME_UNITS),
        codec.encode("MiXeD日本.Dll")
    );
    assert_eq!(astring(&m, a + 288, PATH_UNITS), codec.encode(DLL_PATH));
    assert_eq!(wstring(&m, w + 32, NAME_UNITS), "MiXeD日本.Dll");
    assert_eq!(wstring(&m, w + 544, PATH_UNITS), DLL_PATH);
    assert_eq!(m.cpu.read_u8(a + 547).unwrap(), 0xcc);
    assert_eq!(
        m.cpu.read_u32(a + MODULE_A_BYTES as u32).unwrap(),
        0xcccccccc
    );
    assert!(
        m.sys
            .objects
            .host::<ThreadSnapshot>(m.cpu.read_u32(DATA).unwrap())
            .is_none()
    );
    assert_eq!(
        adv_os::modules(&m.ext)
            .get(adv_os::modules(&m.ext).by_handle(DLL).unwrap())
            .size,
        8192
    );
}
#[test]
fn toolhelp_module_guest_aw_cursors_loader_freeze_paths_and_close_are_shared() {
    frozen_guest(UnicornCpu::new(PARK_ADDR).unwrap());
    frozen_guest(JitCpu::new(PARK_ADDR, false).unwrap());
}

#[test]
fn toolhelp_module_native_sizes_atomic_outputs_and_failed_writes_preserve_cursor() {
    // Microsoft x86 SDK probe source SHA1DE92C83...99872534; A First size<548
    // gives87, A Next gives13; W size!=1064 gives24; all leave dwSize intact.
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    m.spawn("output guards", PARK_ADDR, &[], 65536).unwrap();
    let entry = m.resolve_import(K32, "CreateToolhelp32Snapshot").unwrap();
    let pid = m.sys.pid;
    let handle = returned(create_snapshot(&mut env(&mut m, entry), 8, pid));
    m.cpu.map(BAD, 4096, Prot::RW).unwrap();
    let mut sentinel = vec![0xcc; MODULE_W_BYTES];
    for (wide, size, first, error) in [
        (false, 547u32, true, 87),
        (false, 547, false, 13),
        (true, 1063, true, 24),
        (true, 1065, false, 24),
    ] {
        sentinel[..4].copy_from_slice(&size.to_le_bytes());
        m.cpu.write(BAD, &sentinel).unwrap();
        assert_eq!(
            returned(module_entry(
                &mut env(&mut m, entry),
                handle,
                GuestPtr::new(BAD),
                first,
                wide
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, entry)), error);
        let mut actual = vec![0; sentinel.len()];
        m.cpu.read(BAD, &mut actual).unwrap();
        assert_eq!(actual, sentinel);
        assert_eq!(
            m.sys
                .objects
                .host::<ThreadSnapshot>(handle)
                .unwrap()
                .module_cursor,
            0
        );
    }
    for (out, len, wide) in [(BAD, 1064, true), (BAD + 4096 - 16, 548, false)] {
        let mut before = vec![0xcc; if out == BAD { len } else { 16 }];
        before[..4].copy_from_slice(&(len as u32).to_le_bytes());
        m.cpu.write(out, &before).unwrap();
        if out == BAD {
            m.cpu.protect(BAD, 4096, Prot::READ).unwrap();
        }
        assert_eq!(
            returned(module_entry(
                &mut env(&mut m, entry),
                handle,
                GuestPtr::new(out),
                false,
                wide
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, entry)), 998);
        let mut actual = vec![0; before.len()];
        m.cpu.read(out, &mut actual).unwrap();
        assert_eq!(actual, before);
        assert_eq!(
            m.sys
                .objects
                .host::<ThreadSnapshot>(handle)
                .unwrap()
                .module_cursor,
            0
        );
        m.cpu.protect(BAD, 4096, Prot::RW).unwrap();
    }
    // Only dwSize must be readable; the rest may have WRITE without READ.
    m.cpu.map(BAD + 4096, 4096, Prot::WRITE).unwrap();
    let out = BAD + 4096 - 4;
    m.cpu.write_u32(out, 548).unwrap();
    m.cpu.write_u32(out + 8, 0x12344321).unwrap();
    assert_eq!(
        returned(module_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(out),
            false,
            false
        )),
        1
    );
    assert_eq!(m.cpu.read_u32(out + 20).unwrap(), DLL);
    assert_eq!(m.cpu.read_u32(out + 8).unwrap(), 0x12344321); // native A Next leaves PID
    assert_eq!(
        m.sys
            .objects
            .host::<ThreadSnapshot>(handle)
            .unwrap()
            .module_cursor,
        1
    );
    m.cpu.write_u32(DATA + 512, 1064).unwrap();
    let before = prefix(&m, DATA + 512);
    assert_eq!(
        returned(module_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(DATA + 512),
            false,
            true
        )),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut m, entry)), 18);
    assert_eq!(prefix(&m, DATA + 512), before);
    // Malformed guest LDR pointers must not escape their module's owner span.
    m.cpu.write_u32(0x600000 + 0x28, DATA).unwrap();
    assert_eq!(
        returned(create_snapshot(&mut env(&mut m, entry), 8, pid)),
        u32::MAX
    );
    assert_eq!(crate::util::last_error(&env(&mut m, entry)), 24);
}

#[test]
fn toolhelp_flags_empty_native_outputs_threads_and_unsupported_profiles_are_explicit() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let a = DATA + 1024;
    let w = DATA + 2048;
    m.cpu.write(a, &[0xcc; MODULE_A_BYTES]).unwrap();
    m.cpu.write_u32(a, 548).unwrap();
    m.cpu.write(w, &[0xcc; MODULE_W_BYTES]).unwrap();
    m.cpu.write_u32(w, 1064).unwrap();
    m.cpu.write_u32(DATA + 4096, 28).unwrap();
    m.cpu.write_u32(DATA + 4224, 28).unwrap();
    let mut code = Asm::new(CODE);
    invoke(
        &mut m,
        &mut code,
        "CreateToolhelp32Snapshot",
        &[4 | 8 | 16, 0],
        DATA,
    );
    with_handle(&mut m, &mut code, "Module32FirstW", DATA, w, DATA + 4);
    with_handle(
        &mut m,
        &mut code,
        "Thread32First",
        DATA,
        DATA + 4096,
        DATA + 8,
    );
    with_handle(&mut m, &mut code, "Module32NextW", DATA, w, DATA + 12);
    with_handle(
        &mut m,
        &mut code,
        "Thread32Next",
        DATA,
        DATA + 4224,
        DATA + 16,
    );
    with_handle(&mut m, &mut code, "Module32NextW", DATA, w, DATA + 20);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 24);
    invoke(
        &mut m,
        &mut code,
        "CreateToolhelp32Snapshot",
        &[16, 0],
        DATA + 32,
    );
    with_handle(&mut m, &mut code, "Module32First", DATA + 32, a, DATA + 36);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 40);
    with_handle(&mut m, &mut code, "Module32FirstW", DATA + 32, w, DATA + 44);
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 48);
    invoke(
        &mut m,
        &mut code,
        "CreateToolhelp32Snapshot",
        &[0, 0xdead],
        DATA + 52,
    );
    with_handle(
        &mut m,
        &mut code,
        "Thread32First",
        DATA + 52,
        DATA + 4096,
        DATA + 56,
    );
    invoke(&mut m, &mut code, "GetLastError", &[], DATA + 60);
    run(&mut m, code, true);
    for (at, wanted) in [
        (4, 1),
        (8, 1),
        (12, 1),
        (16, 1),
        (20, 0),
        (24, 18),
        (36, 0),
        (40, 18),
        (44, 0),
        (48, 18),
        (56, 0),
        (60, 18),
    ] {
        assert_eq!(m.cpu.read_u32(DATA + at).unwrap(), wanted);
    }
    let combined = m
        .sys
        .objects
        .host::<ThreadSnapshot>(m.cpu.read_u32(DATA).unwrap())
        .unwrap();
    assert_eq!(combined.modules.len(), 2);
    assert_eq!(combined.rows.len(), 2);
    assert_eq!(combined.cursor, 2);
    assert_eq!(combined.module_cursor, 1);
    assert_eq!(prefix(&m, a), [548, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(m.cpu.read_u8(a + 32).unwrap(), 0);
    assert_eq!(m.cpu.read_u8(a + 33).unwrap(), 0xcc);
    assert_eq!(m.cpu.read_u8(a + 288).unwrap(), 0);
    assert_eq!(m.cpu.read_u8(a + 289).unwrap(), 0xcc);
    let mut bytes = [1u8; MODULE_W_BYTES - 4];
    m.cpu.read(w + 4, &mut bytes).unwrap();
    assert_eq!(bytes, [0; MODULE_W_BYTES - 4]);
    assert!(
        m.sys
            .objects
            .host::<ThreadSnapshot>(m.cpu.read_u32(DATA + 52).unwrap())
            .is_some()
    );
    let entry = m.resolve_import(K32, "CreateToolhelp32Snapshot").unwrap();
    for (flags, pid) in [(0x80000008, 0), (0x20, 0), (8, 0xdead)] {
        assert_eq!(
            returned(create_snapshot(&mut env(&mut m, entry), flags, pid)),
            u32::MAX
        );
        assert_eq!(crate::util::last_error(&env(&mut m, entry)), 50);
    }
    let thread_only = returned(create_snapshot(&mut env(&mut m, entry), 4, 0xdead));
    assert_ne!(thread_only, u32::MAX); // thread-only ignores the process argument
    let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 2);
    assert_eq!(diagnostics.warnings.values().sum::<u64>(), 3);
    // UTF8 clipping is in bytes and retains complete scalars at both fixed
    // buffer limits. W remains available when the ANSI encoder is unsupported.
    m.ext.get_mut::<Win32>().codec = adv_os::Codec::new(65001);
    let handle = m.cpu.read_u32(DATA).unwrap();
    {
        let row = &mut m
            .sys
            .objects
            .host_mut::<ThreadSnapshot>(handle)
            .unwrap()
            .modules[0];
        row.name = "é".repeat(128);
        row.path = "é".repeat(130);
    }
    m.cpu.write(a, &[0xcc; MODULE_A_BYTES]).unwrap();
    m.cpu.write_u32(a, 548).unwrap();
    assert_eq!(
        returned(module_entry(
            &mut env(&mut m, entry),
            handle,
            GuestPtr::new(a),
            true,
            false
        )),
        1
    );
    assert_eq!(astring(&m, a + 32, NAME_UNITS), "é".repeat(127).as_bytes());
    assert_eq!(astring(&m, a + 288, PATH_UNITS), "é".repeat(129).as_bytes());
    assert_eq!(m.cpu.read_u8(a + 547).unwrap(), 0xcc);
    for cp in [437, 20127] {
        m.ext.get_mut::<Win32>().codec = adv_os::Codec::new(cp);
        let before = prefix(&m, a);
        assert_eq!(
            returned(module_entry(
                &mut env(&mut m, entry),
                handle,
                GuestPtr::new(a),
                true,
                false
            )),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut m, entry)), 50);
        assert_eq!(prefix(&m, a), before);
        assert_eq!(
            returned(module_entry(
                &mut env(&mut m, entry),
                handle,
                GuestPtr::new(w),
                true,
                true
            )),
            1
        );
        assert_eq!(wstring(&m, w + 32, NAME_UNITS), "é".repeat(128));
    }
}

#[test]
// spec: PERS-BRIDGE-3
fn k32_enum_process_modules_and_file_name() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    m.spawn("main", CODE, &[], 65536).unwrap();
    let entry = m.resolve_import(K32, "K32EnumProcessModules").unwrap();

    let stack = 0x5a0000;
    m.cpu.map(stack, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, stack);
    m.cpu.write_u32(stack + 4, u32::MAX).unwrap();
    m.cpu.write_u32(stack + 8, DATA).unwrap();
    m.cpu.write_u32(stack + 12, 1024).unwrap();
    m.cpu.write_u32(stack + 16, DATA + 512).unwrap();
    let mut e = env(&mut m, entry);
    let outcome = crate::kernel32::module::k32_enum_process_modules(&mut e);
    assert_eq!(returned(outcome), 1);
    let needed = m.cpu.read_u32(DATA + 512).unwrap();
    assert!(needed >= 8);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), MAIN);
    assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), DLL);

    m.cpu.write_u32(stack + 4, u32::MAX).unwrap();
    m.cpu.write_u32(stack + 8, MAIN).unwrap();
    m.cpu.write_u32(stack + 12, DATA + 1024).unwrap();
    m.cpu.write_u32(stack + 16, 260).unwrap();
    let mut e = env(&mut m, entry);
    let outcome = crate::kernel32::module::k32_get_module_file_name_ex(&mut e, false);
    assert!(returned(outcome) > 0);
    assert_eq!(astring(&m, DATA + 1024, 260), MAIN_PATH.as_bytes());

    m.cpu.write_u32(stack + 4, u32::MAX).unwrap();
    m.cpu.write_u32(stack + 8, MAIN).unwrap();
    m.cpu.write_u32(stack + 12, DATA + 1024).unwrap();
    m.cpu.write_u32(stack + 16, 260).unwrap();
    let mut e = env(&mut m, entry);
    let outcome = crate::kernel32::module::k32_get_module_file_name_ex(&mut e, true);
    assert!(returned(outcome) > 0);
    assert_eq!(wstring(&m, DATA + 1024, 260), MAIN_PATH);

    m.cpu.write_u32(stack + 4, 0x1234).unwrap();
    let mut e = env(&mut m, entry);
    let outcome = crate::kernel32::module::k32_enum_process_modules(&mut e);
    assert_eq!(returned(outcome), 0);
    assert_eq!(crate::util::last_error(&e), 6);
}
