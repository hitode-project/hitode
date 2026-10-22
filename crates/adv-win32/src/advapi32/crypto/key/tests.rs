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
const KEY: u32 = DATA + 0x804;
const COPY: u32 = DATA + 0x808;
const HASH: u32 = DATA + 0x80c;
const DERIVED: u32 = DATA + 0x810;
const HMAC: u32 = DATA + 0x814;
const HMAC_KEY: u32 = DATA + 0x818;
const BLOB: u32 = DATA + 0x900;
const HMAC_BLOB: u32 = DATA + 0x940;
const HMAC_INFO: u32 = DATA + 0x980;
const MESSAGE: u32 = DATA + 0x9a0;
const PLAIN: u32 = DATA + 0xa00;
const OUTPUT: u32 = DATA + 0xb00;
const DERIVED_BLOB: u32 = DATA + 0xc00;
const HMAC_OUTPUT: u32 = DATA + 0xd00;
const MARKER: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    V(u32),
    At(u32),
}
fn session_keys<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/crypto-key-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let key: Vec<u8> = (0..16).collect();
    let mut blob = vec![8, 2, 0, 0];
    blob.extend_from_slice(&0x660eu32.to_le_bytes());
    blob.extend_from_slice(&16u32.to_le_bytes());
    blob.extend_from_slice(&key);
    m.cpu.write(BLOB, &blob).unwrap();
    let plain: Vec<u8> = (0..16).map(|i| i * 17).collect();
    m.cpu.write(PLAIN, &plain).unwrap();
    m.cpu.write(PLAIN + 16, &[0xcd; 64]).unwrap();
    let mut hmac_blob = vec![8, 2, 0, 0];
    hmac_blob.extend_from_slice(&0x6602u32.to_le_bytes());
    hmac_blob.extend_from_slice(&20u32.to_le_bytes());
    hmac_blob.extend_from_slice(&[0x0b; 20]);
    m.cpu.write(HMAC_BLOB, &hmac_blob).unwrap();
    m.cpu.write_u32(HMAC_INFO, 0x800c).unwrap();
    m.cpu.write(MESSAGE, b"Hi There").unwrap();
    // Separate size variables keep size-only queries from changing the later input lengths.
    for (offset, value) in [
        (0x820, 16),
        (0x824, 16),
        (0x828, 16),
        (0x82c, 32),
        (0x830, 28),
        (0x834, 64),
        (0x838, 32),
        (0x83c, 2),
        (0x840, 16),
        (0x844, 64),
        (0x848, 64),
    ] {
        m.cpu.write_u32(DATA + offset, value).unwrap();
    }
    m.cpu.write(OUTPUT, &[0xcd; 64]).unwrap();
    m.cpu.write(DERIVED_BLOB, &[0xcd; 64]).unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    use Arg::{At, V};
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
        "CryptAcquireContextA",
        &[V(CONTEXT), V(0), V(0), V(24), V(0xf0000000)],
        1,
        MARKER,
    );
    call(
        "CryptGenRandom",
        &[At(CONTEXT), V(16), V(OUTPUT + 48)],
        1,
        MARKER,
    );
    call("CryptGenRandom", &[At(CONTEXT), V(0), V(0)], 1, MARKER);
    call(
        "CryptGenRandom",
        &[V(0), V(16), V(OUTPUT + 48)],
        0,
        super::super::BAD_UID,
    );
    call(
        "CryptGenRandom",
        &[At(CONTEXT), V(16), V(0xfffffff0)],
        0,
        87,
    );
    call(
        "CryptGenKey",
        &[At(CONTEXT), V(0x6610), V(1), V(DERIVED)],
        1,
        MARKER,
    );
    call(
        "CryptExportKey",
        &[
            At(DERIVED),
            V(0),
            V(8),
            V(0),
            V(DATA + 0xe00),
            V(DATA + 0x848),
        ],
        1,
        MARKER,
    );
    call("CryptDestroyKey", &[At(DERIVED)], 1, MARKER);
    call(
        "CryptImportKey",
        &[
            At(CONTEXT),
            V(BLOB),
            V(blob.len() as u32),
            V(0),
            V(1),
            V(KEY),
        ],
        1,
        MARKER,
    );
    call(
        "CryptSetKeyParam",
        &[At(KEY), V(4), V(DATA + 0x83c), V(0)],
        1,
        MARKER,
    ); // ECB
    call(
        "CryptEncrypt",
        &[At(KEY), V(0), V(1), V(0), V(0), V(DATA + 0x820), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptEncrypt",
        &[At(KEY), V(0), V(1), V(0), V(PLAIN), V(DATA + 0x824), V(16)],
        0,
        234,
    );
    call(
        "CryptEncrypt",
        &[At(KEY), V(0), V(0), V(0), V(PLAIN), V(DATA + 0x828), V(64)],
        1,
        MARKER,
    );
    call(
        "CryptDuplicateKey",
        &[At(KEY), V(0), V(0), V(COPY)],
        1,
        MARKER,
    );
    call(
        "CryptExportKey",
        &[At(COPY), V(0), V(8), V(0), V(OUTPUT), V(DATA + 0x830)],
        1,
        MARKER,
    );
    call(
        "CryptDecrypt",
        &[At(COPY), V(0), V(0), V(0), V(PLAIN), V(DATA + 0x828)],
        1,
        MARKER,
    );
    call(
        "CryptGetKeyParam",
        &[At(KEY), V(9), V(OUTPUT + 32), V(DATA + 0x834), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptCreateHash",
        &[At(CONTEXT), V(0x800c), V(0), V(0), V(HASH)],
        1,
        MARKER,
    );
    call(
        "CryptHashData",
        &[At(HASH), V(MESSAGE), V(8), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptDeriveKey",
        &[At(CONTEXT), V(0x6610), At(HASH), V(1), V(DERIVED)],
        1,
        MARKER,
    );
    call(
        "CryptExportKey",
        &[
            At(DERIVED),
            V(0),
            V(8),
            V(0),
            V(DERIVED_BLOB),
            V(DATA + 0x844),
        ],
        1,
        MARKER,
    );
    call(
        "CryptImportKey",
        &[
            At(CONTEXT),
            V(HMAC_BLOB),
            V(hmac_blob.len() as u32),
            V(0),
            V(0x100),
            V(HMAC_KEY),
        ],
        1,
        MARKER,
    );
    call(
        "CryptCreateHash",
        &[At(CONTEXT), V(0x8009), At(HMAC_KEY), V(0), V(HMAC)],
        1,
        MARKER,
    );
    call(
        "CryptSetHashParam",
        &[At(HMAC), V(5), V(HMAC_INFO), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptHashData",
        &[At(HMAC), V(MESSAGE), V(8), V(0)],
        1,
        MARKER,
    );
    call(
        "CryptGetHashParam",
        &[At(HMAC), V(2), V(HMAC_OUTPUT), V(DATA + 0x838), V(0)],
        1,
        MARKER,
    );
    call("CryptDestroyHash", &[At(HMAC)], 1, MARKER);
    call("CryptDestroyHash", &[At(HASH)], 1, MARKER);
    for pointer in [KEY, COPY, DERIVED, HMAC_KEY] {
        call("CryptDestroyKey", &[At(pointer)], 1, MARKER);
        call("CryptDestroyKey", &[At(pointer)], 0, 0x80090003);
    }
    call("CryptReleaseContext", &[At(CONTEXT), V(0)], 1, MARKER);
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("session keys", CODE, &[], 65536).unwrap();
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
    let mut actual = [0; 80];
    m.cpu.read(PLAIN, &mut actual).unwrap();
    assert_eq!(&actual[..16], &plain);
    assert_eq!(&actual[16..], &[0xcd; 64]);
    assert_eq!(m.cpu.read_u32(DATA + 0x820).unwrap(), 32);
    assert_eq!(m.cpu.read_u32(DATA + 0x824).unwrap(), 32);
    let mut exported = [0; 28];
    m.cpu.read(OUTPUT, &mut exported).unwrap();
    assert_eq!(&exported, blob.as_slice());
    assert_eq!(m.cpu.read_u32(OUTPUT + 32).unwrap(), 128);
    let mut derived = [0; 44];
    m.cpu.read(DERIVED_BLOB, &mut derived).unwrap();
    assert_eq!(&derived[..12], &[8, 2, 0, 0, 0x10, 0x66, 0, 0, 32, 0, 0, 0]);
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(
        hex(&derived[12..]),
        "cc6d5896d770101ef0280c943a2d3c3f24cd5b11464a5186daf7a238477162ac"
    );
    let mut hmac = [0; 32];
    m.cpu.read(HMAC_OUTPUT, &mut hmac).unwrap();
    assert_eq!(
        hex(&hmac),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    let mut random = [0; 16];
    m.cpu.read(OUTPUT + 48, &mut random).unwrap();
    assert_ne!(random, [0xcd; 16]);
    let mut random_blob = [0; 44];
    m.cpu.read(DATA + 0xe00, &mut random_blob).unwrap();
    assert_eq!(
        &random_blob[..12],
        &[8, 2, 0, 0, 0x10, 0x66, 0, 0, 32, 0, 0, 0]
    );
    assert_ne!(random_blob[12..], [0; 32]);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: SYS-CRYPTO-2, SYS-CRYPTO-3, SYS-CRYPTO-4, SYS-CRYPTO-5
fn crypto_key_lifecycles_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        session_keys(cpu);
    }
    session_keys(UnicornCpu::new(PARK_ADDR).unwrap());
}
