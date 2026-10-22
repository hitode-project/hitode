//! Closure scalar conversions preserve exact decimal boundaries and shared BSTR ownership.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_os::com::{free_bstr, owns_bstr};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const MARK: u32 = 0x12345678;
enum Value {
    Bytes(Vec<u8>),
    Text(String),
    Failure(u32),
}
struct Fixture<B: Backend> {
    m: Machine<B>,
    code: Asm,
    cases: Vec<(String, Value)>,
}
impl<B: Backend> Fixture<B> {
    fn call(&mut self, name: &str, args: &[u32], value: Value) {
        let i = self.cases.len() as u32;
        let out = DATA + 0x1000 + i * 16;
        self.m.cpu.write(out, &[0xcd; 16]).unwrap();
        let set = self
            .m
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap();
        let get = self
            .m
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        let api = self.m.resolve_import("oleaut32.dll", name).unwrap();
        self.code.push_imm(MARK).call(set).push_imm(out);
        for arg in args.iter().rev() {
            self.code.push_imm(*arg);
        }
        self.code
            .call(api)
            .mov_mem_eax(DATA + i * 8)
            .call(get)
            .mov_mem_eax(DATA + i * 8 + 4);
        self.cases.push((name.into(), value));
    }
    fn string(&mut self, name: &str, text: &str, lcid: u32, flags: u32, value: Value) {
        let input = DATA + 0x3000 + self.cases.len() as u32 * 256;
        let bytes = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        self.m.cpu.write(input, &bytes).unwrap();
        self.call(name, &[input, lcid, flags], value);
    }
    fn bstr(&mut self, name: &str, value: u64, wide: bool, lcid: u32, flags: u32, text: &str) {
        let args = if wide {
            vec![value as u32, (value >> 32) as u32, lcid, flags]
        } else {
            vec![value as u32, lcid, flags]
        };
        self.call(name, &args, Value::Text(text.into()));
    }
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-scalar-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 65536, Prot::RW).unwrap();
    let mut f = Fixture {
        m,
        code: Asm::new(CODE),
        cases: Vec::new(),
    };
    for (text, float, currency) in [
        ("1,234.5", 1234.5f64, 12345000i64),
        ("(12.5)", -12.5, -125000),
        ("1e3", 1000.0, 10000000),
        ("&H10", 16.0, 160000),
        ("$1.5", 1.5, 15000),
        ("0.00005", 0.00005, 0),
        ("0.00015", 0.00015, 2),
    ] {
        f.string(
            "VarR8FromStr",
            text,
            0x409,
            0,
            Value::Bytes(float.to_le_bytes().to_vec()),
        );
        f.string(
            "VarCyFromStr",
            text,
            0x409,
            0,
            Value::Bytes(currency.to_le_bytes().to_vec()),
        );
    }
    for (text, value) in [
        ("922337203685477.5807", i64::MAX),
        ("-922337203685477.5808", i64::MIN),
    ] {
        f.string(
            "VarCyFromStr",
            text,
            0x409,
            0,
            Value::Bytes(value.to_le_bytes().to_vec()),
        );
    }
    for (text, value) in [
        ("2.5", 2i32),
        ("1.5", 2),
        ("-1.5", -2),
        ("1.49999999999999999", 1),
        ("2147483647", i32::MAX),
    ] {
        f.string(
            "VarI4FromStr",
            text,
            0x409,
            0,
            Value::Bytes(value.to_le_bytes().to_vec()),
        );
    }
    f.string(
        "VarUI4FromStr",
        "4294967295",
        0x409,
        0,
        Value::Bytes(u32::MAX.to_le_bytes().to_vec()),
    );
    f.string(
        "VarR4FromStr",
        "1.25",
        0x409,
        0,
        Value::Bytes(1.25f32.to_le_bytes().to_vec()),
    );
    for (text, value) in [
        ("TrUe", -1i16),
        ("false", 0),
        ("0", 0),
        ("-2", -1),
        ("1e-999", 0),
    ] {
        f.string(
            "VarBoolFromStr",
            text,
            0x409,
            0,
            Value::Bytes(value.to_le_bytes().to_vec()),
        );
    }
    for (locale, value, text) in [
        (0x409, 0.0, "12:00:00 AM"),
        (0x411, 0.5, "12:00:00"),
        (0x409, -1.25, "12/29/1899 6:00:00 AM"),
        (0x411, -1.25, "1899/12/29 6:00:00"),
        (0x409, 45292.0, "1/1/2024"),
        (0x411, 45292.0, "2024/01/01"),
        (0x804, 45292.0, "2024/1/1"),
    ] {
        f.bstr(
            "VarBstrFromDate",
            f64::to_bits(value),
            true,
            locale,
            0,
            text,
        );
    }
    for (locale, text, value) in [
        (0x409, "2/29/2024", 45351.0f64),
        (0x411, "2024年2月29日", 45351.0),
        (0x804, "2024/2/29", 45351.0),
        (0x409, "12/29/1899 6:00 AM", -1.25),
        (0x409, "6:00 PM", 0.75),
    ] {
        f.string(
            "VarDateFromStr",
            text,
            locale,
            0,
            Value::Bytes(value.to_le_bytes().to_vec()),
        );
    }
    for locale in [0x409, 0x411, 0x804] {
        for (value, text) in [(0, "False"), (7, "True"), (-1i32 as u32, "True")] {
            f.bstr(
                "VarBstrFromBool",
                u64::from(value),
                false,
                locale,
                0x10,
                text,
            );
        }
    }
    for (value, text) in [
        (12500i64, "1.25"),
        (i64::MIN, "-922337203685477.5808"),
        (i64::MAX, "922337203685477.5807"),
    ] {
        f.bstr("VarBstrFromCy", value as u64, true, 0x409, 0, text);
    }
    for (name, text, error) in [
        ("VarI4FromStr", "2147483648", 0x8002000a),
        ("VarUI4FromStr", "-1", 0x8002000a),
        ("VarCyFromStr", "922337203685477.5808", 0x8002000a),
        ("VarR8FromStr", "1e999", 0x8002000a),
        ("VarR4FromStr", "1e39", 0x8002000a),
        ("VarI4FromStr", "wrong", 0x80020005),
        ("VarDateFromStr", "2/29/2023", 0x80020005),
    ] {
        f.string(name, text, 0x409, 0, Value::Failure(error));
    }
    f.call("VarR8FromStr", &[0, 0x409, 0], Value::Failure(0x80070057));
    f.code
        .mov_mem_imm(DATA + 0xff00, 0)
        .mov_eax_mem(DATA + 0xff00)
        .ret();
    f.m.cpu.write(CODE, &f.code.bytes).unwrap();
    f.m.spawn("Automation scalar closure", CODE, &[], 65536)
        .unwrap();
    let result = f.m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value)) in f.cases.into_iter().enumerate() {
        let i = i as u32;
        let status = f.m.cpu.read_u32(DATA + i * 8).unwrap();
        let out = DATA + 0x1000 + i * 16;
        assert_eq!(
            f.m.cpu.read_u32(DATA + i * 8 + 4).unwrap(),
            MARK,
            "{name} LastError {i}"
        );
        match value {
            Value::Bytes(expected) => {
                assert_eq!(status, 0, "{name} row {i}");
                let mut actual = vec![0; expected.len()];
                f.m.cpu.read(out, &mut actual).unwrap();
                assert_eq!(actual, expected, "{name} row {i}");
            }
            Value::Text(expected) => {
                assert_eq!(status, 0, "{name} row {i}");
                let pointer = f.m.cpu.read_u32(out).unwrap();
                let n = f.m.cpu.read_u32(pointer - 4).unwrap();
                let mut raw = vec![0; n as usize];
                f.m.cpu.read(pointer, &mut raw).unwrap();
                let actual = String::from_utf16(
                    &raw.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes(*b))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
                assert_eq!(actual, expected, "{name} row {i}");
                let mut env = Env {
                    context: f.m.sys.current,
                    cpu: &mut f.m.cpu,
                    sys: &mut f.m.sys,
                    ext: &mut f.m.ext,
                    thunk: 0,
                };
                assert!(owns_bstr(&env, pointer));
                free_bstr(&mut env, pointer);
                assert!(!owns_bstr(&env, pointer));
            }
            Value::Failure(error) => {
                assert_eq!(status, error, "{name} row {i}");
                let mut actual = [0; 16];
                f.m.cpu.read(out, &mut actual).unwrap();
                assert_eq!(actual, [0xcd; 16], "{name} failure output {i}");
            }
        }
    }
}
#[test]
// spec: SYS-AUTOMATION-2
fn scalar_locale_date_currency_rounding_and_failures_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
