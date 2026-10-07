//! Guest cdecl CRT fronts, stdcall entry routines and shared thread cleanup.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Object, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x420000;

trait TestCpu: Backend {
    fn region(&mut self, _: u32, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, start: u32, size: usize) {
        self.executor_mut()
            .add_region(Region::new(start, size))
            .unwrap();
    }
}

enum Arg {
    Value(u32),
    Memory(u32),
}
use Arg::{Memory as M, Value as I};

fn invoke(code: &mut Asm, function: u32, args: &[Arg], cdecl: bool) {
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(address) => {
                code.mov_eax_mem(*address).push_eax();
            }
        }
    }
    code.call(function);
    if cdecl && !args.is_empty() {
        code.add_esp(args.len() as u8 * 4);
    }
}

fn observed(env: &mut Env<'_>) -> Outcome {
    let (handle, cell, code) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(Object::Thread { ctx }) = env.sys.objects.get(handle) else {
        panic!("missing caller-owned thread handle")
    };
    assert_eq!(env.sys.context(*ctx).exit_code(), Some(code));
    assert_eq!(env.ext.get::<adv_os::Heaps>().owner(cell), None);
    assert!(
        env.ext
            .get::<super::super::CrtState>()
            .errno
            .keys()
            .all(|(_, ctx)| *ctx == env.context)
    );
    Outcome::Return(1)
}

struct Fixture<B: TestCpu> {
    machine: Machine<B>,
    code: Asm,
    data: u32,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        super::super::register(&mut registry);
        registry.add(
            "fixture.dll",
            "observed",
            Conv::Stdcall { argc: 3 },
            observed,
        );
        for module in MODULES {
            for (api, argc) in [("_beginthreadex", 6), ("_endthreadex", 1)] {
                assert_eq!(
                    registry.get(module, api).unwrap().conv,
                    Conv::Cdecl { argc }
                );
                assert!(
                    registry.dynamic_available(module, api),
                    "missing {module}!{api}; registered={} conflicts={:?}",
                    registry.is_implemented(module, api),
                    registry.signature_conflicts()
                );
            }
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-thread-tests");
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(&root, "fixture.exe", &root),
        )
        .unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READONLY, &[0xcc; 4]).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            data: DATA,
        }
    }
    fn alloc(&mut self, value: u32) -> u32 {
        let address = self.data;
        self.data += 4;
        self.machine.cpu.write_u32(address, value).unwrap();
        address
    }
    fn function(&mut self, module: &str, name: &str) -> u32 {
        self.machine.resolve_import(module, name).unwrap()
    }
    fn call(&mut self, module: &str, name: &str, args: &[Arg]) -> u32 {
        let function = self.function(module, name);
        let result = self.alloc(0);
        invoke(&mut self.code, function, args, module.starts_with("msvc"));
        self.code.mov_mem_eax(result);
        result
    }
    fn seed(&mut self, cell: u32) {
        self.code
            .mov_eax_mem(cell)
            .bytes
            .extend_from_slice(&[0xc7, 0x00]);
        self.code.bytes.extend_from_slice(&79_u32.to_le_bytes());
        self.code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
        self.code.bytes.extend_from_slice(&91_u32.to_le_bytes());
        self.call("kernel32.dll", "SetLastError", &[I(0x12345678)]);
    }
    fn errors(&mut self, cell: u32) -> [u32; 3] {
        let error = self.alloc(0);
        let dos = self.alloc(0);
        self.code
            .mov_eax_mem(cell)
            .mov_eax_ptr_eax()
            .mov_mem_eax(error);
        self.code
            .mov_eax_mem(cell)
            .add_eax_imm(4)
            .mov_eax_ptr_eax()
            .mov_mem_eax(dos);
        let last = self.call("kernel32.dll", "GetLastError", &[]);
        [error, dos, last]
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

fn guest<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu);
    let mut expected = Vec::new();
    let mut workers = Vec::new();
    let mut handler_count = Vec::new();
    for (index, module) in MODULES.into_iter().enumerate() {
        let legacy = module == "msvcrt.dll";
        let cell = test.call(module, "_errno", &[]);
        let errno_api = test.function(module, "_errno");
        let last_api = test.function("kernel32.dll", "GetLastError");
        let end_api = test.function(module, "_endthreadex");
        let observed = test.function("fixture.dll", "observed");
        let worker_base = CODE + 0x8000 + index as u32 * 0x800;
        let handler_base = worker_base + 0x400;
        let count = test.alloc(0);
        let seen = [test.alloc(0), test.alloc(0), test.alloc(0)];
        let mut handler = Asm::new(handler_base);
        handler.bytes.extend_from_slice(&[0xff, 0x05]);
        handler.bytes.extend_from_slice(&count.to_le_bytes());
        handler
            .mov_eax_mem(cell)
            .mov_eax_ptr_eax()
            .mov_mem_eax(seen[0]);
        handler
            .mov_eax_mem(cell)
            .add_eax_imm(4)
            .mov_eax_ptr_eax()
            .mov_mem_eax(seen[1]);
        handler.call(last_api).mov_mem_eax(seen[2]).ret();
        test.machine
            .cpu
            .write(handler_base, &handler.bytes)
            .unwrap();
        test.machine.cpu.region(handler_base, handler.bytes.len());
        if !legacy {
            test.call(module, "_set_invalid_parameter_handler", &[I(handler_base)]);
        }
        handler_count.push(count);

        for explicit in [false, true] {
            let worker = worker_base + if explicit { 0x200 } else { 0 };
            let worker_cell = test.alloc(0);
            let worker_errno = test.alloc(u32::MAX);
            let worker_dos = test.alloc(u32::MAX);
            let after_end = test.alloc(0);
            let mut body = Asm::new(worker);
            body.call(errno_api)
                .mov_mem_eax(worker_cell)
                .mov_eax_ptr_eax()
                .mov_mem_eax(worker_errno);
            body.mov_eax_mem(worker_cell)
                .add_eax_imm(4)
                .mov_eax_ptr_eax()
                .mov_mem_eax(worker_dos);
            if explicit {
                body.push_imm(0x80000001)
                    .call(end_api)
                    .mov_mem_imm(after_end, 1);
            } else {
                body.mov_eax_esp(4).add_eax_imm(1);
            }
            body.ret_n(4); // SDK _beginthreadex_proc_type is unsigned __stdcall(void*).
            test.machine.cpu.write(worker, &body.bytes).unwrap();
            test.machine.cpu.region(worker, body.bytes.len());
            let tid = test.alloc(0xaaaaaaaa);
            test.seed(cell);
            let handle = test.call(
                module,
                "_beginthreadex",
                &[I(0), I(0), I(worker), I(41), I(0), I(tid)],
            );
            let preserved = test.errors(cell);
            expected.extend(preserved.into_iter().zip([
                79,
                91,
                if legacy { 0x12345678 } else { 0 },
            ]));
            let waiting = if !explicit && index == 2 {
                // Same kernel Thread object remains alive through a duplicated
                // handle even when its original returned handle is closed.
                let alias = test.alloc(0);
                test.call(
                    "kernel32.dll",
                    "DuplicateHandle",
                    &[
                        I(u32::MAX),
                        M(handle),
                        I(u32::MAX),
                        I(alias),
                        I(0),
                        I(0),
                        I(2),
                    ],
                );
                test.call("kernel32.dll", "CloseHandle", &[M(handle)]);
                alias
            } else {
                handle
            };
            let wait = test.call(
                "kernel32.dll",
                "WaitForSingleObject",
                &[M(waiting), I(u32::MAX)],
            );
            let exit = test.alloc(0);
            let status = test.call("kernel32.dll", "GetExitCodeThread", &[M(waiting), I(exit)]);
            let code = if explicit { 0x80000001 } else { 42 };
            invoke(
                &mut test.code,
                observed,
                &[M(waiting), M(worker_cell), I(code)],
                false,
            );
            let closed = test.call("kernel32.dll", "CloseHandle", &[M(waiting)]);
            workers.push((handle, tid, worker_errno, worker_dos, after_end));
            expected.extend([(wait, 0), (status, 1), (exit, code), (closed, 1)]);
        }

        for (start, stack, flags, want_error, want_dos, want_le) in [
            (0, 0, 0, 22, 91, if legacy { 0x12345678 } else { 0 }),
            (worker_base, 0, 4, 22, 50, 50),
            (worker_base, 0, 0x10000, 22, 50, 50),
            (worker_base, 0, 1, 22, 50, 50),
            (worker_base, u32::MAX, 0, 22, 87, 87),
        ] {
            let tid = test.alloc(0xaaaaaaaa);
            test.seed(cell);
            let result = test.call(
                module,
                "_beginthreadex",
                &[I(0), I(stack), I(start), I(41), I(flags), I(tid)],
            );
            let errors = test.errors(cell);
            expected.extend([
                (result, if legacy && start == 0 { u32::MAX } else { 0 }),
                (tid, 0xaaaaaaaa),
            ]);
            expected.extend(errors.into_iter().zip([want_error, want_dos, want_le]));
        }
        expected.extend(
            seen.into_iter()
                .zip(if legacy { [0, 0, 0] } else { [22, 91, 0] }),
        );
    }
    // A bare fixture-entry RET only retires the machine context. Exercise the
    // actual Win32 process exit path so remaining main-thread CRT cells detach.
    test.call("kernel32.dll", "ExitProcess", &[I(0)]);
    assert!(test.code.bytes.len() < 0x8000);
    test.machine.cpu.write(CODE, &test.code.bytes).unwrap();
    test.machine.cpu.region(CODE, test.code.bytes.len());
    test.machine
        .spawn("CRT thread guest", CODE, &[], 65536)
        .unwrap();
    let result = test.machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (address, value) in expected {
        assert_eq!(test.value(address), value, "at {address:#x}");
    }
    for (handle, tid, error, dos, after) in workers {
        assert_ne!(test.value(handle), 0);
        assert_ne!(test.value(tid), 0xaaaaaaaa);
        assert_eq!(
            (test.value(error), test.value(dos), test.value(after)),
            (0, 0, 0)
        );
        assert!(test.machine.sys.objects.get(test.value(handle)).is_none());
    }
    for (index, count) in handler_count.into_iter().enumerate() {
        assert_eq!(test.value(count), u32::from(index != 0));
    }
    assert!(
        test.machine
            .ext
            .get::<super::super::CrtState>()
            .errno
            .is_empty()
    );
    assert!(test.machine.sys.contexts.iter().all(|ctx| ctx.is_done()));
    assert_eq!(test.machine.sys.contexts.len(), 7); // Main + six real workers; no failed births.

    // Pointer preflight is the explicit ABI-fault profile (not a claimed native
    // AV return value). It cannot allocate a thread or partially write a DWORD.
    let thunk = test
        .machine
        .sys
        .thunks
        .iter()
        .find(|(_, t)| t.module == "msvcr90.dll" && t.name == "_beginthreadex")
        .unwrap()
        .0;
    let before = (
        test.machine.sys.contexts.len(),
        test.machine.sys.objects.object_count(),
    );
    for address in [READONLY, DATA + 0x3fff, u32::MAX - 1] {
        let mut env = Env {
            context: 0,
            cpu: &mut test.machine.cpu,
            sys: &mut test.machine.sys,
            ext: &mut test.machine.ext,
            thunk,
        };
        assert!(matches!(
            begin(
                &mut env,
                GuestPtr::new(0),
                0,
                GuestPtr::new(CODE + 0x8000),
                GuestPtr::new(41),
                0,
                GuestPtr::new(address)
            ),
            Outcome::Fail(_)
        ));
    }
    assert_eq!(test.value(READONLY), 0xcccccccc);
    assert_eq!(
        (
            test.machine.sys.contexts.len(),
            test.machine.sys.objects.object_count()
        ),
        before
    );
    let diagnostics = test.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    for module in MODULES {
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|key| key.0 == format!("{module}!_beginthreadex")
                    && key.1.contains("initialization flags"))
        );
    }
}

#[test]
fn crt_thread_fronts_share_stdcall_entries_waitable_handles_error_tls_and_exit_cleanup() {
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(JitCpu::new(PARK_ADDR, true).unwrap());
}
