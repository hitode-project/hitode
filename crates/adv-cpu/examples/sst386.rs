//! Hardware-derived register integer tests, adapted from 16-bit real mode to
//! this runtime's 32-bit flat mode. No runtime dependency or downloaded code.
//! See tools/cpu/README.md for the explicit coverage boundary.
use adv_cpu::{
    CpuState, EXEC, Executor, Memory, READ, Region, RunOutcome, WRITE,
    interpreter::{Interpreter, Step},
};
use iced_x86::{
    CodeSize, Decoder, DecoderOptions, Encoder, FlowControl, Instruction, Mnemonic as M, OpKind,
    Register, RflagsBits,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
};

const CODE: u32 = 0x10000;
const FLAGS: u32 = 0xcd5; // CF/PF/AF/ZF/SF/DF/OF; no real-mode privilege state.
const REGS: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];

struct Case {
    bytes: Vec<u8>,
    initial: CpuState,
    expected: [u32; 8],
    masks: [u32; 8],
    flags: u32,
    flag_mask: u32,
    mnemonic: String,
}

enum Selection {
    Run(Box<Case>),
    Skip(&'static str),
}

fn number(value: &Value) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| format!("invalid u32: {value}"))
}

fn byte_array(value: &Value) -> Result<Vec<u8>, String> {
    value
        .as_array()
        .ok_or("missing byte array")?
        .iter()
        .map(|byte| u8::try_from(number(byte)?).map_err(|_| "byte out of range".into()))
        .collect()
}

fn integer(m: M) -> bool {
    matches!(
        m,
        M::Add
            | M::Adc
            | M::Sub
            | M::Sbb
            | M::Cmp
            | M::And
            | M::Or
            | M::Xor
            | M::Test
            | M::Inc
            | M::Dec
            | M::Neg
            | M::Not
            | M::Mul
            | M::Imul
            | M::Div
            | M::Idiv
            | M::Shl
            | M::Shr
            | M::Sar
            | M::Shld
            | M::Shrd
            | M::Rol
            | M::Ror
            | M::Rcl
            | M::Rcr
            | M::Mov
            | M::Movsx
            | M::Movzx
            | M::Xchg
            | M::Bt
            | M::Bts
            | M::Btr
            | M::Btc
            | M::Bsf
            | M::Bsr
            | M::Cwd
            | M::Cdq
            | M::Cbw
            | M::Cwde
            | M::Clc
            | M::Stc
            | M::Cmc
            | M::Cld
            | M::Std
            | M::Lahf
            | M::Sahf
            | M::Nop
            | M::Sete
            | M::Setne
            | M::Seta
            | M::Setae
            | M::Setb
            | M::Setbe
            | M::Setg
            | M::Setge
            | M::Setl
            | M::Setle
            | M::Seto
            | M::Setno
            | M::Sets
            | M::Setns
            | M::Setp
            | M::Setnp
    )
}

fn flag_bits(iced: u32) -> u32 {
    [
        (RflagsBits::CF, 1),
        (RflagsBits::PF, 4),
        (RflagsBits::AF, 16),
        (RflagsBits::ZF, 64),
        (RflagsBits::SF, 128),
        (RflagsBits::DF, 1024),
        (RflagsBits::OF, 2048),
    ]
    .into_iter()
    .filter(|(bit, _)| iced & bit != 0)
    .fold(0, |bits, (_, value)| bits | value)
}

fn register_value(state: &CpuState, register: Register) -> u32 {
    let index = match register.full_register32() {
        Register::EAX => 0,
        Register::ECX => 1,
        Register::EDX => 2,
        Register::EBX => 3,
        Register::ESP => 4,
        Register::EBP => 5,
        Register::ESI => 6,
        Register::EDI => 7,
        _ => unreachable!("the adapter accepts only GPR operands"),
    };
    let value = state.regs[index];
    match register {
        Register::AH | Register::CH | Register::DH | Register::BH => (value >> 8) & 255,
        _ if register.is_gpr8() => value & 255,
        _ if register.is_gpr16() => value & 65535,
        _ => value,
    }
}

fn selected(test: &Value) -> Result<Selection, String> {
    if test["revoked"]
        .as_bool()
        .ok_or("missing revocation decision")?
    {
        return Ok(Selection::Skip("revoked"));
    }
    if !test["exception"].is_null() {
        return Ok(Selection::Skip("real_mode_exception"));
    }
    let initial = &test["initial"]["regs"];
    let final_regs = &test["final"]["regs"];
    let raw = byte_array(&test["bytes"])?;
    // The exact 386EX vector is discussed upstream in issue #3. Our current
    // WoW64 raw-byte probe produces divide overflow instead (0xc0000095).
    // Count this as excluded CPU-model behavior, never as an emulator pass.
    if test["hash"] == "c6d731127bec724d40594edc28a6ad8fdf14acbc" {
        if raw != [0xf6, 0xf9, 0xf4]
            || number(&initial["eax"])? != 0xa1719c71
            || number(&initial["ecx"])? != 0x2ba20547
            || number(&final_regs["eax"])? != 0xa171f180
        {
            return Err("known 386EX quirk identity changed its contents".into());
        }
        return Ok(Selection::Skip("documented_386ex_idiv_quirk"));
    }
    let ip = number(&initial["eip"])?;
    let original = Decoder::with_ip(16, &raw, u64::from(ip), DecoderOptions::NONE).decode();
    if original.is_invalid() {
        return Ok(Selection::Skip("decoder_unsupported"));
    }
    if !integer(original.mnemonic()) || original.flow_control() != FlowControl::Next {
        return Ok(Selection::Skip("outside_register_integer_scope"));
    }
    for operand in 0..original.op_count() {
        match original.op_kind(operand) {
            OpKind::Register if original.op_register(operand).is_gpr() => {}
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32 => {}
            _ => return Ok(Selection::Skip("memory_or_non_gpr_operand")),
        }
    }
    if original.has_lock_prefix() || original.has_rep_prefix() || original.has_repne_prefix() {
        return Ok(Selection::Skip("semantic_prefix"));
    }
    let original_len = original.len();
    if raw.get(original_len) != Some(&0xf4) || raw.len() != original_len + 1 {
        return Err(
            "expected exactly one instruction followed by the hardware HLT sentinel".into(),
        );
    }
    if number(&final_regs["eip"])? != ip.wrapping_add(raw.len() as u32) & 65535 {
        return Err("unexpected hardware next IP (including HLT)".into());
    }
    if !test["final"]["ram"]
        .as_array()
        .ok_or("missing final RAM")?
        .is_empty()
    {
        return Err("register-only case unexpectedly writes hardware RAM".into());
    }
    // Check that convenience bytes actually match the hardware's initial RAM.
    let ram = test["initial"]["ram"]
        .as_array()
        .ok_or("missing initial RAM")?;
    let physical = (number(&initial["cs"])? & 65535) * 16 + ip;
    for (offset, byte) in raw.iter().enumerate() {
        let entry = ram
            .iter()
            .find(|entry| entry[0].as_u64() == Some(u64::from(physical) + offset as u64))
            .ok_or("instruction byte absent from hardware RAM")?;
        if number(&entry[1])? != u32::from(*byte) {
            return Err("instruction bytes disagree with hardware RAM".into());
        }
    }
    let mut state = CpuState {
        eip: CODE,
        eflags: (number(&initial["eflags"])? & FLAGS) | 2,
        ..CpuState::default()
    };
    let mut expected = [0; 8];
    let mut masks = [u32::MAX; 8];
    for (index, name) in REGS.iter().enumerate() {
        state.regs[index] = number(&initial[name])?;
        expected[index] = final_regs
            .get(name)
            .map(number)
            .transpose()?
            .unwrap_or(state.regs[index]);
        masks[index] = test["final"]["masks"]
            .get(name)
            .or_else(|| test["masks"].get(name))
            .map(number)
            .transpose()?
            .unwrap_or(u32::MAX);
    }
    let flags = final_regs
        .get("eflags")
        .map(number)
        .transpose()?
        .unwrap_or(state.eflags);
    let source_mask = test["final"]["masks"]
        .get("eflags")
        .or_else(|| test["masks"].get("eflags"))
        .map(number)
        .transpose()?
        .unwrap_or(u32::MAX);
    let mut flag_mask = FLAGS & source_mask & !flag_bits(original.rflags_undefined());
    // CL-count flag metadata is conservative. Tighten the architectural mask
    // from the actual effective count; count zero preserves every input flag.
    if matches!(
        original.mnemonic(),
        M::Shl | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr | M::Shld | M::Shrd
    ) {
        let operand = original.op_count() - 1;
        let count = if original.op_kind(operand) == OpKind::Register {
            register_value(&state, original.op_register(operand))
        } else {
            u32::from(original.immediate8())
        } & 31;
        let bits = original.op0_register().size() as u32 * 8;
        if count == 0 {
            flag_mask = FLAGS & source_mask;
        } else if count == 1 {
            // iced marks CL-count OF undefined without knowing runtime CL.
            flag_mask |= 0x800 & source_mask;
        } else {
            flag_mask &= !0x800;
        }
        if matches!(original.mnemonic(), M::Shl | M::Shr) && count >= bits {
            flag_mask &= !1;
        }
        if matches!(original.mnemonic(), M::Shld | M::Shrd) && count > bits {
            return Ok(Selection::Skip("undefined_double_shift_result"));
        }
    }
    if matches!(original.mnemonic(), M::Bsf | M::Bsr)
        && register_value(&state, original.op1_register()) == 0
    {
        // Architectural destination is undefined, not the complete GPR file.
        let destination = original.op0_register();
        for (index, register) in [
            Register::EAX,
            Register::ECX,
            Register::EDX,
            Register::EBX,
            Register::ESP,
            Register::EBP,
            Register::ESI,
            Register::EDI,
        ]
        .iter()
        .enumerate()
        {
            if destination.full_register32() == *register {
                masks[index] &= if destination.is_gpr16() {
                    0xffff0000
                } else {
                    0
                };
            }
        }
    }
    let bytes = encode_flat(original)?;
    Ok(Selection::Run(Box::new(Case {
        bytes,
        initial: state,
        expected,
        masks,
        flags,
        flag_mask,
        mnemonic: format!("{:?}", original.mnemonic()),
    })))
}

fn encode_flat(mut original: Instruction) -> Result<Vec<u8>, String> {
    // No explicit or implicit memory is admitted above. Its segment prefix has
    // no architectural effect, and must not introduce a flat-mode GS feature.
    original.set_segment_prefix(Register::None);
    original.set_code_size(CodeSize::Code32);
    let mut encoder = Encoder::new(32);
    encoder
        .encode(&original, u64::from(CODE))
        .map_err(|e| e.to_string())?;
    let bytes = encoder.take_buffer();
    let decoded = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE).decode();
    if decoded.code() != original.code() || decoded.op_count() != original.op_count() {
        return Err("mode adapter changed instruction semantics".into());
    }
    for operand in 0..original.op_count() {
        if decoded.op_kind(operand) != original.op_kind(operand)
            || (original.op_kind(operand) == OpKind::Register
                && decoded.op_register(operand) != original.op_register(operand))
            || (original.op_kind(operand) != OpKind::Register
                && decoded.immediate(operand) != original.immediate(operand))
        {
            return Err("mode adapter changed an operand".into());
        }
    }
    Ok(bytes)
}

fn compare(case: &Case, actual: &CpuState) -> Result<(), String> {
    for (index, name) in REGS.iter().enumerate() {
        if (actual.regs[index] ^ case.expected[index]) & case.masks[index] != 0 {
            return Err(format!(
                "{name}: got {:08x}, expected {:08x}, mask {:08x}",
                actual.regs[index], case.expected[index], case.masks[index]
            ));
        }
    }
    if (actual.eflags ^ case.flags) & case.flag_mask != 0 {
        return Err(format!(
            "flags: got {:08x}, expected {:08x}, mask {:08x}",
            actual.eflags, case.flags, case.flag_mask
        ));
    }
    if actual.eip != CODE + case.bytes.len() as u32 {
        return Err(format!("next PC: {:08x}", actual.eip));
    }
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: sst386 VECTORS.jsonl [JIT_SAMPLE_STRIDE; 0 disables]")?;
    let jit_stride: usize = args
        .next()
        .map_or(Ok(0), |s| s.parse().map_err(|_| "invalid stride"))?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut interpreter = Interpreter::new();
    let mut memory = Memory::new(CODE, 4096)?;
    memory.protect(CODE, 4096, READ | WRITE | EXEC)?;
    let mut selected_count = 0usize;
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut total = 0usize;
    let mut jit_native = 0usize;
    let mut jit_fallback = 0usize;
    let mut skipped = BTreeMap::<String, usize>::new();
    let mut mnemonics = BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();
    let mut failed_sources = BTreeMap::<String, usize>::new();
    for line in file.lines() {
        let test: Value =
            serde_json::from_str(&line.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        total += 1;
        let case = match selected(&test).map_err(|error| {
            format!(
                "{} #{} {}: {error}",
                test["source"], test["idx"], test["hash"]
            )
        })? {
            Selection::Skip(reason) => {
                *skipped.entry(reason.into()).or_default() += 1;
                continue;
            }
            Selection::Run(case) => case,
        };
        selected_count += 1;
        *mnemonics.entry(case.mnemonic.clone()).or_default() += 1;
        memory.write(CODE, &case.bytes)?;
        let before = memory.read(CODE, 4096)?.to_vec();
        let mut actual = case.initial.clone();
        let step = interpreter.step(&mut actual, &mut memory);
        let mut result = if step == Step::Executed {
            compare(&case, &actual)
        } else {
            Err(format!("interpreter: {step:?}"))
        };
        if result.is_ok() && memory.read(CODE, 4096)? != before {
            result = Err("register instruction modified memory".into());
        }
        if result.is_ok() && jit_stride != 0 && (selected_count - 1).is_multiple_of(jit_stride) {
            let mut jit = Executor::new(CODE, 4096)?;
            jit.protect(CODE, 4096, READ | WRITE | EXEC)?;
            jit.write(CODE, &case.bytes)?;
            jit.add_region(Region::new(CODE, case.bytes.len()))?;
            jit.state = case.initial.clone();
            let outcome = jit.run(CODE + case.bytes.len() as u32, 8)?;
            result = if outcome == RunOutcome::ReachedStop {
                compare(&case, &jit.state)
            } else {
                Err(format!("JIT: {outcome:?}"))
            };
            if jit.stats.interpreter_instructions == 0 {
                jit_native += 1;
            } else {
                jit_fallback += 1;
            }
            if result.is_ok() && jit.memory().read(CODE, case.bytes.len())? != case.bytes {
                result = Err("JIT register instruction modified memory".into());
            }
        }
        if let Err(error) = result {
            failed += 1;
            *failed_sources
                .entry(test["source"].as_str().ok_or("missing source")?.into())
                .or_default() += 1;
            if failures.len() < 32 {
                failures.push(
                    json!({"source":test["source"], "idx":test["idx"], "hash":test["hash"],
                    "name":test["name"], "error":error, "bytes":case.bytes}),
                );
            }
        } else {
            passed += 1;
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({"scope":"adapted-register-integer", "total":total,
        "selected":selected_count, "passed":passed, "failed":failed, "skipped":skipped, "mnemonics":mnemonics,
        "jit_sample_stride":jit_stride, "jit_native_cases":jit_native, "jit_fallback_cases":jit_fallback,
            "failed_sources":failed_sources, "first_failures":failures})).unwrap());
    if failed != 0 || passed == 0 {
        return Err("hardware vector validation failed or selected no tests".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authored(bytes: &[u8], eax: u32, after: u32, flags: u32) -> Value {
        let mut raw = bytes.to_vec();
        raw.push(0xf4);
        let ram: Vec<_> = raw
            .iter()
            .enumerate()
            .map(|(offset, byte)| json!([0x30000 + offset, byte]))
            .collect();
        json!({"idx":0,"hash":"authored-adapter-check", "name":"adapter-check", "revoked":false,
            "bytes":raw, "masks":{},
            "initial":{"regs":{"eax":eax,"ecx":1,"edx":2,"ebx":3,"esp":4,"ebp":5,"esi":6,"edi":7,
                "cs":0x3000,"eip":0,"eflags":0x202}, "ram":ram},
            "final":{"regs":{"eax":after,"eip":raw.len(),"eflags":flags},"ram":[],"masks":{}}})
    }

    #[test]
    fn encoding_preserves_8_16_and_32_bit_arithmetic_and_upper_register_bits() {
        for (bytes, before, after, flags) in [
            (&[0x04, 1][..], 0x1234ffff, 0x1234ff00, 0x257),
            (&[0x05, 1, 0][..], 0x1234ffff, 0x12340000, 0x257),
            (&[0x66, 0x05, 1, 0, 0, 0][..], 0xffffffff, 0, 0x257),
        ] {
            let Selection::Run(case) = selected(&authored(bytes, before, after, flags)).unwrap()
            else {
                panic!("filtered")
            };
            let mut memory = Memory::new(CODE, 4096).unwrap();
            memory.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
            memory.write(CODE, &case.bytes).unwrap();
            let mut state = case.initial.clone();
            assert_eq!(
                Interpreter::new().step(&mut state, &mut memory),
                Step::Executed
            );
            compare(&case, &state).unwrap();
            state.regs[3] ^= 1;
            assert!(
                compare(&case, &state).unwrap_err().contains("ebx"),
                "unchanged registers are checked"
            );
        }
    }

    #[test]
    fn memory_exception_and_revocation_are_exclusions_not_passes() {
        let memory = authored(&[0x03, 0x06, 0, 0], 0, 0, 2);
        assert!(matches!(
            selected(&memory).unwrap(),
            Selection::Skip("memory_or_non_gpr_operand")
        ));
        let mut exception = authored(&[0xf7, 0xf1], 0, 0, 2);
        exception["exception"] = json!({"number":0});
        assert!(matches!(
            selected(&exception).unwrap(),
            Selection::Skip("real_mode_exception")
        ));
        exception["revoked"] = json!(true);
        assert!(matches!(
            selected(&exception).unwrap(),
            Selection::Skip("revoked")
        ));
    }

    #[test]
    fn hardware_bytes_and_unchanged_flags_are_checked() {
        let mut vector = authored(&[0x40], 1, 2, 0x202);
        vector["initial"]["ram"][0][1] = json!(0x48);
        assert!(selected(&vector).err().unwrap().contains("disagree"));
        let Selection::Run(case) = selected(&authored(&[0x90], 1, 1, 0x202)).unwrap() else {
            panic!("filtered")
        };
        let mut state = case.initial.clone();
        state.eip += case.bytes.len() as u32;
        state.eflags ^= 1;
        assert!(compare(&case, &state).unwrap_err().contains("flags"));
    }

    #[test]
    fn shift_masks_follow_actual_count_without_comparing_undefined_overflow() {
        for (bytes, count, expected_mask) in [
            (&[0xd2, 0xe0][..], 0, FLAGS),            // shl al,cl with count zero.
            (&[0xd2, 0xe0][..], 1, FLAGS & !16),      // AF undefined; OF defined.
            (&[0xc0, 0xc0, 8][..], 1, FLAGS & !2048), // rol al,8: OF undefined.
            (&[0xc0, 0xd0, 9][..], 1, FLAGS & !2048), // rcl al,9: still OF undefined.
        ] {
            let mut vector = authored(bytes, 0x80, 1, 0x803);
            vector["initial"]["regs"]["ecx"] = json!(count);
            let Selection::Run(case) = selected(&vector).unwrap() else {
                panic!("filtered")
            };
            assert_eq!(case.flag_mask, expected_mask);
        }
    }
}
