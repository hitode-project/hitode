use super::*;
use adv_abi::code_signature::{FunctionSnapshot, Profile};
use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_machine::layout::thunk_addr;
use adv_machine::layout::{PAGE, PARK_ADDR};
use adv_machine::{Clock, RunResult};
use adv_machine::{HostFn, Machine, Resolver};

struct Platform;
impl Resolver for Platform {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if module == "test" && name == "done" {
            return Some(HostFn {
                conv: Conv::Cdecl { argc: 0 },
                handler: |_| Outcome::ExitProcess(0),
                data: 0,
                trace: false,
                guest: None,
                leaf: None,
            });
        }
        super::super::host_fn(module, name)
    }
}

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const ENTRY: u32 = CODE + 0x200;
const LOOKUP: u32 = DATA + 0x100;
const TRIG: u32 = DATA + 0x1000;
const BITREV: u32 = DATA + 0x2000;
const INPUT: u32 = DATA + 0x3000;
const OUTPUT: u32 = DATA + 0x4000;
const RESULT: u32 = 0xabcd_1234;

/// An authored ABI fixture, not a codec fingerprint. It writes one sample,
/// raises a masked x87 divide-by-zero, and returns a diagnostic EAX value.
fn fixture(mode: Mode) -> Machine<JitCpu> {
    fixture_abi(mode, false)
}

fn fixture_abi(mode: Mode, registers: bool) -> Machine<JitCpu> {
    fixture_cpu(mode, registers, false)
}

fn fixture_cpu(mode: Mode, registers: bool, jit: bool) -> Machine<JitCpu> {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, PAGE, Prot::RX).unwrap();
    m.cpu.map(DATA, PAGE * 8, Prot::RW).unwrap();
    let mut body = Asm::new(ENTRY);
    body.bytes.extend_from_slice(&[
        0x83, 0xec, 0x20, // sub esp,32
        0x53, // push ebx
        0x8b, 0x5c, 0x24, 0x28, // mov ebx,[esp+40]
        0x8b, 0x4c, 0x24, 0x30, // mov ecx,[esp+48] (output)
        0xc7, 0x01, // mov dword [ecx],42.0f
    ]);
    body.bytes.extend_from_slice(&42.0f32.to_le_bytes());
    body.bytes.extend_from_slice(&[
        0xd9, 0xe8, // fld1
        0xd9, 0xee, // fldz
        0xde, 0xf9, // fdivp st(1),st(0)
        0xdd, 0xd8, // fstp st(0)
        0xb8, // mov eax,RESULT
    ]);
    body.bytes.extend_from_slice(&RESULT.to_le_bytes());
    body.bytes.push(0x5b); // pop ebx
    body.add_esp(32).ret();
    if registers {
        // Authored register ABI: output = lookup.n + input[0] bits.
        body.bytes.truncate(8);
        body.bytes
            .extend_from_slice(&[0x8b, 0x01, 0x03, 0x02, 0x89, 0x03]);
        body.bytes.push(0xb8);
        body.bytes.extend_from_slice(&RESULT.to_le_bytes());
        body.bytes.push(0x5b);
        body.add_esp(32).ret();
    }
    m.cpu.write(ENTRY, &body.bytes).unwrap();
    let matched = Match {
        entry: ENTRY,
        prologue_len: 8,
        profile: Profile {
            kernel: if registers { REGISTER_KERNEL } else { KERNEL }.into(),
            properties: BTreeMap::from([("table_pi_bits".into(), TABLE_PI_BITS)]),
        },
        functions: vec![FunctionSnapshot {
            role: "main".into(),
            entry: ENTRY,
            bytes: body.bytes.clone(),
        }],
        constants: Vec::new(),
    };
    install(&mut m.ext, mode);
    m.visit_context(None, |e| bind_one(e, CODE, matched))
        .unwrap()
        .unwrap();
    let target = if jit {
        ENTRY
    } else {
        thunk_addr(m.ext.get::<Provider>().bindings[0].entry.thunk)
    };
    let tables = StandardTables::new(64, registers);
    let trig: Vec<_> = tables.trig.iter().flat_map(|f| f.to_le_bytes()).collect();
    let bitrev: Vec<_> = tables.bitrev.iter().flat_map(|i| i.to_le_bytes()).collect();
    m.cpu.write(TRIG, &trig).unwrap();
    m.cpu.write(BITREV, &bitrev).unwrap();
    for (offset, value) in [
        (0, 64),
        (4, 6),
        (8, TRIG),
        (12, BITREV),
        (16, (4.0f32 / 64.0).to_bits()),
    ] {
        m.cpu.write_u32(LOOKUP + offset, value).unwrap();
    }
    let done = m.resolve_import("test", "done").unwrap();
    let mut caller = Asm::new(CODE);
    for _ in 0..if jit { 8 } else { 1 } {
        caller.push_imm(OUTPUT);
        if registers {
            caller.mov_ecx_imm(LOOKUP);
            caller.bytes.push(0xba); // mov edx,INPUT
            caller.bytes.extend_from_slice(&INPUT.to_le_bytes());
        } else {
            caller.push_imm(INPUT).push_imm(LOOKUP);
        }
        caller
            .call(target)
            .add_esp(if registers { 4 } else { 12 })
            .mov_mem_eax(DATA);
    }
    caller.call(done);
    m.cpu.write(CODE, &caller.bytes).unwrap();
    m.spawn("codec ABI fixture", CODE, &[], 0x10000).unwrap();
    m
}

// spec: DEV-AUDIO-4
// spec: CODEC-CALL-1
#[test]
fn register_abi_preserves_verify_and_fallback_arguments() {
    for (mode, bad_tables) in [
        (Mode::Native, false),
        (Mode::Verify, false),
        (Mode::Native, true),
    ] {
        let mut m = fixture_abi(mode, true);
        if bad_tables {
            m.cpu.write_u32(BITREV, 29).unwrap();
        }
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        assert_eq!(
            m.cpu.read_u32(DATA).unwrap(),
            if mode == Mode::Native && !bad_tables {
                0
            } else {
                RESULT
            }
        );
        let stats = stats(&m.ext).unwrap();
        if mode == Mode::Verify {
            assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 64);
            assert_eq!(stats.verify_calls, 1);
            assert_eq!(stats.compared_floats, 64);
        } else if bad_tables {
            assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 64);
            assert_eq!(stats.fallback_calls, 1);
        } else {
            assert_eq!(f32::from_bits(m.cpu.read_u32(OUTPUT).unwrap()), 0.0);
            assert_eq!(stats.native_calls, 1);
        }
    }
}

// spec: CODEC-CALL-1
#[test]
fn verify_keeps_original_output_eax_and_floating_exception_state() {
    let mut m = fixture(Mode::Verify);
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), RESULT);
    assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 42.0f32.to_bits());
    let architecture = m.cpu.capture_architecture().unwrap();
    assert_eq!(architecture.x87.control, 0x027f);
    assert_eq!(architecture.x87.tag, 0xffff);
    assert_ne!(architecture.x87.status & 4, 0);
    let stats = stats(&m.ext).unwrap();
    assert_eq!(
        (stats.native_calls, stats.verify_calls, stats.fallback_calls),
        (0, 1, 0)
    );
    assert_eq!(stats.compared_floats, 64);
    // FFT output also contains negative zeros: the diagnostic deliberately
    // counts bit differences even when their absolute numeric error is zero.
    assert!(stats.different_floats >= 1);
    assert_eq!(stats.max_abs, 42.0);
}

// spec: CODEC-CALL-1
#[test]
fn modified_original_trampoline_fails_before_following_a_recursive_old_jump() {
    let mut m = fixture(Mode::Native);
    let binding = &m.ext.get::<Provider>().bindings[0];
    let mut recursive = Asm::new(binding.entry.original);
    recursive.jmp(thunk_addr(binding.entry.thunk));
    m.cpu
        .write(binding.entry.original, &recursive.bytes)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    assert_eq!(stats(&m.ext).unwrap().errors, 1);
    assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 0);
}

// spec: CODEC-CALL-1
#[test]
fn changed_lookup_contents_fall_back_before_publishing_native_output() {
    let mut m = fixture(Mode::Native);
    m.cpu.write_u32(BITREV, 29).unwrap(); // standard first index is 30
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 42.0f32.to_bits());
    let stats = stats(&m.ext).unwrap();
    assert_eq!((stats.native_calls, stats.fallback_calls), (0, 1));
    assert_eq!(
        stats.last_rejection.as_deref(),
        Some("nonstandard lookup tables")
    );
}

// spec: CODEC-CALL-1
#[test]
fn reviewed_f32_pi_profile_has_the_initializers_table_values() {
    let tables = StandardTables::new(256, false);
    assert_eq!(tables.trig[0].to_bits(), 1.0f32.to_bits());
    assert_eq!(tables.trig[1].to_bits(), (-0.0f32).to_bits());
    // Independently observed standard initializer value. Using full f64 PI
    // gives 0x3e78cfcc and incorrectly rejects this verified f32 build.
    assert_eq!(tables.trig[54].to_bits(), 0x3e78_cfc9);
    assert_eq!(&tables.bitrev[..4], &[126, 0, 62, 64]);
    let sse = StandardTables::new(256, true);
    assert_eq!(sse.trig[44].to_bits(), 0x3ef1_5ae7);
    assert_ne!(sse.trig[44].to_bits(), tables.trig[44].to_bits());
    assert!(!ulp_match(
        f32::from_bits(0x3e78_cfc9),
        f32::from_bits(0x3e78_cfcc)
    ));
}

// spec: CODEC-CALL-1, DEV-AUDIO-4
#[test]
fn native_transform_matches_guest_dot_products() {
    // The device's authored unscaled-IMDCT oracle, evaluated by x87 guest code.
    for input in [
        vec![0.0f32; 32],
        {
            let mut values = vec![0.0; 32];
            values[0] = 1.0;
            values
        },
        (0..32).map(|i| (i as f32 * 0.7).sin() * 0.01).collect(),
    ] {
        let mut outputs = Vec::new();
        for mode in [Mode::Native, Mode::Verify] {
            let mut m = fixture(mode);
            let coefficients = DATA + 0x8000;
            m.cpu.map(coefficients, PAGE * 4, Prot::RW).unwrap();
            let mut bytes = Vec::new();
            for i in 0..64 {
                for j in 0..32 {
                    let value =
                        (std::f64::consts::PI / 128.0 * (2 * i + 33) as f64 * (2 * j + 1) as f64)
                            .cos();
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            m.cpu.write(coefficients, &bytes).unwrap();
            let mut body = Asm::new(ENTRY);
            body.bytes.extend_from_slice(&[
                0x83, 0xec, 0x20, 0x53, 0x8b, 0x5c, 0x24, 0x28, 0x56, 0x57,
                0x55, // save esi,edi,ebp
                0x8b, 0x7c, 0x24, 0x3c, // mov edi,[esp+60]
                0xbd, // mov ebp,coefficients
            ]);
            body.bytes.extend_from_slice(&coefficients.to_le_bytes());
            body.bytes.extend_from_slice(&[0xbb, 64, 0, 0, 0]); // mov ebx,64
            let outer = body.bytes.len();
            body.bytes
                .extend_from_slice(&[0x8b, 0x74, 0x24, 0x38, 0xb9, 32, 0, 0, 0, 0xd9, 0xee]);
            let inner = body.bytes.len();
            body.bytes.extend_from_slice(&[
                0xdd, 0x45, 0, // fld qword [ebp]
                0xd8, 0x0e, // fmul dword [esi]
                0xde, 0xc1, // faddp
                0x83, 0xc5, 8, 0x83, 0xc6, 4, 0x49, 0x75,
            ]);
            body.bytes
                .push((inner as i32 - body.bytes.len() as i32 - 1) as u8);
            body.bytes
                .extend_from_slice(&[0xd9, 0x1f, 0x83, 0xc7, 4, 0x4b, 0x75]);
            body.bytes
                .push((outer as i32 - body.bytes.len() as i32 - 1) as u8);
            body.bytes.extend_from_slice(&[0x5d, 0x5f, 0x5e, 0x5b]);
            body.add_esp(32).ret();
            let mut matched = m.ext.get::<Provider>().bindings[0].matched.clone();
            matched.functions[0].bytes = body.bytes.clone();
            m.cpu.write(ENTRY, &body.bytes).unwrap();
            install(&mut m.ext, mode);
            m.visit_context(None, |e| bind_one(e, CODE, matched))
                .unwrap()
                .unwrap();
            m.cpu
                .write(
                    INPUT,
                    &input
                        .iter()
                        .flat_map(|f| f.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            let result = m.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            let mut output = vec![0; 256];
            m.cpu.read(OUTPUT, &mut output).unwrap();
            outputs.push(output);
            if mode == Mode::Verify {
                let stats = stats(&m.ext).unwrap();
                assert_eq!(stats.compared_floats, 64);
                assert!(stats.max_abs < 3e-7, "{}", stats.max_abs);
            } else {
                assert_eq!(stats(&m.ext).unwrap().native_calls, 1);
            }
        }
        for (a, b) in outputs[0]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(outputs[1].as_chunks::<4>().0)
        {
            assert!((f32::from_le_bytes(*a) - f32::from_le_bytes(*b)).abs() < 3e-7);
        }
    }
}

// spec: CODEC-MAP-1, CODEC-CALL-1
#[test]
fn signature_admission_rejects_a_near_miss_without_patching_code() {
    for registers in [false, true] {
        for changed in [false, true] {
            let mut m = fixture_abi(Mode::Native, registers);
            let original = m.ext.get::<Provider>().bindings[0].matched.functions[0]
                .bytes
                .clone();
            install(&mut m.ext, Mode::Native);
            m.sys.redirects.clear();
            if changed {
                m.cpu.write_u32(ENTRY + 15, 0x12345678).unwrap();
            }
            let (length, hash) = if registers {
                (
                    24,
                    "f88352705623aed7bc3eb21f9b44a7675dc7fb7663355ccd88fe9609c305c139",
                )
            } else {
                (
                    36,
                    "5ed43c3cfe7b12ea48365a1bbccf79b0862337f033ed0b74d22f2501d7b6596c",
                )
            };
            let mut text = format!(
                "format 1\nid authored-mdct-abi\nsource authored ABI fixture\nlicense CC0 fixture\nprofile {}\ncandidate cdecl-stack32\nentry main\nfunction main {length} {hash} 8\n",
                if registers { REGISTER_KERNEL } else { KERNEL }
            );
            for (key, value) in [
                ("lookup_bytes", 20),
                ("n_offset", 0),
                ("log2n_offset", 4),
                ("trig_offset", 8),
                ("bitrev_offset", 12),
                ("scale_offset", 16),
                ("min_n", 64),
                ("max_n", 8192),
                ("table_pi_bits", TABLE_PI_BITS),
                ("table_angle_f32", 1),
            ] {
                text.push_str(&format!("property {key} {value}\n"));
            }
            m.visit_context(None, |e| {
                bind_signatures(
                    e,
                    CODE,
                    PAGE,
                    &[ExecutableRange {
                        start: CODE,
                        len: PAGE,
                    }],
                    &text,
                )
            })
            .unwrap()
            .unwrap();
            assert_eq!(stats(&m.ext).unwrap().installed, u64::from(!changed));
            assert_eq!(m.sys.redirects.len(), usize::from(!changed));
            if !changed {
                assert!(identical(&m.cpu, ENTRY, &original));
            }
        }
    }
}

// spec: CODEC-CALL-1
#[test]
fn decline_preserves_memory_and_architecture_before_guest_fallback() {
    for registers in [false, true] {
        let mut m = fixture_abi(Mode::Native, registers);
        let context = 0;
        // Materialize this context, then call the leaf directly with its ABI frame.
        m.visit_context(Some(context), |e| {
            let stack = e.cpu.reg(Reg::Esp) - 32;
            e.cpu.set_reg(Reg::Esp, stack);
            e.cpu.write_u32(stack, CODE).unwrap();
            e.cpu
                .write_u32(stack + 4, if registers { OUTPUT } else { LOOKUP })
                .unwrap();
            e.cpu.write_u32(stack + 8, INPUT).unwrap();
            e.cpu.write_u32(stack + 12, OUTPUT).unwrap();
            e.cpu.set_reg(Reg::Ecx, LOOKUP);
            e.cpu.set_reg(Reg::Edx, INPUT);
            e.thunk = e.ext.get::<Provider>().bindings[0].entry.thunk;
            e.cpu.write_u32(BITREV, 29).unwrap();
            let before = e.cpu.capture_architecture().unwrap();
            let mut memory = vec![0; (PAGE * 8) as usize];
            e.cpu.read(DATA, &mut memory).unwrap();
            assert_eq!(native(e), None);
            let after = e.cpu.capture_architecture().unwrap();
            assert_eq!(before, after);
            let mut actual = vec![0; memory.len()];
            e.cpu.read(DATA, &mut actual).unwrap();
            assert_eq!(memory, actual);
        })
        .unwrap();
    }
}

// spec: CODEC-CALL-1
#[test]
fn changed_closure_revokes_native_admission() {
    let mut m = fixture(Mode::Native);
    m.visit_context(Some(0), |e| {
        let thunk = e.ext.get::<Provider>().bindings[0].entry.thunk;
        e.thunk = thunk;
        let mut bytes = vec![0; 36];
        e.cpu.read(ENTRY, &mut bytes).unwrap();
        e.cpu.write(ENTRY + 35, &[0x90]).unwrap();
        assert_eq!(native(e), None);
        assert_eq!(e.ext.get::<Provider>().stats.revocations, 1);
        assert!(!e.sys.redirects.contains_key(&ENTRY));
        e.cpu.write(ENTRY, &bytes).unwrap();
        assert_eq!(native(e), None);
        assert_eq!(
            e.ext.get::<Provider>().stats.last_rejection.as_deref(),
            Some("binding revoked")
        );
        assert_eq!(e.cpu.read_u32(OUTPUT).unwrap(), 0);
    })
    .unwrap();
}

// spec: CODEC-CALL-1, NAT-BODY-1
#[cfg(target_arch = "aarch64")]
#[test]
fn translated_entries_use_redirects_and_declined_calls_use_trampolines() {
    for registers in [false, true] {
        for (mode, bad_tables) in [
            (Mode::Native, false),
            (Mode::Verify, false),
            (Mode::Native, true),
        ] {
            let mut m = fixture_cpu(mode, registers, true);
            if bad_tables {
                m.cpu.write_u32(BITREV, 29).unwrap();
            }
            let bytes = m.ext.get::<Provider>().bindings[0].matched.functions[0]
                .bytes
                .clone();
            let result = m.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            assert!(identical(&m.cpu, ENTRY, &bytes));
            let stats = stats(&m.ext).unwrap();
            if bad_tables {
                assert_eq!(stats.native_calls, 0);
                assert!(stats.fallback_calls > 0);
                assert_eq!(
                    m.cpu.read_u32(OUTPUT).unwrap(),
                    if registers { 64 } else { 42.0f32.to_bits() }
                );
            } else if mode == Mode::Verify {
                assert!(stats.verify_calls > 0);
                assert_eq!(stats.native_calls, 0);
                assert_eq!(
                    m.cpu.read_u32(OUTPUT).unwrap(),
                    if registers { 64 } else { 42.0f32.to_bits() }
                );
            } else {
                assert!(stats.native_calls > 0);
                assert_eq!(f32::from_bits(m.cpu.read_u32(OUTPUT).unwrap()), 0.0);
            }
        }
    }
}
