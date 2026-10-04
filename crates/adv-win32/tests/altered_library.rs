//! Authored LoadLibraryEx code and DLLs; no game data, native DLL or host window.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const COUNT: u32 = DATA + 1024;
struct Test {
    machine: Machine<JitCpu>,
    asm: Asm,
    next: u32,
}
impl Test {
    fn new(jit: bool, rejects: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "target/altered-library-{}-{jit}-{rejects}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let body = if rejects {
            vec![0x31, 0xc0, 0xc2, 12, 0]
        } else {
            let mut body = vec![0xff, 0x05]; // inc DWORD PTR [COUNT]; mov eax,1; ret 12
            body.extend_from_slice(&COUNT.to_le_bytes());
            body.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc2, 12, 0]);
            body
        };
        let mut image = adv_os::synthetic::build(
            0x500000,
            "owned.dll",
            &[adv_os::synthetic::Export {
                name: Some("DllMain".into()),
                ordinal: Some(1),
                target: adv_os::synthetic::Target::Code(body),
            }],
        )
        .unwrap()
        .bytes;
        let pe = adv_pe::Pe::parse(&image).unwrap();
        let adv_pe::ExportTarget::Rva(entry) = pe.exports().unwrap()[0].target else {
            panic!("entry")
        };
        image[0x98 + 16..0x98 + 20].copy_from_slice(&entry.to_le_bytes());
        std::fs::write(root.join("owned.dll"), image).unwrap();
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(&root, "authored.exe", root.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.write(DATA, b"C:\\Game\\owned.dll\0").unwrap();
        let wide: Vec<_> = "C:\\Game\\owned.dll\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        machine.cpu.write(DATA + 128, &wide).unwrap();
        machine.cpu.write(DATA + 256, b"owned.dll\0").unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
            next: DATA + 2048,
        }
    }
    fn call(&mut self, name: &str, args: &[u32]) -> u32 {
        for &arg in args.iter().rev() {
            self.asm.push_imm(arg);
        }
        let address = self.machine.resolve_import("kernel32.dll", name).unwrap();
        let output = self.next;
        self.next += 4;
        self.asm.call(address).mov_mem_eax(output);
        output
    }
    fn stored_call(&mut self, name: &str, arg: u32) -> u32 {
        self.asm.mov_eax_mem(arg).push_eax();
        let address = self.machine.resolve_import("kernel32.dll", name).unwrap();
        let output = self.next;
        self.next += 4;
        self.asm.call(address).mov_mem_eax(output);
        output
    }
    fn count(&mut self) -> u32 {
        let output = self.next;
        self.next += 4;
        self.asm.mov_eax_mem(COUNT).mov_mem_eax(output);
        output
    }
    fn run(&mut self) {
        self.call("ExitProcess", &[0]);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        self.machine.spawn("authored", CODE, &[], 65536).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

#[test]
fn altered_a_w_loads_run_initialization_once_retain_one_owner_and_unload_normally() {
    for jit in [false, true] {
        let mut test = Test::new(jit, false);
        let first = test.call("LoadLibraryExA", &[DATA, 0, 8]);
        let initialized = test.count();
        let second = test.call("LoadLibraryExW", &[DATA + 128, 0, 8]);
        let repeated = test.count();
        let release1 = test.stored_call("FreeLibrary", first);
        let visible = test.call("GetModuleHandleA", &[DATA + 256]);
        let release2 = test.stored_call("FreeLibrary", second);
        let gone = test.call("GetModuleHandleA", &[DATA + 256]);
        let detached = test.count();
        test.run();
        assert_eq!(test.value(first), 0x500000);
        assert_eq!(test.value(second), test.value(first));
        assert_eq!(test.value(initialized), 1);
        assert_eq!(test.value(repeated), 1);
        assert_eq!(test.value(release1), 1);
        assert_eq!(test.value(visible), test.value(first));
        assert_eq!(test.value(release2), 1);
        assert_eq!(test.value(gone), 0);
        assert_eq!(test.value(detached), 2);
    }
}

#[test]
fn altered_failed_initializer_and_invalid_flag_combinations_leave_no_live_image() {
    let mut test = Test::new(false, true);
    let rejected = test.call("LoadLibraryExA", &[DATA, 0, 8]);
    let init_error = test.call("GetLastError", &[]);
    let gone = test.call("GetModuleHandleA", &[DATA + 256]);
    let invalid = test.call("LoadLibraryExW", &[DATA + 128, 1, 8]);
    let parameter_error = test.call("GetLastError", &[]);
    let combined = test.call("LoadLibraryExA", &[DATA, 0, 8 | 0x800]);
    let combined_error = test.call("GetLastError", &[]);
    let unknown = test.call("LoadLibraryExA", &[DATA, 0, 0x20]);
    let unknown_error = test.call("GetLastError", &[]);
    test.run();
    assert_eq!(test.value(rejected), 0);
    assert_eq!(test.value(init_error), 1114);
    assert_eq!(test.value(gone), 0);
    assert_eq!(test.value(invalid), 0);
    assert_eq!(test.value(parameter_error), 87);
    assert_eq!(test.value(combined), 0);
    assert_eq!(test.value(combined_error), 87);
    assert_eq!(test.value(unknown), 0);
    assert_eq!(test.value(unknown_error), 50);
}
