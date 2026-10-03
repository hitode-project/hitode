use super::*;
use crate::interpreter::{Interpreter, Step};
use iced_x86::{Decoder, DecoderOptions, code_asm::*};
const CODE: u32 = 0x10000;
const DATA: u32 = 0x12000;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(CODE as u64).unwrap()
}

fn pcm() -> Vec<u8> {
    code(|a| {
        a.fld(dword_ptr(DATA as i32)).unwrap();
        a.fmul(dword_ptr(DATA as i32 + 4)).unwrap();
        a.fstp(qword_ptr(DATA as i32 + 8)).unwrap();
        a.fld(qword_ptr(DATA as i32 + 8)).unwrap();
        a.fistp(dword_ptr(DATA as i32 + 16)).unwrap();
    })
}

fn memory(bytes: &[u8]) -> Memory {
    let mut m = Memory::new(CODE, 0x4000).unwrap();
    m.protect(CODE, 0x4000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.write(CODE, bytes).unwrap();
    m
}

fn prepare(bytes: &[u8]) -> Vec<Prepared> {
    Decoder::with_ip(32, bytes, CODE as u64, DecoderOptions::NONE)
        .into_iter()
        .map(|i| {
            let at = i.ip32() as usize - CODE as usize;
            let op = bytes[at..at + i.len()]
                .windows(2)
                .find(|b| (0xd8..=0xdf).contains(&b[0]))
                .map(|b| ((b[0] as u16 & 7) << 8) | b[1] as u16);
            Prepared::decode(&i, op).unwrap()
        })
        .collect()
}

fn software(state: &mut CpuState, memory: &mut Memory, count: usize) {
    let mut interpreter = Interpreter::new();
    for _ in 0..count {
        assert_eq!(interpreter.step(state, memory), Step::Executed);
    }
}

#[test]
fn pcm_alias_and_all_precision_rounding_controls_match_softfloat_exactly() {
    let bytes = pcm();
    let operations = prepare(&bytes);
    for precision in [0, 2, 3] {
        for rounding in 0..4 {
            for source in [
                1.5f32 / 32768.0,
                -1.5 / 32768.0,
                -0.0,
                1.0 / 3.0,
                f32::MIN_POSITIVE,
            ] {
                let mut actual = CpuState {
                    eip: CODE,
                    ..CpuState::default()
                };
                actual.x87.control = 0x7f | (precision << 8) | (rounding << 10);
                actual.x87.status = 0x4500;
                let mut reference = actual.clone();
                let mut m = memory(&bytes);
                let mut oracle = memory(&bytes);
                for memory in [&mut m, &mut oracle] {
                    memory.write(DATA, &source.to_le_bytes()).unwrap();
                    memory.write(DATA + 4, &32768.0f32.to_le_bytes()).unwrap();
                    // The aliased temporary starts as a NaN but is overwritten
                    // before the FLD. No eager operand snapshot is permitted.
                    memory.write(DATA + 8, &[0xff; 8]).unwrap();
                }
                assert_eq!(
                    execute(
                        &operations,
                        &mut actual,
                        &mut m,
                        5,
                        CODE + bytes.len() as u32,
                        None
                    ),
                    Some(5)
                );
                software(&mut reference, &mut oracle, 5);
                assert_eq!(
                    actual, reference,
                    "PC={precision} RC={rounding} source={source}"
                );
                assert_eq!(m.read(DATA, 32).unwrap(), oracle.read(DATA, 32).unwrap());
            }
        }
    }
}

#[test]
fn raw80_arithmetic_flags_tags_and_empty_payloads_match_softfloat() {
    let bytes = code(|a| {
        a.fmul(qword_ptr(DATA as i32)).unwrap();
        a.fadd(qword_ptr(DATA as i32 + 8)).unwrap();
        a.fstp(qword_ptr(DATA as i32 + 16)).unwrap();
    });
    let operations = prepare(&bytes);
    let mut random = 0x7218_4137_2513_92abu64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    for precision in [0, 2, 3] {
        for rounding in 0..4 {
            for _ in 0..512 {
                let mut actual = CpuState {
                    eip: CODE,
                    ..CpuState::default()
                };
                actual.x87.control = 0x7f | (precision << 8) | (rounding << 10);
                // Include nonzero raw bits in empty registers and condition flags.
                actual.x87.regs =
                    std::array::from_fn(|_| X87Register::from_parts(next(), next() as u16));
                actual.x87.push(X87Register::from_parts(
                    next() | (1 << 63),
                    ((next() & 1) << 15) as u16 | (0x3ff0 + (next() % 31)) as u16,
                ));
                actual.x87.status |= next() as u16 & 0x4720;
                actual.refresh_mmx();
                let mut reference = actual.clone();
                let mut m = memory(&bytes);
                let mut oracle = memory(&bytes);
                for offset in [0, 8] {
                    let bits = next() & 0x800f_ffff_ffff_ffff | ((1007 + next() % 31) << 52);
                    m.write(DATA + offset, &bits.to_le_bytes()).unwrap();
                    oracle.write(DATA + offset, &bits.to_le_bytes()).unwrap();
                }
                assert_eq!(
                    execute(&operations, &mut actual, &mut m, 3, u32::MAX, None),
                    Some(3)
                );
                software(&mut reference, &mut oracle, 3);
                assert_eq!(actual, reference, "PC={precision} RC={rounding}");
                assert_eq!(m.read(DATA, 32).unwrap(), oracle.read(DATA, 32).unwrap());
            }
        }
    }
}

#[test]
fn every_memory_form_handles_fs_scaled_addresses_and_unaligned_page_edges() {
    let bytes = code(|a| {
        a.fld(dword_ptr(esi + ecx * 4 - 12).fs()).unwrap();
        a.fmul(dword_ptr(esi + ecx * 4 - 8).fs()).unwrap();
        a.fadd(dword_ptr(esi + ecx * 4 - 8).fs()).unwrap();
        a.fst(dword_ptr(edi).fs()).unwrap();
        a.fld(qword_ptr(esi + ecx * 4 + 4).fs()).unwrap();
        a.fmul(qword_ptr(esi + ecx * 4 + 12).fs()).unwrap();
        a.fadd(qword_ptr(esi + ecx * 4 + 20).fs()).unwrap();
        a.fst(qword_ptr(edi + 8).fs()).unwrap();
        a.fstp(qword_ptr(edi + 16).fs()).unwrap();
        a.fstp(dword_ptr(edi + 24).fs()).unwrap();
    });
    let operations = prepare(&bytes);
    for precision in [0, 2, 3] {
        for rounding in 0..4 {
            for misalignment in [0, 1, 4092] {
                let at = DATA + misalignment;
                let mut actual = CpuState {
                    eip: CODE,
                    fs_base: at - 0x1000,
                    ..CpuState::default()
                };
                actual.regs[1] = 3;
                actual.regs[6] = 0x1000;
                actual.regs[7] = 0x1040;
                actual.x87.control = 0x7f | (precision << 8) | (rounding << 10);
                let mut reference = actual.clone();
                let mut m = memory(&bytes);
                let mut oracle = memory(&bytes);
                for memory in [&mut m, &mut oracle] {
                    memory.write(at, &(1.0f32 / 3.0).to_le_bytes()).unwrap();
                    memory.write(at + 4, &1.25f32.to_le_bytes()).unwrap();
                    memory
                        .write(at + 16, &(-2.0f64 / 3.0).to_le_bytes())
                        .unwrap();
                    memory.write(at + 24, &3.5f64.to_le_bytes()).unwrap();
                    memory.write(at + 32, &0.125f64.to_le_bytes()).unwrap();
                }
                assert_eq!(
                    execute(&operations, &mut actual, &mut m, 10, u32::MAX, None),
                    Some(10)
                );
                software(&mut reference, &mut oracle, 10);
                assert_eq!(
                    actual, reference,
                    "PC={precision} RC={rounding} align={misalignment}"
                );
                assert_eq!(m.read(at, 96).unwrap(), oracle.read(at, 96).unwrap());
            }
        }
    }
}

#[test]
fn unsafe_inputs_or_late_faults_leave_the_entire_batch_untouched() {
    let bytes = pcm();
    let operations = prepare(&bytes);
    for source in [1u32, 0x7f80_0000, 0x7fc0_1234, 0x7f7f_ffff] {
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        let mut m = memory(&bytes);
        m.write(DATA, &source.to_le_bytes()).unwrap();
        m.write(DATA + 4, &f32::MAX.to_le_bytes()).unwrap();
        m.write(DATA + 8, &[0xa5; 24]).unwrap();
        let before = state.clone();
        let data = m.read(DATA, 32).unwrap().to_vec();
        assert_eq!(
            execute(&operations, &mut state, &mut m, 5, u32::MAX, None),
            None
        );
        assert_eq!(state, before);
        assert_eq!(m.read(DATA, 32).unwrap(), data);
    }
    for denied in [true, false] {
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        let mut m = memory(&bytes);
        m.write(DATA, &0.25f32.to_le_bytes()).unwrap();
        m.write(DATA + 4, &32768.0f32.to_le_bytes()).unwrap();
        m.protect(DATA, 4096, if denied { READ } else { READ | WRITE | EXEC })
            .unwrap();
        let before = state.clone();
        let data = m.read(DATA, 32).unwrap().to_vec();
        let epoch = m.epoch;
        assert_eq!(
            execute(&operations, &mut state, &mut m, 5, u32::MAX, None),
            None
        );
        assert_eq!(state, before);
        assert_eq!(m.read(DATA, 32).unwrap(), data);
        assert_eq!(m.epoch, epoch);
    }
}

#[test]
fn unmasked_pending_reserved_precision_and_invalid_stack_use_the_portable_lane() {
    let bytes = pcm();
    let operations = prepare(&bytes);
    for case in 0..5 {
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        match case {
            0 => state.x87.control &= !1,
            1 => state.x87.status |= 0x8080,
            2 => state.x87.control = 0x17f,
            3 => {
                for _ in 0..8 {
                    state.x87.push(X87Register::from_parts(1 << 63, 0x3fff));
                }
            }
            _ => state.mmx_mode = 1,
        }
        let mut m = memory(&bytes);
        let before = state.clone();
        assert_eq!(
            execute(&operations, &mut state, &mut m, 5, u32::MAX, None),
            None
        );
        assert_eq!(state, before);
    }
}

#[cfg(windows)]
#[test]
fn shared_device_pages_never_enter_the_native_raw_pointer_lane() {
    let bytes = pcm();
    let operations = prepare(&bytes);
    let mut memory = Memory::reserve(CODE, 0x4000).unwrap();
    memory.map(CODE, 4096, READ | EXEC).unwrap();
    memory.write(CODE, &bytes).unwrap();
    let alias = memory.map_shared_audio(DATA, 8192).unwrap();
    memory.write(DATA, &0.25f32.to_le_bytes()).unwrap();
    memory.write(DATA + 4, &32768.0f32.to_le_bytes()).unwrap();
    let mut state = CpuState { eip: CODE, ..CpuState::default() };
    let before = state.clone();
    assert_eq!(execute(&operations, &mut state, &mut memory, 5, u32::MAX, None), None);
    assert_eq!(state, before);
    assert_eq!(alias.load_u8(8), 0);
}

#[test]
fn budget_stop_and_return_pad_limit_the_prefix_before_any_later_store() {
    let bytes = pcm();
    let operations = prepare(&bytes);
    for (budget, stop, pad) in [
        (3, u32::MAX, None),
        (5, operations[3].pc, None),
        (5, u32::MAX, Some(operations[3].pc)),
    ] {
        let mut actual = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        let mut reference = actual.clone();
        let mut m = memory(&bytes);
        let mut oracle = memory(&bytes);
        for memory in [&mut m, &mut oracle] {
            memory.write(DATA, &0.25f32.to_le_bytes()).unwrap();
            memory.write(DATA + 4, &32768.0f32.to_le_bytes()).unwrap();
        }
        assert_eq!(
            execute(&operations, &mut actual, &mut m, budget, stop, pad),
            Some(3)
        );
        software(&mut reference, &mut oracle, 3);
        assert_eq!(actual, reference);
        assert_eq!(m.read(DATA, 32).unwrap(), oracle.read(DATA, 32).unwrap());
    }
}

#[test]
#[ignore = "diagnostic native versus SoftFloat throughput, not a CI timing gate"]
fn report_native_pcm_batch_throughput() {
    use std::time::Instant;
    let bytes = pcm();
    let operations = prepare(&bytes);
    let decoded: Vec<_> = Decoder::with_ip(32, &bytes, CODE as u64, DecoderOptions::NONE)
        .into_iter()
        .collect();
    for strategy in ["public_step", "predecoded_softfloat", "native"] {
        let mut m = memory(&bytes);
        m.write(DATA, &0.25f32.to_le_bytes()).unwrap();
        m.write(DATA + 4, &32768.0f32.to_le_bytes()).unwrap();
        let mut state = CpuState::default();
        let mut interpreter = Interpreter::new();
        let start = Instant::now();
        for _ in 0..100_000 {
            state.eip = CODE;
            if strategy == "native" {
                assert_eq!(
                    execute(&operations, &mut state, &mut m, 5, u32::MAX, None),
                    Some(5)
                );
            } else if strategy == "public_step" {
                for _ in 0..5 {
                    assert_eq!(interpreter.step(&mut state, &mut m), Step::Executed);
                }
            } else {
                for (i, prepared) in decoded.iter().zip(&operations) {
                    assert_eq!(
                        super::super::execute(
                            i,
                            super::super::Operation::X87,
                            Some(prepared.opcode),
                            &mut state,
                            &mut m,
                            false
                        ),
                        Step::Executed
                    );
                }
                state.refresh_mmx();
            }
        }
        println!(
            "strategy={strategy}, ns_per_batch={:.2}",
            start.elapsed().as_secs_f64() * 1e9 / 100_000.0
        );
        assert_eq!(m.read(DATA + 16, 4).unwrap(), 8192i32.to_le_bytes());
    }
}
