use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CONTEXT: u32 = DATA + 0x800;
const HASH: u32 = DATA + 0x804;
const COPY: u32 = DATA + 0x808;
const LENGTH: u32 = DATA + 0x80c;
const TEXT: u32 = DATA + 0x900;
const OUTPUT: u32 = DATA + 0xa00;
const MARKER: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    At(u32),
}
fn lifecycle<B: Backend>(cpu: B, wide: bool) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/crypto-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(TEXT, b"abc").unwrap();
    m.cpu.write(OUTPUT, &[0xcd; 256]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    use Arg::{At, Value as V};
    let mut call = |name, args: &[Arg], value, error| {
        code.push_imm(MARKER).call(set_error);
        for arg in args.iter().rev() {
            match arg {
                V(value) => {
                    code.push_imm(*value);
                }
                At(pointer) => {
                    code.mov_eax_mem(*pointer).push_eax();
                }
            }
        }
        let function = m.resolve_import("advapi32.dll", name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.call(function)
            .mov_mem_eax(row)
            .call(get_error)
            .mov_mem_eax(row + 4);
        expected.push((value, error));
    };
    call(
        if wide {
            "CryptAcquireContextW"
        } else {
            "CryptAcquireContextA"
        },
        &[V(CONTEXT), V(0), V(0), V(24), V(0xf0000000)],
        1,
        MARKER,
    );
    call("CryptContextAddRef", &[At(CONTEXT), V(0), V(0)], 1, MARKER);
    call("CryptReleaseContext", &[At(CONTEXT), V(0)], 1, MARKER);
    call(
        "CryptCreateHash",
        &[At(CONTEXT), V(0x800c), V(0), V(0), V(HASH)],
        1,
        MARKER,
    );
    call("CryptHashData", &[At(HASH), V(TEXT), V(2), V(0)], 1, MARKER);
    call(
        "CryptDuplicateHash",
        &[At(HASH), V(0), V(0), V(COPY)],
        1,
        MARKER,
    );
    // Short outputs and unreadable inputs must leave the stream writable.
    call(
        "CryptGetHashParam",
        &[At(HASH), V(2), V(OUTPUT), V(LENGTH), V(0)],
        0,
        234,
    );
    call(
        "CryptHashData",
        &[At(HASH), V(0xfffffff0), V(32), V(0)],
        0,
        87,
    );
    call(
        "CryptHashData",
        &[At(HASH), V(TEXT + 2), V(1), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptGetHashParam",
        &[At(HASH), V(2), V(OUTPUT), V(LENGTH), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptHashData",
        &[At(HASH), V(TEXT), V(0), V(0)],
        0,
        super::BAD_HASH_STATE,
    );
    call(
        "CryptGetHashParam",
        &[At(COPY), V(2), V(OUTPUT + 64), V(LENGTH), V(0)],
        1,
        MARKER,
    );
    call("CryptDestroyHash", &[At(HASH)], 1, MARKER);
    call("CryptDestroyHash", &[At(HASH)], 0, super::BAD_HASH);
    call("CryptReleaseContext", &[At(CONTEXT), V(0)], 1, MARKER);
    call(
        "CryptCreateHash",
        &[At(CONTEXT), V(0x800c), V(0), V(0), V(HASH)],
        0,
        super::BAD_UID,
    );
    call("CryptDestroyHash", &[At(COPY)], 1, MARKER);
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("hash lifecycle", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (value, error)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8).unwrap(),
            value,
            "call {i}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
            error,
            "error {i}"
        );
    }
    let mut actual = [0; 128];
    m.cpu.read(OUTPUT, &mut actual).unwrap();
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(&actual[..32]),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&actual[64..96]),
        "fb8e20fc2e4c3f248c60c39bd652f3c1347298bb977b8b4d5903b85055620603"
    );
    assert_eq!(&actual[32..64], &[0xcd; 32]);
    assert_eq!(&actual[96..], &[0xcd; 32]);
    assert_eq!(m.cpu.read_u32(LENGTH).unwrap(), 32);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: PERS-CRYPTO-1, PERS-CRYPTO-2
fn crypto_hashes_preserve_lifecycle_on_all_backends() {
    for wide in [false, true] {
        for jit in [false, true] {
            let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
            cpu.set_async_compilation(false);
            lifecycle(cpu, wide);
        }
        lifecycle(UnicornCpu::new(PARK_ADDR).unwrap(), wide);
    }
}
