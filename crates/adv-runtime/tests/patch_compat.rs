//! Repository-owned PE patches are validated natively, then on JIT, interpreter and UC.
#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::RunResult;
use adv_runtime::boot::Runtime;
#[path = "support/cpu.rs"]
mod backend;

fn named_export(cpu: &dyn adv_cpu::backend::Guest, base: u32, name: &[u8]) -> u32 {
    use adv_cpu::backend::GuestExt;
    let pe = base + cpu.read_u32(base + 0x3c).unwrap();
    let directory = base + cpu.read_u32(pe + 24 + 96).unwrap();
    let count = cpu.read_u32(directory + 24).unwrap();
    let functions = base + cpu.read_u32(directory + 28).unwrap();
    let names = base + cpu.read_u32(directory + 32).unwrap();
    let ordinals = base + cpu.read_u32(directory + 36).unwrap();
    for i in 0..count {
        let text = base + cpu.read_u32(names + i * 4).unwrap();
        if cpu.read_cstr(text, 256).unwrap() == name {
            let ordinal = u32::from(cpu.read_u16(ordinals + i * 2).unwrap());
            return base + cpu.read_u32(functions + ordinal * 4).unwrap();
        }
    }
    panic!("missing fixture export {:?}", name);
}

const GOLDEN: &str = "iat=41\nminhook=16,trampoline=original,stdcall=balanced\nminhook-restored=1\ncom-vtable=QueryInterface,original=1\ndllmain-code=17->33\ndllmain-stack=33->44->33->17,unloaded=1\n";

fn fixtures() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/fixtures-patch-tests");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-patch.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    directory
}

#[test]
fn relocated_api_thunks_preserve_stack_registers_callbacks_and_float_returns() {
    use adv_abi::Registry;
    use adv_cpu::backend::asm::Asm;
    use adv_cpu::backend::{Guest, GuestExt, Prot};
    use adv_machine::layout::THUNK_STRIDE;
    use adv_machine::{Clock, Conv, Machine, Outcome, Wait};
    const CODE: u32 = 0x0040_0000;
    const RELOCATED: u32 = CODE + 0x400;
    const CALLBACK: u32 = CODE + 0x500;
    const DATA: u32 = CODE + 0x800;
    for backend in ["jit", "interpreter", "unicorn"] {
        let mut registry = Registry::default();
        registry.add("probe.dll", "sum", Conv::Stdcall { argc: 2 }, |e| {
            *e.ext.get_mut::<u32>() += 1;
            Outcome::Return(e.arg(0) + e.arg(1))
        });
        registry.add("probe.dll", "callback", Conv::Stdcall { argc: 2 }, |e| {
            Outcome::call(CALLBACK, vec![e.arg(0), e.arg(1)], |_, value| {
                let adv_machine::Resume::Returned(value) = value else {
                    unreachable!()
                };
                Outcome::Return(value + 1)
            })
        });
        registry.add("probe.dll", "float", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ReturnF32(1.25)
        });
        registry.add("probe.dll", "wait", Conv::Stdcall { argc: 0 }, |e| {
            Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 100), 42)
        });
        registry.add("probe.dll", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut m = Machine::new(
            backend::Cpu::new(backend).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.ext.insert(0u32);
        m.cpu.map(CODE, 4096, Prot::RWX).unwrap();
        let sum = m.resolve_import("probe.dll", "sum").unwrap();
        let callback = m.resolve_import("probe.dll", "callback").unwrap();
        let float = m.resolve_import("probe.dll", "float").unwrap();
        let wait = m.resolve_import("probe.dll", "wait").unwrap();
        let done = m.resolve_import("probe.dll", "done").unwrap();
        let mut bytes = vec![0; THUNK_STRIDE as usize];
        m.cpu.read(sum, &mut bytes).unwrap();
        m.cpu.write(RELOCATED, &bytes).unwrap();
        // Overwriting the API address with a guest replacement must bypass its handler;
        // only the later call into the copied push-token/int sequence dispatches it.
        let mut replacement = Asm::new(CODE + 0x600);
        replacement
            .bytes
            .extend_from_slice(&[0xb8, 0xe7, 0x03, 0, 0]); // mov eax,999
        replacement.ret_n(8);
        m.cpu.write(CODE + 0x600, &replacement.bytes).unwrap();
        let mut patch = Asm::new(sum);
        patch.jmp(CODE + 0x600);
        m.cpu.write(sum, &patch.bytes).unwrap();
        let mut a = Asm::new(CODE);
        a.bytes
            .extend_from_slice(&[0xbd, 0x78, 0x56, 0x34, 0x12, 0xbf, 0x98, 0xba, 0xdc, 0xfe]);
        a.bytes.extend_from_slice(&[0x89, 0x25]);
        a.bytes.extend_from_slice(&DATA.to_le_bytes()); // mov [DATA],esp
        a.push_imm(5).push_imm(7).call(sum).mov_mem_eax(DATA + 32);
        a.push_imm(5)
            .push_imm(7)
            .call(RELOCATED)
            .mov_mem_eax(DATA + 4);
        a.push_imm(5)
            .push_imm(7)
            .call(callback)
            .mov_mem_eax(DATA + 8);
        a.call(float);
        a.bytes.extend_from_slice(&[0xd9, 0x1d]);
        a.bytes.extend_from_slice(&(DATA + 12).to_le_bytes()); // fstp dword [DATA+12]
        a.call(wait).mov_mem_eax(DATA + 16);
        for (opcode, address) in [(0x25, DATA + 20), (0x2d, DATA + 24), (0x3d, DATA + 28)] {
            a.bytes.extend_from_slice(&[0x89, opcode]);
            a.bytes.extend_from_slice(&address.to_le_bytes());
        }
        a.call(done);
        m.cpu.write(CODE, &a.bytes).unwrap();
        let mut a = Asm::new(CALLBACK);
        a.mov_eax_esp(4);
        a.bytes.extend_from_slice(&[0x03, 0x44, 0x24, 0x08]); // add eax,[esp+8]
        a.ret_n(8);
        m.cpu.write(CALLBACK, &a.bytes).unwrap();
        m.spawn("main", CODE, &[], 0x1_0000).unwrap();
        assert!(
            matches!(m.run_virtual(1_000_000), RunResult::Exited(0)),
            "backend={backend}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA).unwrap(),
            m.cpu.read_u32(DATA + 20).unwrap()
        );
        assert_eq!(
            *m.ext.get::<u32>(),
            1,
            "patched API entry must not dispatch its original handler"
        );
        for (offset, expected) in [
            (4, 12),
            (8, 13),
            (12, 1.25f32.to_bits()),
            (16, 42),
            (24, 0x1234_5678),
            (28, 0xfedc_ba98),
            (32, 999),
        ] {
            assert_eq!(
                m.cpu.read_u32(DATA + offset).unwrap(),
                expected,
                "backend={backend} offset={offset}"
            );
        }
    }
}

#[test]
fn real_minhook_iat_com_and_dllmain_match_native_windows() {
    let directory = fixtures();
    let exe = directory.join("fixture_patch_compat.exe");
    // Native Win32 must pass before any emulator is used to establish the result.
    let mut child = Command::new(&exe)
        .current_dir(&directory)
        .creation_flags(0x0800_0000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("native patch fixture exceeded ten seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        status.success(),
        "native fixture exit {:?}; last log {:?}",
        status.code(),
        std::fs::read_to_string(directory.join("patch-compat.log"))
    );
    let golden = std::fs::read_to_string(directory.join("patch-compat.log")).unwrap();
    assert_eq!(golden, GOLDEN);

    for backend in ["jit", "interpreter", "unicorn"] {
        let jit = backend == "jit";
        let data = directory.join(backend);
        let mut runtime =
            Runtime::with_cpu(&exe, &data, backend::Cpu::new(backend).unwrap(), 64, 32).unwrap();
        let module = adv_os::modules(&runtime.machine.ext)
            .by_name("fixture_patch_compat.exe")
            .unwrap();
        let target = named_export(
            &runtime.machine.cpu,
            adv_os::modules(&runtime.machine.ext).get(module).base,
            b"PatchTarget",
        );
        if jit {
            // Keep a compiled copy of the exact six-byte target before the DLL changes
            // it. A stale entry would return 17 and fail the guest's post-DllMain checks.
            runtime
                .machine
                .cpu
                .jit_mut()
                .unwrap()
                .executor_mut()
                .add_region(adv_cpu::Region::new(target, 6))
                .unwrap();
        }
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(10)));
        let result = runtime.step();
        assert!(
            matches!(result, RunResult::Exited(0)),
            "backend={backend}: {result:?}; recent: {:?}",
            runtime.machine.sys.recent_calls()
        );
        let actual = std::fs::read_to_string(data.join("overlay/patch-compat.log")).unwrap();
        assert_eq!(
            actual, golden,
            "backend={backend}: guest patch behavior differs from native Windows"
        );
        if jit {
            assert!(runtime.machine.cpu.stats().native_entries > 0);
            assert!(runtime.machine.cpu.stats().compilations > 0);
            assert!(
                runtime.machine.cpu.stats().invalidations > 0,
                "guest patches must invalidate previously translated code"
            );
        }
    }
}
