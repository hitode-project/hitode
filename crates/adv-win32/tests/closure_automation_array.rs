//! Real array storage, bounds and element ownership across asynchronous guest callbacks.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const BOUNDS: u32 = DATA + 0x1000;
const INDEX: u32 = DATA + 0x1100;
const OUT: u32 = DATA + 0x1200;
const HANDLES: u32 = DATA + 0x1400;
const SOURCE: u32 = DATA + 0x1500;
const DEST: u32 = DATA + 0x1520;
const TEXT: u32 = DATA + 0x1600;
const OBJECT: u32 = DATA + 0x1700;
const TABLE: u32 = DATA + 0x1740;
const REFS: u32 = DATA + 0x1780;
const MARK: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
struct Calls<B: Backend> {
    m: Machine<B>,
    code: Asm,
    expected: Vec<(String, Option<u32>)>,
}
impl<B: Backend> Calls<B> {
    fn call(&mut self, name: &str, args: &[Arg], value: Option<u32>) -> u32 {
        let api = self.m.resolve_import("oleaut32.dll", name).unwrap();
        let set = self
            .m
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap();
        let get = self
            .m
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        self.code.push_imm(MARK).call(set);
        for arg in args.iter().rev() {
            match arg {
                I(v) => {
                    self.code.push_imm(*v);
                }
                L(p) => {
                    self.code.mov_eax_mem(*p).push_eax();
                }
            }
        }
        let row = DATA + self.expected.len() as u32 * 8;
        self.code
            .call(api)
            .mov_mem_eax(row)
            .call(get)
            .mov_mem_eax(row + 4);
        self.expected.push((name.into(), value));
        row
    }
    fn capture(&mut self, source: u32, destination: u32) {
        self.code.mov_eax_mem(source).mov_mem_eax(destination);
    }
    fn store(&mut self, pointer: u32, value: u32) {
        self.code.mov_eax_mem(pointer);
        self.code.bytes.extend([0xc7, 0x00]);
        self.code.bytes.extend(value.to_le_bytes());
    }
    fn data(&mut self, handle: u32, output: u32) {
        self.code
            .mov_eax_mem(handle)
            .add_eax_imm(12)
            .mov_eax_ptr_eax()
            .mov_mem_eax(output);
    }
    fn variant(&mut self, kind: u32, handle: u32) {
        self.code
            .mov_mem_imm(SOURCE, kind)
            .mov_eax_mem(handle)
            .mov_mem_eax(SOURCE + 8);
    }
    fn create(&mut self, kind: u32, dimensions: u32) {
        let row = self.call(
            "SafeArrayCreate",
            &[I(kind), I(dimensions), I(BOUNDS)],
            None,
        );
        self.capture(row, HANDLES);
        self.data(HANDLES, HANDLES + 4);
    }
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-array-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    for (i, value) in [2, (-3i32) as u32, 3, 7].into_iter().enumerate() {
        m.cpu.write_u32(BOUNDS + i as u32 * 4, value).unwrap();
    }
    for (i, index) in [[-3i32, 7], [-2, 7], [-3, 8], [-4, 7]]
        .into_iter()
        .enumerate()
    {
        for (j, value) in index.into_iter().enumerate() {
            m.cpu
                .write_u32(INDEX + i as u32 * 16 + j as u32 * 4, value as u32)
                .unwrap();
        }
    }
    m.cpu.write(TEXT, &[0x42, 0x30, 0, 0, 90, 0]).unwrap();
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(TABLE + 4, CODE + 0x2000).unwrap();
    m.cpu.write_u32(TABLE + 8, CODE + 0x2100).unwrap();
    m.cpu.write_u32(REFS, 1).unwrap();
    let mut f = Calls {
        m,
        code: Asm::new(CODE),
        expected: Vec::new(),
    };
    f.create(3, 2);
    f.capture(HANDLES + 4, DATA + 0x800);
    for (name, dimension, offset) in [
        ("SafeArrayGetLBound", 1, 0),
        ("SafeArrayGetUBound", 1, 4),
        ("SafeArrayGetLBound", 2, 8),
        ("SafeArrayGetUBound", 2, 12),
    ] {
        f.call(name, &[L(HANDLES), I(dimension), I(OUT + offset)], Some(0));
    }
    for i in 0..4 {
        f.code.mov_mem_imm(OUT + 16 + i * 4, MARK);
        f.call(
            "SafeArrayPtrOfIndex",
            &[L(HANDLES), I(INDEX + i * 16), I(OUT + 16 + i * 4)],
            Some(if i == 3 { 0x8002000b } else { 0 }),
        );
    }
    f.store(HANDLES + 4, 42);
    f.variant(0x2003, HANDLES);
    f.call("SafeArrayLock", &[L(HANDLES)], Some(0));
    f.call("VariantClear", &[I(SOURCE)], Some(0x8002000d));
    f.capture(SOURCE, DATA + 0x804);
    f.call("SafeArrayUnlock", &[L(HANDLES)], Some(0));
    f.call("VariantCopy", &[I(DEST), I(SOURCE)], Some(0));
    f.capture(DEST + 8, HANDLES + 8);
    f.data(HANDLES + 8, HANDLES + 12);
    f.store(HANDLES + 4, 99);
    f.code
        .mov_eax_mem(HANDLES + 12)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 0x808);
    f.call("VariantClear", &[I(SOURCE)], Some(0));
    f.call("VariantClear", &[I(DEST)], Some(0));
    // BSTR array storage owns its strings; the copy receives independent BSTRs.
    f.code.mov_mem_imm(BOUNDS, 2).mov_mem_imm(BOUNDS + 4, 0);
    f.create(8, 1);
    let row = f.call("SysAllocStringLen", &[I(TEXT), I(3)], None);
    f.capture(row, HANDLES + 16);
    f.code
        .mov_eax_mem(HANDLES + 4)
        .mov_ebx_eax()
        .mov_eax_mem(HANDLES + 16);
    f.code.bytes.extend([0x89, 0x03]);
    f.variant(0x2008, HANDLES);
    f.call("VariantCopy", &[I(DEST), I(SOURCE)], Some(0));
    f.capture(DEST + 8, HANDLES + 8);
    f.data(HANDLES + 8, HANDLES + 12);
    f.code
        .mov_eax_mem(HANDLES + 12)
        .mov_eax_ptr_eax()
        .mov_mem_eax(HANDLES + 20);
    f.capture(HANDLES + 16, DATA + 0x80c);
    f.capture(HANDLES + 20, DATA + 0x810);
    f.call("VariantClear", &[I(SOURCE)], Some(0));
    f.call("SysStringLen", &[L(HANDLES + 20)], Some(3));
    f.code
        .mov_eax_mem(HANDLES + 20)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 0x814);
    f.call("VariantClear", &[I(DEST)], Some(0));
    // A VARIANT array invokes actual AddRef/Release methods which yield.
    f.code.mov_mem_imm(BOUNDS, 1);
    f.create(12, 1);
    f.store(HANDLES + 4, 13);
    f.code.mov_eax_mem(HANDLES + 4).add_eax_imm(8);
    f.code.bytes.extend([0xc7, 0x00]);
    f.code.bytes.extend(OBJECT.to_le_bytes());
    f.variant(0x200c, HANDLES);
    f.call("VariantCopy", &[I(DEST), I(SOURCE)], Some(0));
    f.capture(REFS, DATA + 0x818);
    f.call("VariantClear", &[I(SOURCE)], Some(0));
    f.capture(REFS, DATA + 0x81c);
    f.call("VariantClear", &[I(DEST)], Some(0));
    f.capture(REFS, DATA + 0x820);
    f.call("SafeArrayCreate", &[I(0), I(1), I(BOUNDS)], Some(0));
    f.call("SafeArrayCreate", &[I(3), I(0), I(0)], Some(0));
    f.code
        .mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    f.m.cpu.write(CODE, &f.code.bytes).unwrap();
    let sleep = f.m.resolve_import("kernel32.dll", "Sleep").unwrap();
    for (address, delta) in [(CODE + 0x2000, 1u32), (CODE + 0x2100, u32::MAX)] {
        let mut cb = Asm::new(address);
        cb.mov_eax_mem(REFS)
            .add_eax_imm(delta)
            .mov_mem_eax(REFS)
            .push_imm(1)
            .call(sleep)
            .mov_eax_mem(REFS)
            .ret_n(4);
        f.m.cpu.write(address, &cb.bytes).unwrap();
    }
    f.m.spawn("Automation array ownership", CODE, &[], 65536)
        .unwrap();
    let result = f.m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value)) in f.expected.into_iter().enumerate() {
        let actual = f.m.cpu.read_u32(DATA + i as u32 * 8).unwrap();
        if let Some(value) = value {
            assert_eq!(actual, value, "{name} row {i}");
        } else {
            assert_ne!(actual, 0, "{name} row {i}");
        }
        assert_eq!(
            f.m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
            MARK,
            "{name} LastError row {i}"
        );
    }
    let data = f.m.cpu.read_u32(DATA + 0x800).unwrap();
    for (offset, value) in [
        (0, (-3i32) as u32),
        (4, (-2i32) as u32),
        (8, 7),
        (12, 9),
        (16, data),
        (20, data + 4),
        (24, data + 8),
        (28, MARK),
    ] {
        assert_eq!(
            f.m.cpu.read_u32(OUT + offset).unwrap(),
            value,
            "bound/index {offset}"
        );
    }
    for (offset, value) in [
        (4, 0x2003),
        (8, 42),
        (20, 0x3042),
        (24, 2),
        (28, 1),
        (32, 0),
    ] {
        assert_eq!(
            f.m.cpu.read_u32(DATA + 0x800 + offset).unwrap(),
            value,
            "ownership {offset}"
        );
    }
    assert_ne!(
        f.m.cpu.read_u32(DATA + 0x80c).unwrap(),
        f.m.cpu.read_u32(DATA + 0x810).unwrap()
    );
}
#[test]
// spec: SYS-AUTOMATION-1
fn array_bounds_copies_locks_and_yielding_com_elements_share_ownership_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
