//! Actual path searches and secure font/path copies use existing memory/CRT owners.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const VCRT: &str = "vcruntime140.dll";
const APISET: &str = "api-ms-win-crt-string-l1-1-0.dll";
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x420000;
const ANSI: u32 = DATA + 0x300;
const WIDE: u32 = DATA + 0x380;
const FONT: u32 = DATA + 0x400;
const SUFFIX: u32 = DATA + 0x430;
const CALLBACK: u32 = CODE + 0xc00;
const ERRNO_PTR: u32 = DATA + 0xe00;
const ERRNO_BEFORE: u32 = DATA + 0xe04;
const CALLBACKS: u32 = DATA + 0xe08;
const SEEN_ERRNO: u32 = DATA + 0xe0c;

struct Fixture<B: Backend> {
    machine: Machine<B>,
    code: Asm,
    slots: Vec<(u32, u32)>,
}
impl<B: Backend> Fixture<B> {
    fn call(&mut self, module: &str, name: &str, args: &[u32]) -> u32 {
        let output = DATA + self.slots.len() as u32 * 16;
        self.code.bytes.extend_from_slice(&[0x89, 0x25]);
        self.code
            .bytes
            .extend_from_slice(&(output + 4).to_le_bytes());
        for &value in args.iter().rev() {
            self.code.push_imm(value);
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
fn wide(units: &[u16]) -> Vec<u8> {
    units
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn paths<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for (module, name, argc) in [
        (VCRT, "strrchr", 2),
        (VCRT, "wcsrchr", 2),
        ("ucrtbase.dll", "wcscpy_s", 3),
        (APISET, "wcscpy_s", 3),
        ("ucrtbase.dll", "strcpy_s", 3),
        (APISET, "strcpy_s", 3),
        ("ucrtbase.dll", "strncpy_s", 4),
        (APISET, "strncpy_s", 4),
        (APISET, "wcscat_s", 3),
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
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/string-paths",
            "fixture.exe",
            "/nonexistent/string-paths-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
    let ansi = b"C:\\dir\\cp\x82\xa0\\file.ttf\0";
    let path_wide = [
        0x43, 0x3a, 0x5c, 0x4e2d, 0x5c, 0xd83d, 0xde00, 0x5c, 0x66, 0x74, 0x66,
    ];
    let font = [0x5b8b, 0x4f53, 0xd83d, 0xde00];
    machine.cpu.write(ANSI, ansi).unwrap();
    machine.cpu.write(WIDE, &wide(&path_wide)).unwrap();
    machine.cpu.write(FONT, &wide(&font)).unwrap();
    machine
        .cpu
        .write(SUFFIX, &wide(&[0x2d, 0x56, 0x4e]))
        .unwrap();
    machine.cpu.write(READONLY, &[0xa5; 64]).unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.bytes.extend_from_slice(&[0xff, 0x05]);
    callback.bytes.extend_from_slice(&CALLBACKS.to_le_bytes());
    callback
        .mov_eax_mem(ERRNO_PTR)
        .bytes
        .extend_from_slice(&[0x8b, 0x00]);
    callback.mov_mem_eax(SEEN_ERRNO);
    callback.bytes.extend_from_slice(&[0x31, 0xc0]);
    callback.ret();
    machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    let mut fixture = Fixture {
        machine,
        code: Asm::new(CODE),
        slots: Vec::new(),
    };
    let errno = fixture.call("ucrtbase.dll", "_errno", &[]);
    fixture
        .code
        .mov_eax_mem(errno)
        .mov_mem_eax(ERRNO_PTR)
        .bytes
        .extend_from_slice(&[0xc7, 0x00]);
    fixture.code.bytes.extend_from_slice(&79u32.to_le_bytes());
    fixture.call(
        "ucrtbase.dll",
        "_set_invalid_parameter_handler",
        &[CALLBACK],
    );
    let mut searches = Vec::new();
    for (name, source, value, expected) in [
        (
            "strrchr",
            ANSI,
            0x15c,
            ANSI + ansi.iter().rposition(|&byte| byte == b'\\').unwrap() as u32,
        ),
        ("strrchr", ANSI, 0, ANSI + ansi.len() as u32 - 1),
        ("strrchr", ANSI, b'Z' as u32, 0),
        ("wcsrchr", WIDE, 0x1005c, WIDE + 14),
        ("wcsrchr", WIDE, 0, WIDE + path_wide.len() as u32 * 2),
        ("wcsrchr", WIDE, b'Z' as u32, 0),
    ] {
        searches.push((fixture.call(VCRT, name, &[source, value]), expected));
    }
    let mut copies = Vec::new();
    for (index, module) in [APISET, "ucrtbase.dll"].into_iter().enumerate() {
        let output_w = DATA + 0x500 + index as u32 * 0x100;
        let output_a = output_w + 0x50;
        let output_n = DATA + 0x900 + index as u32 * 0x180;
        fixture.machine.cpu.write(output_w, &[0xa5; 64]).unwrap();
        fixture.machine.cpu.write(output_a, &[0xa5; 32]).unwrap();
        fixture.machine.cpu.write(output_n, &[0xa5; 260]).unwrap();
        let copy_w = fixture.call(module, "wcscpy_s", &[output_w, 32, FONT]);
        let copy_a = fixture.call(module, "strcpy_s", &[output_a, 32, ANSI]);
        let copy_n = fixture.call(module, "strncpy_s", &[output_n, 260, ANSI, 6]);
        copies.push((output_w, output_a, output_n, copy_w, copy_a, copy_n));
    }
    let append = fixture.call(APISET, "wcscat_s", &[DATA + 0x500, 32, SUFFIX]);
    fixture
        .code
        .mov_eax_mem(ERRNO_PTR)
        .bytes
        .extend_from_slice(&[0x8b, 0x00]);
    fixture.code.mov_mem_eax(ERRNO_BEFORE);
    fixture.machine.cpu.write(DATA + 0xc00, &[0xa5; 4]).unwrap();
    let range_error = fixture.call(APISET, "strcpy_s", &[DATA + 0xc00, 3, ANSI]);
    // Fail atomically on the full protected destination, after all ordinary calls.
    fixture.call(APISET, "wcscpy_s", &[READONLY, 32, FONT]);
    fixture
        .machine
        .cpu
        .write(CODE, &fixture.code.bytes)
        .unwrap();
    fixture
        .machine
        .spawn("Modern string paths", CODE, &[], 65536)
        .unwrap();
    let result = fixture.machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    for &(output, bytes) in &fixture.slots[..fixture.slots.len() - 1] {
        let before = fixture.word(output + 4);
        assert_eq!(fixture.word(output + 8), before - bytes);
        assert_eq!(fixture.word(output + 12), before);
    }
    for (result, expected) in searches {
        assert_eq!(fixture.word(result), expected);
    }
    for (index, (output_w, output_a, output_n, copy_w, copy_a, copy_n)) in
        copies.into_iter().enumerate()
    {
        assert_eq!(
            [
                fixture.word(copy_w),
                fixture.word(copy_a),
                fixture.word(copy_n)
            ],
            [0; 3]
        );
        let mut actual_w = [0; 64];
        fixture.machine.cpu.read(output_w, &mut actual_w).unwrap();
        let mut expected_units = font.to_vec();
        if index == 0 {
            expected_units.extend([0x2d, 0x56, 0x4e]);
        }
        let expected_w = wide(&expected_units);
        assert_eq!(&actual_w[..expected_w.len()], expected_w);
        assert!(
            actual_w[expected_w.len()..]
                .iter()
                .all(|&byte| byte == 0xa5)
        );
        let mut actual_a = [0; 32];
        fixture.machine.cpu.read(output_a, &mut actual_a).unwrap();
        assert_eq!(&actual_a[..ansi.len()], ansi);
        assert!(actual_a[ansi.len()..].iter().all(|&byte| byte == 0xa5));
        let mut actual_n = [0; 260];
        fixture.machine.cpu.read(output_n, &mut actual_n).unwrap();
        assert_eq!(&actual_n[..6], &ansi[..6]);
        assert_eq!(actual_n[6], 0);
        assert!(actual_n[7..].iter().all(|&byte| byte == 0xa5));
    }
    assert_eq!(fixture.word(append), 0);
    assert_eq!(fixture.word(ERRNO_BEFORE), 79);
    assert_eq!(fixture.word(range_error), 34);
    assert_eq!(fixture.word(CALLBACKS), 1);
    assert_eq!(fixture.word(SEEN_ERRNO), 34);
    assert_eq!(fixture.machine.cpu.read_u8(DATA + 0xc00).unwrap(), 0);
    let mut protected = [0; 64];
    fixture.machine.cpu.read(READONLY, &mut protected).unwrap();
    assert_eq!(protected, [0xa5; 64]);
}

#[test]
fn modern_string_fronts_search_and_copy_real_guest_paths_on_three_backends() {
    paths(JitCpu::new(PARK_ADDR, false).unwrap());
    paths(JitCpu::new(PARK_ADDR, true).unwrap());
    paths(UnicornCpu::new(PARK_ADDR).unwrap());
}
