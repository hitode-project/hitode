//! Actual rb/seek/tell/size2/read/close sequence over one shared FILE/VFS owner.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const APISET: &str = "api-ms-win-crt-stdio-l1-1-0.dll";
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x450000;
const NAME: u32 = DATA + 0x800;
const MISSING: u32 = DATA + 0xa00;
const MODE: u32 = DATA + 0xc00;

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};

struct Fixture<B: Backend> {
    machine: Machine<B>,
    code: Asm,
    slots: Vec<(u32, u32)>,
}
impl<B: Backend> Fixture<B> {
    fn call(&mut self, module: &str, name: &str, args: &[Arg]) -> u32 {
        let output = DATA + self.slots.len() as u32 * 16;
        self.code.bytes.extend_from_slice(&[0x89, 0x25]);
        self.code
            .bytes
            .extend_from_slice(&(output + 4).to_le_bytes());
        for &arg in args.iter().rev() {
            match arg {
                V(value) => {
                    self.code.push_imm(value);
                }
                S(pointer) => {
                    self.code.mov_eax_mem(pointer).push_eax();
                }
            }
        }
        self.code
            .call(self.machine.resolve_import(module, name).unwrap())
            .mov_mem_eax(output);
        self.code.bytes.extend_from_slice(&[0x89, 0x25]);
        self.code
            .bytes
            .extend_from_slice(&(output + 8).to_le_bytes());
        if !args.is_empty() {
            self.code.add_esp(args.len() as u8 * 4);
        }
        self.code.bytes.extend_from_slice(&[0x89, 0x25]);
        self.code
            .bytes
            .extend_from_slice(&(output + 12).to_le_bytes());
        self.slots.push((output, args.len() as u32 * 4));
        output
    }
    fn word(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

fn wide(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn reader<B: Backend>(cpu: B, label: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/modern-stdio-tests")
        .join(label);
    std::fs::create_dir_all(&root).unwrap();
    let file_name = "字碼映射.bin";
    let bytes: Vec<u8> = (0..947u16).flat_map(|index| index.to_le_bytes()).collect();
    assert_eq!(bytes.len(), 1894);
    std::fs::write(root.join(file_name), &bytes).unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for module in [APISET, "ucrtbase.dll"] {
        for (name, argc) in [
            ("_wfopen_s", 3),
            ("fseek", 3),
            ("ftell", 1),
            ("fread", 4),
            ("fclose", 1),
        ] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc }
            );
            assert_eq!(
                registry.level(module, name),
                Some(ImplementationLevel::Implement)
            );
            assert_eq!(
                adv_abi::signature::lookup(module, name)
                    .unwrap()
                    .stack_words(),
                u16::from(argc)
            );
            assert!(registry.dynamic_available(module, name));
        }
        assert!(!registry.dynamic_available(module, "fwrite"));
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(&root, "fixture.exe", &root);
    config.os.write_game = false;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
    machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write(NAME, &wide(file_name)).unwrap();
    machine.cpu.write(MISSING, &wide("不存在.bin")).unwrap();
    machine.cpu.write(MODE, &wide("rb")).unwrap();
    machine.cpu.write(READONLY, &[0xa5; 4]).unwrap();
    let mut fixture = Fixture {
        machine,
        code: Asm::new(CODE),
        slots: Vec::new(),
    };
    let errno = fixture.call("ucrtbase.dll", "_errno", &[]);
    fixture
        .code
        .mov_eax_mem(errno)
        .bytes
        .extend_from_slice(&[0xc7, 0x00]);
    fixture.code.bytes.extend_from_slice(&79u32.to_le_bytes());
    let mut observations = Vec::new();
    for (index, module) in [APISET, "ucrtbase.dll"].into_iter().enumerate() {
        let output = DATA + 0xd04 + index as u32 * 16;
        fixture.machine.cpu.write(output - 4, &[0xa5; 12]).unwrap();
        let buffer = DATA + 0x2004 + index as u32 * 0x1000;
        fixture
            .machine
            .cpu
            .write(buffer - 4, &vec![0xcd; bytes.len() + 8])
            .unwrap();
        let opened = fixture.call(module, "_wfopen_s", &[V(output), V(NAME), V(MODE)]);
        let end = fixture.call(module, "fseek", &[S(output), V(0), V(2)]);
        let length = fixture.call(module, "ftell", &[S(output)]);
        let start = fixture.call(module, "fseek", &[S(output), V(0), V(0)]);
        let count = DATA + 0xe00 + index as u32 * 4;
        fixture
            .code
            .mov_eax_mem(length)
            .bytes
            .extend_from_slice(&[0xd1, 0xe8]);
        fixture.code.mov_mem_eax(count);
        let read = fixture.call(module, "fread", &[V(buffer), V(2), S(count), S(output)]);
        // A legacy front sees the exact stream position of the modern FILE.
        let legacy_position = fixture.call("msvcrt.dll", "ftell", &[S(output)]);
        let closed = fixture.call(module, "fclose", &[S(output)]);
        observations.push((
            output,
            buffer,
            opened,
            end,
            length,
            start,
            read,
            legacy_position,
            closed,
        ));
    }
    fixture
        .code
        .mov_eax_mem(errno)
        .bytes
        .extend_from_slice(&[0x8b, 0x00]);
    fixture.code.mov_mem_eax(DATA + 0xe20);
    let failed_output = DATA + 0xd44;
    fixture
        .machine
        .cpu
        .write(failed_output - 4, &[0xa5; 12])
        .unwrap();
    let missing = fixture.call(
        APISET,
        "_wfopen_s",
        &[V(failed_output), V(MISSING), V(MODE)],
    );
    // The final real thunk call must fail before allocating a FILE or writing
    // even one byte of this protected FILE* output.
    fixture.call(APISET, "_wfopen_s", &[V(READONLY), V(NAME), V(MODE)]);
    fixture
        .machine
        .cpu
        .write(CODE, &fixture.code.bytes)
        .unwrap();
    fixture
        .machine
        .spawn("Modern stdio rb reader", CODE, &[], 65536)
        .unwrap();
    let result = fixture.machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    for &(output, stack_bytes) in &fixture.slots[..fixture.slots.len() - 1] {
        let before = fixture.word(output + 4);
        assert_eq!(fixture.word(output + 8), before - stack_bytes);
        assert_eq!(fixture.word(output + 12), before);
    }
    for (output, buffer, opened, end, length, start, read, legacy_position, closed) in observations
    {
        assert_eq!(
            [
                fixture.word(opened),
                fixture.word(end),
                fixture.word(start),
                fixture.word(closed)
            ],
            [0; 4]
        );
        assert_eq!(fixture.word(length), 1894);
        assert_eq!(fixture.word(read), 947);
        assert_eq!(fixture.word(legacy_position), 1894);
        let file = fixture.word(output);
        assert_ne!(file, 0);
        assert!(
            fixture
                .machine
                .ext
                .get::<Heaps>()
                .owner(&fixture.machine.cpu, file)
                .is_none()
        );
        let mut output_bytes = [0; 12];
        fixture
            .machine
            .cpu
            .read(output - 4, &mut output_bytes)
            .unwrap();
        assert_eq!(&output_bytes[..4], &[0xa5; 4]);
        assert_eq!(&output_bytes[8..], &[0xa5; 4]);
        let mut actual = vec![0; bytes.len() + 8];
        fixture.machine.cpu.read(buffer - 4, &mut actual).unwrap();
        assert_eq!(&actual[..4], &[0xcd; 4]);
        assert_eq!(&actual[4..bytes.len() + 4], bytes);
        assert_eq!(&actual[bytes.len() + 4..], &[0xcd; 4]);
    }
    assert_eq!(fixture.word(DATA + 0xe20), 79);
    assert_eq!(fixture.word(missing), 2);
    assert_eq!(fixture.word(failed_output), 0);
    assert_eq!(fixture.word(failed_output - 4), 0xa5a5a5a5);
    assert_eq!(fixture.word(failed_output + 4), 0xa5a5a5a5);
    assert_eq!(fixture.word(fixture.word(errno)), 2);
    assert_eq!(fixture.word(READONLY), 0xa5a5a5a5);
}

#[test]
fn modern_stdio_reads_unicode_binary_vfs_through_the_shared_file_owner() {
    reader(JitCpu::new(PARK_ADDR, false).unwrap(), "interpreter");
    reader(JitCpu::new(PARK_ADDR, true).unwrap(), "jit");
    reader(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
}
