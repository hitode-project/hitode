use super::super::tests::{call, env, fixture, machine};
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const MODULE: &str = "msvcr90.dll";
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
fn pointer<B: Backend>(machine: &mut Machine<B>, entry: u32) -> u32 {
    match get(&mut env(machine, entry)) {
        Outcome::Return(pointer) => pointer,
        _ => panic!("supported ctype query must return its table"),
    }
}
fn bytes<B: Backend>(machine: &Machine<B>, pointer: u32) -> [u8; 514] {
    let mut bytes = [0; 514];
    machine.cpu.read(pointer - 2, &mut bytes).unwrap();
    bytes
}

#[test]
fn pctype_profiles_match_all_257_words_from_precise_vc90_oracle() {
    // Independent VC90 captures, including EOF, all controls and high bytes.
    for (cp, expected) in [
        (0, 0x48cabdc90fcb0865),
        (932, 0x44eb5be34d5b7579),
        (936, 0xe33c618c172b2971),
        (1252, 0x006150ad6479dba0),
    ] {
        let words = table(cp).unwrap();
        assert_eq!(words[0], 0);
        let bytes = words
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(hash(&bytes), expected, "cp={cp}");
    }
    assert!(table(65001).is_none());
}

#[test]
fn pctype_cached_profiles_follow_ctype_not_acp_and_have_readonly_module_lifetime() {
    let (mut machine, set_entry) = fixture();
    let entry = machine.resolve_import(MODULE, "__pctype_func").unwrap();
    let first = pointer(&mut machine, entry);
    assert_eq!(hash(&bytes(&machine, first)), 0x48cabdc90fcb0865);
    let errno = floating::errno_address(&mut env(&mut machine, entry));
    errno.write(&mut machine.cpu, 79).unwrap();
    crate::util::set_last_error(&mut env(&mut machine, entry), 0x12345678);
    let mut profiles = Vec::new();
    for (requested, expected) in [
        ("C", 0x48cabdc90fcb0865),
        ("Japanese_Japan.932", 0x44eb5be34d5b7579),
        ("Chinese (Simplified)_China.936", 0xe33c618c172b2971),
        ("English_United States.1252", 0x006150ad6479dba0),
    ] {
        call(&mut machine, set_entry, 2, Some(requested));
        let address = pointer(&mut machine, entry);
        assert_eq!(pointer(&mut machine, entry), address);
        assert!(!profiles.contains(&address));
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address - 2)
                .is_some()
        );
        assert_eq!(hash(&bytes(&machine, address)), expected);
        assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
        assert_eq!(
            crate::util::last_error(&env(&mut machine, entry)),
            0x12345678
        );
        profiles.push(address);
    }
    assert_eq!(machine.ext.get::<crate::Win32>().codec.acp, 932);
    // Changing another category or language keeps the CP932 classification.
    call(
        &mut machine,
        set_entry,
        2,
        Some("English_United States.932"),
    );
    assert_eq!(pointer(&mut machine, entry), profiles[1]);
    call(&mut machine, set_entry, 5, Some("Chinese (Simplified)"));
    assert_eq!(pointer(&mut machine, entry), profiles[1]);
    assert_eq!(hash(&bytes(&machine, profiles[0])), 0x48cabdc90fcb0865);
    let before = bytes(&machine, profiles[1]);
    machine
        .cpu
        .protect((profiles[1] - 2) & !4095, 4096, Prot::READ)
        .unwrap();
    assert_eq!(pointer(&mut machine, entry), profiles[1]);
    assert_eq!(bytes(&machine, profiles[1]), before);
    machine
        .cpu
        .protect((profiles[1] - 2) & !4095, 4096, Prot::RW)
        .unwrap();
    let other = machine.resolve_import("msvcr80.dll", "_errno").unwrap();
    let other_pointer = pointer(&mut machine, other);
    assert!(!profiles.contains(&other_pointer));
    assert_eq!(hash(&bytes(&machine, other_pointer)), 0x48cabdc90fcb0865);
    // The guards reject an unknown future profile, never return a stale table.
    machine
        .ext
        .get_mut::<CrtState>()
        .locales
        .get_mut(MODULE)
        .unwrap()
        .categories[1]
        .codepage = 65001;
    assert_eq!(pointer(&mut machine, entry), 0);
    assert_eq!(
        machine.ext.get::<CrtState>().locales[MODULE]
            .ctype_tables
            .len(),
        4
    );
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert!(
        diagnostics
            .warnings
            .keys()
            .next()
            .unwrap()
            .1
            .contains("65001")
    );
    let locale_buffer = machine.ext.get::<CrtState>().locales[MODULE].buffer;
    super::super::super::detach_thread(&mut env(&mut machine, entry)).unwrap();
    for &address in &profiles {
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address - 2)
                .is_some()
        );
    }
    super::super::release_process_tables(&mut env(&mut machine, entry)).unwrap();
    for address in profiles.into_iter().chain([other_pointer]) {
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address - 2)
                .is_none()
        );
    }
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, locale_buffer)
            .is_none()
    );
    assert!(machine.ext.get::<CrtState>().locales.is_empty());
    super::super::release_process_tables(&mut env(&mut machine, entry)).unwrap();
}

fn guest_table<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let ctype = machine.resolve_import(MODULE, "__pctype_func").unwrap();
    let calloc = machine.resolve_import(MODULE, "_calloc_crt").unwrap();
    let copy = machine.resolve_import(MODULE, "memcpy").unwrap();
    let release = machine.resolve_import(MODULE, "free").unwrap();
    let mut code = Asm::new(CODE);
    // Actual adjacent game path: allocate512 then copy256 WORDs from getter.
    code.push_imm(2)
        .push_imm(256)
        .call(calloc)
        .add_esp(8)
        .mov_mem_eax(DATA);
    code.call(ctype).mov_mem_eax(DATA + 4);
    code.bytes.extend_from_slice(&[0x0f, 0xb7, 0x40, 0xfe]); // EOF at p[-1].
    code.mov_mem_eax(DATA + 8);
    code.push_imm(512);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&(DATA + 4).to_le_bytes());
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.call(copy).add_esp(12);
    code.push_imm(512);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.push_imm(DATA + 512).call(copy).add_esp(12);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.call(release).add_esp(4);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT pctype cdecl getter and512byte copy", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 0);
    let mut actual = [0; 514];
    machine.cpu.read(DATA + 512, &mut actual[2..]).unwrap();
    assert_eq!(hash(&actual), 0x48cabdc90fcb0865);
    let allocation = machine.cpu.read_u32(DATA).unwrap();
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, allocation)
            .is_none()
    );
}
#[test]
fn pctype_actual_cdecl_getter_eof_bias_and512byte_copy_run_in_interpreter_and_jit() {
    guest_table(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_table(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn guest_string_type<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let classify = machine
        .resolve_import(MODULE, "__crtGetStringTypeW")
        .unwrap();
    machine.cpu.write(DATA, &[0x41, 0, 0x00, 0x30]).unwrap();
    let mut code = Asm::new(CODE);
    // One-character CT_CTYPE1 query with page and LCID, as libcpmt passes them.
    for (character, mask) in [(DATA, DATA + 8), (DATA + 2, DATA + 10)] {
        code.push_imm(0x411)
            .push_imm(932)
            .push_imm(mask)
            .push_imm(1)
            .push_imm(character)
            .push_imm(1)
            .push_imm(0)
            .call(classify)
            .add_esp(28);
    }
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT wide string type", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(1)), "{result:?}");
    // C1_UPPER|C1_XDIGIT|C1_ALPHA|C1_DEFINED, then C1_SPACE|C1_BLANK|C1_DEFINED.
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 0x0248_0381);
}
#[test]
fn crt_string_type_w_is_a_seven_argument_cdecl_ctype1_query_in_interpreter_and_jit() {
    guest_string_type(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_string_type(JitCpu::new(PARK_ADDR, false).unwrap());
}
