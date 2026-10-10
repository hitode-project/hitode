//! Self-authored fixtures only. No private sample bytes enter these tests.
use super::{diagnostics::Census, eval, ir::*, lift, verify};
use crate::{
    CpuState, EXEC, Memory, READ, WRITE,
    interpreter::{Interpreter, Step},
};
use iced_x86::{Decoder, DecoderOptions, Instruction, code_asm::*};
use std::collections::BTreeMap;

const BASE: u32 = 0x10000;
const DATA: u32 = BASE + 0x2000;
const STACK: u32 = BASE + 0x5000;
const LENGTH: usize = 0x6000;

fn assemble(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(BASE)).unwrap()
}
fn decoded(bytes: &[u8]) -> BTreeMap<u32, Instruction> {
    Decoder::with_ip(32, bytes, u64::from(BASE), DecoderOptions::NONE)
        .into_iter()
        .map(|i| (i.ip32(), i))
        .collect()
}
fn graph(bytes: &[u8]) -> Region {
    let result = lift::lift(&decoded(bytes), BASE).unwrap();
    assert!(result.rejected.is_empty(), "{:?}", result.rejected);
    result.region
}
fn memory(bytes: &[u8], permission: u8) -> Memory {
    let mut m = Memory::new(BASE, LENGTH).unwrap();
    m.protect(BASE, LENGTH, READ | WRITE).unwrap();
    m.write(BASE, bytes).unwrap();
    let data: Vec<_> = (0..LENGTH - 0x2000)
        .map(|n| (n as u8).wrapping_mul(137))
        .collect();
    m.write(DATA, &data).unwrap();
    m.protect(BASE, 0x1000, READ | EXEC).unwrap();
    m.protect(DATA + 0x1000, 0x1000, permission).unwrap();
    m
}
fn initial() -> CpuState {
    CpuState {
        regs: [
            0x7fff_ffff,
            1,
            0x8000_0000,
            0xffff_ffff,
            STACK,
            STACK,
            DATA,
            DATA + 0x1000,
        ],
        eip: BASE,
        eflags: 0x202,
        ..CpuState::default()
    }
}
fn same(frame: &eval::Frame, cpu: &CpuState, prefix: u64) {
    assert_eq!(frame.gpr, cpu.regs, "GPR prefix {prefix}");
    assert_eq!(frame.flags, cpu.eflags, "full EFLAGS prefix {prefix}");
    assert_eq!(frame.pc, cpu.eip, "PC prefix {prefix}");
    assert_eq!(frame.committed, prefix, "retirement prefix");
}
fn compare(bytes: &[u8], cpu: CpuState, permission: u8, budget: u64) -> eval::Outcome {
    let r = graph(bytes);
    compare_graph(&r, bytes, cpu, permission, budget)
}
fn compare_graph(
    r: &Region,
    bytes: &[u8],
    cpu: CpuState,
    permission: u8,
    budget: u64,
) -> eval::Outcome {
    verify::verify(r).unwrap();
    let mut candidate = memory(bytes, permission);
    let mut reference = memory(bytes, permission);
    let mut oracle = Interpreter::new();
    let mut state = cpu.clone();
    let mut committed = 0;
    let outcome = eval::evaluate_observed(
        r,
        &mut candidate,
        &eval::Frame::from_cpu(&cpu),
        budget,
        |frame, m| {
            assert_eq!(oracle.step(&mut state, &mut reference), Step::Executed);
            committed += 1;
            same(frame, &state, committed);
            assert_eq!(
                m.read_data(BASE, LENGTH).unwrap(),
                reference.read_data(BASE, LENGTH).unwrap(),
                "memory prefix {committed}"
            );
        },
    )
    .unwrap();
    same(&outcome.frame, &state, committed);
    if let eval::Stop::Fault { step, snapshot } = outcome.stop {
        assert!(snapshot.0 < r.snapshots.len());
        let mut recovered = cpu.clone();
        outcome.frame.restore(&mut recovered);
        assert_eq!(
            oracle.step(&mut state, &mut reference),
            step,
            "oracle fault"
        );
        assert_eq!(
            Interpreter::new().step(&mut recovered, &mut candidate),
            step,
            "resumed snapshot fault"
        );
        same(&outcome.frame, &recovered, committed);
    }
    assert_eq!(
        candidate.read_data(BASE, LENGTH).unwrap(),
        reference.read_data(BASE, LENGTH).unwrap(),
        "memory at stop"
    );
    outcome
}

#[test]
fn integer_families_partial_registers_and_flags() {
    let bytes = assemble(|a| {
        a.mov(al, 0xffu32).unwrap();
        a.mov(ah, 0x80u32).unwrap();
        a.mov(ax, 0x7fffu32).unwrap();
        a.movzx(ebx, ah).unwrap();
        a.movsx(edx, al).unwrap();
        a.movsx(cx, ah).unwrap();
        a.mov(byte_ptr(esi), ah).unwrap();
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.movsx(ebx, word_ptr(esi)).unwrap();
        a.lea(eax, ptr(esi + ecx * 4 - 16)).unwrap();
        a.xchg(al, ah).unwrap();
        a.xchg(ax, bx).unwrap();
        a.xchg(eax, edx).unwrap();
        a.add(al, 1).unwrap();
        a.adc(ah, bl).unwrap();
        a.adc(ax, bx).unwrap();
        a.adc(eax, edx).unwrap();
        a.sub(ax, 1).unwrap();
        a.sbb(al, bl).unwrap();
        a.sbb(ax, bx).unwrap();
        a.sbb(eax, edx).unwrap();
        a.cmp(al, ah).unwrap();
        a.cmp(ax, bx).unwrap();
        a.cmp(eax, edx).unwrap();
        a.inc(al).unwrap();
        a.dec(ah).unwrap();
        a.inc(ax).unwrap();
        a.dec(eax).unwrap();
        a.neg(al).unwrap();
        a.neg(ax).unwrap();
        a.neg(eax).unwrap();
        a.and(al, bl).unwrap();
        a.or(ax, bx).unwrap();
        a.xor(eax, edx).unwrap();
        a.not(ah).unwrap();
        a.not(ax).unwrap();
        a.not(eax).unwrap();
        a.test(al, bl).unwrap();
        a.test(ax, bx).unwrap();
        a.test(eax, edx).unwrap();
        a.add(dword_ptr(esi), eax).unwrap();
        a.adc(byte_ptr(esi + 1), ah).unwrap();
        a.sbb(word_ptr(esi + 2), bx).unwrap();
        a.neg(dword_ptr(esi + 4)).unwrap();
        a.inc(byte_ptr(esi)).unwrap();
        a.dec(word_ptr(esi + 2)).unwrap();
        a.not(dword_ptr(esi)).unwrap();
        a.and(byte_ptr(esi), al).unwrap();
        a.or(word_ptr(esi), bx).unwrap();
        a.xor(dword_ptr(esi), edx).unwrap();
        a.imul(edx).unwrap();
        a.mul(ebx).unwrap();
        a.imul_2(eax, ebx).unwrap();
        a.imul_3(edx, dword_ptr(esi), -17).unwrap();
        a.imul_2(ax, bx).unwrap();
        a.imul_3(dx, word_ptr(esi), -128).unwrap();
        a.cbw().unwrap();
        a.cwde().unwrap();
        a.cwd().unwrap();
        a.cdq().unwrap();
    });
    // RF is excluded: the executor consumes RF in the interpreter before entering
    // native code, so a lifted region never starts with RF set.
    for flags in [2, 0x202, 0xad7, 0xfffe_feff] {
        let mut cpu = initial();
        cpu.eflags = flags;
        compare(&bytes, cpu, READ | WRITE, 1000);
    }
}

#[test]
fn arithmetic_boundary_matrix_and_carry_chains() {
    let forms: &[&[u8]] = &[
        &[0x00, 0xd8],
        &[0x10, 0xd8],
        &[0x28, 0xd8],
        &[0x18, 0xd8],
        &[0x66, 0x01, 0xd8],
        &[0x66, 0x11, 0xd8],
        &[0x66, 0x29, 0xd8],
        &[0x66, 0x19, 0xd8],
        &[0x01, 0xd8],
        &[0x11, 0xd8],
        &[0x29, 0xd8],
        &[0x19, 0xd8],
        &[0xfe, 0xc0],
        &[0xfe, 0xc8],
        &[0xf6, 0xd8],
        &[0x84, 0xd8],
    ];
    let boundaries = [
        0,
        1,
        0xf,
        0x10,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
    ];
    for form in forms {
        for a in boundaries {
            for b in boundaries {
                for flags in [0x202, 0xad7] {
                    let mut cpu = initial();
                    cpu.regs[0] = a;
                    cpu.regs[3] = b;
                    cpu.eflags = flags;
                    compare(form, cpu, READ | WRITE, 2);
                }
            }
        }
    }
    let chain = [0x11, 0xd8, 0x11, 0xda, 0x19, 0xd8, 0x19, 0xda];
    for flags in [0x202, 0xad7] {
        let mut cpu = initial();
        cpu.eflags = flags;
        compare(&chain, cpu, READ | WRITE, 10);
    }
}

#[test]
fn shifts_and_rotates_all_counts_widths_and_zero_write_checks() {
    for group in [0u8, 1, 4, 5, 7] {
        for (byte, word) in [(true, false), (false, true), (false, false)] {
            for immediate in [false, true] {
                for count in [0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 255] {
                    for input in [0, 1, 0x80, 0x8000, 0x8000_0000, u32::MAX] {
                        let mut bytes = vec![];
                        if word {
                            bytes.push(0x66);
                        }
                        bytes.extend([
                            if immediate {
                                if byte { 0xc0 } else { 0xc1 }
                            } else if byte {
                                0xd2
                            } else {
                                0xd3
                            },
                            0xc0 | (group << 3),
                        ]);
                        if immediate {
                            bytes.push(count);
                        }
                        let mut cpu = initial();
                        cpu.regs[0] = input;
                        cpu.regs[1] = u32::from(count);
                        cpu.eflags = 0xad7;
                        compare(&bytes, cpu, READ | WRITE, 2);
                    }
                }
            }
        }
    }
    let bytes = [0xc1, 0x27, 0x00];
    let result = compare(&bytes, initial(), READ, 2);
    assert!(matches!(
        result.stop,
        eval::Stop::Fault {
            step: Step::WriteFault { .. },
            ..
        }
    ));
    let mut cpu = initial();
    cpu.regs[7] = DATA;
    let mut m = memory(&bytes, READ | WRITE);
    let epoch = m.epoch;
    eval::evaluate(&graph(&bytes), &mut m, &eval::Frame::from_cpu(&cpu), 2).unwrap();
    assert_eq!(
        m.epoch, epoch,
        "zero count must not write/touch code epochs"
    );
}

#[test]
fn conditions_memory_sources_and_multiblock_state() {
    for cc in 0..16u8 {
        let mut bytes = vec![0x0f, 0x90 + cc, 0xc4, 0x0f, 0x90 + cc, 0x06];
        bytes.extend([0x0f, 0x40 + cc, 0xc3, 0x66, 0x0f, 0x40 + cc, 0x16]);
        bytes.extend([0x70 + cc, 2, 0x40, 0x43, 0x42]);
        for flags in 0..64u32 {
            let mut cpu = initial();
            cpu.eflags = 2;
            for (n, bit) in [CF, PF, AF, ZF, SF, OF].into_iter().enumerate() {
                if flags & (1 << n) != 0 {
                    cpu.eflags |= bit;
                }
            }
            compare(&bytes, cpu, READ | WRITE, 32);
        }
        compare(&[0x0f, 0x40 + cc, 0x07], initial(), 0, 2);
    }
    let bytes = assemble(|a| {
        let mut top = a.create_label();
        let mut branch = a.create_label();
        let mut join = a.create_label();
        a.mov(ecx, 9).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(al, cl).unwrap();
        a.test(cl, 1).unwrap();
        a.je(branch).unwrap();
        a.adc(ebx, eax).unwrap();
        a.jmp(join).unwrap();
        a.set_label(&mut branch).unwrap();
        a.sbb(edx, eax).unwrap();
        a.set_label(&mut join).unwrap();
        a.mov(dword_ptr(esi), edx).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
    });
    let outcome = compare(&bytes, initial(), READ | WRITE, 1000);
    assert!(outcome.frame.committed > decoded(&bytes).len() as u64);
}

#[test]
fn stack_control_flow_success_and_fault_recovery() {
    let bytes = assemble(|a| {
        a.push(eax).unwrap();
        a.push(word_ptr(esi)).unwrap();
        a.pop(ax).unwrap();
        a.pop(ebx).unwrap();
        a.push(dword_ptr(esp)).unwrap();
        a.pop(dword_ptr(esp + 4)).unwrap();
        a.push(esp).unwrap();
        a.pop(esp).unwrap();
        a.call(u64::from(BASE + 0x1000)).unwrap();
    });
    compare(&bytes, initial(), READ | WRITE, 100);
    for bytes in [
        vec![0xff, 0xd0],
        vec![0xff, 0x16],
        vec![0xff, 0xe0],
        vec![0xff, 0x26],
        vec![0xc3],
        vec![0xc2, 8, 0],
    ] {
        compare(&bytes, initial(), READ | WRITE, 10);
    }
    let cases = [
        (vec![0x8b, 0x07], false, false),
        (vec![0x89, 0x07], true, false),
        (vec![0x01, 0x07], true, false),
        (vec![0xff, 0x07], true, false),
        (vec![0x50], true, true),
        (vec![0xe8, 0, 0, 0, 0], true, true),
        (vec![0xff, 0xd0], true, true),
        (vec![0xff, 0x16], true, true),
        (vec![0xff, 0x36], true, true),
        (vec![0xff, 0x37], false, false),
        (vec![0x58], false, true),
        (vec![0xc3], false, true),
    ];
    for (body, write, stack) in cases {
        let mut bytes = vec![0x83, 0xc3, 7];
        bytes.extend(body);
        let mut cpu = initial();
        if stack {
            cpu.regs[4] = DATA + 0x1000 + if write { 4 } else { 0 };
        }
        let result = compare(&bytes, cpu, if write { READ } else { 0 }, 20);
        assert_eq!(result.frame.committed, 1);
        assert!(matches!(result.stop, eval::Stop::Fault { .. }));
    }
    for offset in [0xffd, 0xffe, 0xfff] {
        let mut cpu = initial();
        cpu.regs[7] = DATA + offset;
        let outcome = compare(&[0x01, 0x07], cpu, READ, 4);
        assert_eq!(outcome.frame.committed, 0);
    }
    let mut cpu = initial();
    cpu.regs[4] = DATA + 0xffc;
    let outcome = compare(&[0x8f, 0x04, 0x24], cpu, READ, 4);
    assert_eq!(
        outcome.frame.committed, 0,
        "POP memory uses updated ESP but recovers old ESP"
    );
    for address in [0xffff_fffe, 0, BASE - 1] {
        let mut cpu = initial();
        cpu.regs[7] = address;
        compare(&[0x89, 0x07], cpu, READ | WRITE, 2);
    }
}

#[test]
fn every_budget_boundary_preserves_exact_memory_prefix() {
    let bytes = assemble(|a| {
        a.add(eax, 1).unwrap();
        a.mov(dword_ptr(esi), eax).unwrap();
        a.adc(edx, ebx).unwrap();
        a.push(edx).unwrap();
        a.pop(eax).unwrap();
        a.sub(word_ptr(esi), ax).unwrap();
        a.mov(eax, dword_ptr(edi)).unwrap();
    });
    for budget in 0..10 {
        compare(&bytes, initial(), 0, budget);
    }
}

#[test]
fn guard_snapshots_observe_the_committed_prefix() {
    let bytes = [0x40, 0xeb, 0, 0x43];
    for passes in [false, true] {
        let mut r = graph(&bytes);
        let Terminator::Jump(Target::Block(passed)) = r.blocks[0].terminator.clone() else {
            panic!()
        };
        let Operation::Commit { state, .. } = r.blocks[0]
            .operations
            .iter()
            .rev()
            .find(|op| matches!(op, Operation::Commit { .. }))
            .unwrap()
            .clone()
        else {
            panic!()
        };
        let condition = ValueId(r.values.len());
        r.values.push(Value {
            width: Width::B1,
            kind: ValueKind::Constant(u64::from(passes)),
        });
        r.blocks[0].operations.push(Operation::Define(condition));
        let failed = SnapshotId(r.snapshots.len());
        r.snapshots.push(Snapshot {
            state,
            commit: CommitId(2),
            recipes: vec![],
            private_slots: vec![],
            dependencies: vec![DependencyId(1)],
            lease: None,
        });
        r.recovery.push(super::recovery::Dag::build(
            &r,
            &r.snapshots[failed.0].state,
        ));
        let token = r.blocks[0].entry_token;
        r.blocks[0].terminator = Terminator::Guard {
            condition,
            passed,
            failed,
            token,
        };
        let outcome = compare_graph(&r, &bytes, initial(), READ | WRITE, 10);
        assert_eq!(outcome.frame.committed, if passes { 3 } else { 2 });
        if !passes {
            assert_eq!(outcome.stop, eval::Stop::Guard(failed));
        }
    }
}

#[test]
fn explicit_call_and_validated_return_edges() {
    let bytes = assemble(|a| {
        let mut callee = a.create_label();
        a.call(callee).unwrap();
        a.add(ebx, eax).unwrap();
        a.jmp(u64::from(BASE + 0x1000)).unwrap();
        a.set_label(&mut callee).unwrap();
        a.add(eax, 13).unwrap();
        a.ret().unwrap();
    });
    let mut r = graph(&bytes);
    let ret = r
        .blocks
        .iter()
        .position(|b| matches!(b.terminator, Terminator::Return { .. }))
        .unwrap();
    compare_graph(&r, &bytes, initial(), READ | WRITE, 100);
    if let Terminator::Return { expected, .. } = &mut r.blocks[ret].terminator {
        *expected += 1;
    }
    assert_eq!(
        compare_graph(&r, &bytes, initial(), READ | WRITE, 100)
            .frame
            .committed,
        3
    );
}

#[test]
fn rejected_forms_exit_before_instruction_and_census_is_anonymous() {
    for bytes in [
        &[0x40, 0xd9, 0xe8][..],
        &[0x40, 0xf0, 0x01, 0x06],
        &[0x40, 0x64, 0x8b, 0x06],
        &[0x40, 0x67, 0x8b, 0x00],
        &[0x40, 0x87, 0x06],
        &[0x40, 0xe0, 0x00],
    ] {
        let map = decoded(bytes);
        let lifted = lift::lift(&map, BASE).unwrap();
        assert_eq!(lifted.supported, 1);
        let result = compare_graph(&lifted.region, bytes, initial(), READ | WRITE, 10);
        assert_eq!(result.frame.committed, 1);
        assert!(matches!(
            result.stop,
            eval::Stop::Exit(ExitReason::Unsupported(_))
        ));
        let mut census = Census::default();
        census.observe(&map, BASE);
        assert_eq!(census.instructions, 2);
        assert_eq!(census.supported, 1);
        assert_eq!(census.complete_regions, 0);
        assert_eq!(census.verify_failures, 0);
        assert!(!census.line().contains(&format!("{BASE:x}")));
    }
}

fn random(state: &mut u64) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state as u32
}
fn random_sequence(seed: u64) -> (Vec<Vec<u8>>, CpuState, u8) {
    let mut rng = seed;
    let mut cpu = initial();
    for n in [0, 1, 2, 3, 5] {
        cpu.regs[n] = random(&mut rng);
    }
    cpu.eflags = 0x202 | random(&mut rng) & ARITHMETIC;
    let mut sequence = vec![];
    for _ in 0..32 {
        let k = random(&mut rng);
        let imm = random(&mut rng);
        let instruction = match k % 32 {
            0 => {
                let mut b = vec![0xb8];
                b.extend(imm.to_le_bytes());
                b
            }
            1 => vec![0x00, 0xd8],
            2 => vec![0x10, 0xdc],
            3 => vec![0x66, 0x19, 0xd8],
            4 => vec![0x11, 0xd8],
            5 => vec![0x19, 0xda],
            6 => vec![0x29, 0xd8],
            7 => vec![0x66, 0x01, 0xd0],
            8 => vec![0xf6, 0xd8],
            9 => vec![0x66, 0xf7, 0xd0],
            10 => vec![0xc1, 0xe0, imm as u8],
            11 => vec![0xd2, 0xec],
            12 => vec![0x66, 0xd3, 0xf8],
            13 => vec![0xc0, 0xc0, imm as u8],
            14 => vec![0xd3, 0xc8],
            15 => vec![0x0f, 0xaf, 0xc3],
            16 => vec![0xf7, 0xe3],
            17 => vec![0xf7, 0xea],
            18 => vec![0x0f, 0xb6, 0xc4],
            19 => vec![0x0f, 0xbe, 0xd0],
            20 => vec![0x86, 0xe0],
            21 => vec![0x89, 0x46, (imm & 63) as u8],
            22 => vec![0x0f, 0xbf, 0x46, (imm & 63) as u8],
            23 => vec![0x01, 0x5e, (imm & 63) as u8],
            24 => vec![0x80, 0x76, (imm & 63) as u8, imm as u8],
            25 => vec![0x0f, 0x90 + (imm & 15) as u8, 0xc4],
            26 => vec![0x0f, 0x40 + (imm & 15) as u8, 0x16],
            27 => vec![0x50],
            28 => vec![0x5b],
            29 => vec![0x99],
            30 => vec![0x09, 0xd8],
            _ => vec![0x84, 0xe0],
        };
        sequence.push(instruction);
    }
    let permission = match seed % 3 {
        0 => READ | WRITE,
        1 => READ,
        _ => 0,
    };
    if permission != READ | WRITE {
        sequence.push(if seed & 1 == 0 {
            vec![0x01, 0x07]
        } else {
            vec![0x8b, 0x07]
        });
    }
    (sequence, cpu, permission)
}

#[test]
fn deterministic_random_strict_differential_65536_instructions() {
    let mut committed = 0;
    let mut faults = 0;
    let mut permissions = [0usize; 3];
    for index in 0..2048u64 {
        let seed = 0x6a09_e667_f3bc_c909u64.wrapping_add(index.wrapping_mul(0x9e37_79bb));
        let (mut sequence, cpu, permission) = random_sequence(seed);
        permissions[match permission {
            0 => 0,
            READ => 1,
            _ => 2,
        }] += 1;
        let run = |sequence: &[Vec<u8>]| compare(&sequence.concat(), cpu.clone(), permission, 1000);
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&sequence))) {
            Ok(outcome) => {
                committed += outcome.frame.committed;
                faults += u64::from(matches!(outcome.stop, eval::Stop::Fault { .. }));
            }
            Err(_) => {
                // Greedy deletion produces a reproducible 1-minimal sequence.
                let mut n = 0;
                while n < sequence.len() && sequence.len() > 1 {
                    let mut smaller = sequence.clone();
                    smaller.remove(n);
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&smaller)))
                        .is_err()
                    {
                        sequence = smaller;
                        n = 0;
                    } else {
                        n += 1;
                    }
                }
                panic!(
                    "GIR seed={seed:#018x} synthetic_minimal={:02x?}",
                    sequence.concat()
                );
            }
        }
    }
    assert!(committed >= 65_536, "all generated prefixes must execute");
    assert!(permissions.iter().all(|n| *n > 0) && faults > 0);
    eprintln!(
        "gir_random sequences=2048 generated=65536 committed={committed} faults={faults} permissions={permissions:?}"
    );
}

#[test]
fn synthetic_decoder_forms_are_total_and_verified() {
    let mut rng = 0x510e_527f_ade6_82d1;
    let mut supported = 0;
    for _ in 0..20_000 {
        let bytes: Vec<_> = (0..15).map(|_| random(&mut rng) as u8).collect();
        let instruction =
            Decoder::with_ip(32, &bytes, u64::from(BASE), DecoderOptions::NONE).decode();
        let map = BTreeMap::from([(BASE, instruction)]);
        let lifted = lift::lift(&map, BASE).unwrap_or_else(|error| {
            panic!(
                "synthetic form {:?}: {error}; bytes={bytes:02x?}",
                instruction.code()
            )
        });
        if lifted.supported == 1 {
            supported += 1;
            compare_graph(
                &lifted.region,
                &bytes[..instruction.len()],
                initial(),
                READ | WRITE,
                1,
            );
        }
    }
    assert!(supported > 5000);
    eprintln!("gir_decoder generated=20000 supported={supported}");
}

#[test]
fn verifier_rejects_corrupted_graphs_without_panicking() {
    let base = graph(&[0x8b, 0x06, 0x01, 0x07, 0x40]);
    let reject = |r: Region| {
        let error = verify::verify(&r).expect_err("malformed GIR accepted");
        assert!(!error.message.is_empty());
    };
    let mut r = base.clone();
    r.entry = BlockId(99999);
    reject(r);
    let mut r = base.clone();
    r.memory[0].snapshot = SnapshotId(99999);
    reject(r);
    let mut r = base.clone();
    r.memory[1].input = r.blocks[r.entry.0].parameters[9];
    reject(r);
    let mut r = base.clone();
    r.memory[0].address = r.memory[0].result.unwrap();
    reject(r);
    let mut r = base.clone();
    r.memory[0].output = r.memory[0].input;
    reject(r);
    let mut r = base.clone();
    r.snapshots[0].state.gpr[0] = r.memory[0].result.unwrap();
    reject(r);
    let mut r = base.clone();
    r.snapshots[0].state.flags = r.blocks[r.entry.0].parameters[9];
    reject(r);
    let mut r = base.clone();
    r.commits[0].units = 2;
    reject(r);
    let mut r = base.clone();
    r.commits[0].subprogress = 1;
    reject(r);
    let mut r = base.clone();
    r.memory[1].commit = CommitId(0);
    reject(r);
    let mut r = base.clone();
    r.blocks[r.entry.0].parameters.pop();
    reject(r);
    let mut r = base.clone();
    if let Terminator::Jump(Target::Block(e)) = &mut r.blocks[r.entry.0].terminator {
        e.arguments.push(ValueId(99999));
    }
    reject(r);
    let mut r = base.clone();
    if let Terminator::Jump(Target::Block(e)) = &mut r.blocks[r.entry.0].terminator {
        e.block = BlockId(99999);
    }
    reject(r);
    let mut r = base.clone();
    if let Terminator::Jump(Target::Block(e)) = &mut r.blocks[r.entry.0].terminator {
        e.arguments.push(ValueId(99999));
    }
    reject(r);
    let mut r = base.clone();
    r.values[r.blocks[r.entry.0].parameters[0].0].width = Width::B8;
    reject(r);
    let mut r = base.clone();
    let op = r.blocks[0].operations[0].clone();
    r.blocks[0].operations.push(op);
    reject(r);
    let mut r = base.clone();
    r.snapshots[0].dependencies.push(DependencyId(99999));
    reject(r);
    let mut r = base.clone();
    let Operation::Define(id) = r.blocks[0].operations[0] else {
        panic!()
    };
    r.values[id.0].kind = ValueKind::Pure(Pure::Binary(
        Binary::Add,
        id,
        r.blocks[r.entry.0].parameters[0],
    ));
    reject(r);

    let mut r = base.clone();
    let Operation::Commit { state, next_pc, .. } = r.blocks[0]
        .operations
        .iter()
        .rev()
        .find(|op| matches!(op, Operation::Commit { .. }))
        .unwrap()
        .clone()
    else {
        panic!()
    };
    let point = CommitId(r.commits.len());
    r.commits.push(r.commits[0].clone());
    r.blocks[0].operations.push(Operation::Commit {
        point,
        state,
        next_pc,
    });
    assert!(
        verify::verify(&r)
            .unwrap_err()
            .message
            .contains("nonmonotonic")
    );
}

#[test]
#[ignore = "private capture census; requires an explicit local directory"]
fn lift_private_captures() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ADV_GIR_LIFT_CORPUS").expect("local corpus directory"),
    );
    let mut census = Census::default();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "tsv") {
            continue;
        }
        let manifest = std::fs::read_to_string(&path).unwrap();
        let mut lines = manifest.lines();
        let header: Vec<u32> = lines
            .next()
            .unwrap()
            .split('\t')
            .map(|n| n.parse().unwrap())
            .collect();
        let bytes = std::fs::read(path.with_extension("bin")).unwrap();
        let instructions = lines
            .next()
            .unwrap()
            .split('\t')
            .map(|n| {
                let pc: u32 = n.parse().unwrap();
                let i = Decoder::with_ip(
                    32,
                    &bytes[(pc - header[1]) as usize..],
                    u64::from(pc),
                    DecoderOptions::NONE,
                )
                .decode();
                (pc, i)
            })
            .collect();
        census.observe(&instructions, header[0]);
    }
    eprintln!("{}", census.line());
    assert!(census.regions > 0);
    assert_eq!(census.verify_failures, 0);
}

fn cfg_fixture(seed: u64, corrupt_return: bool) -> Vec<u8> {
    let mut rng = seed;
    assemble(|a| {
        let mut top = a.create_label();
        let mut odd = a.create_label();
        let mut join = a.create_label();
        let mut callee = a.create_label();
        let mut end = a.create_label();
        a.mov(ecx, 3 + (random(&mut rng) % 4) as i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(eax, random(&mut rng) as i32).unwrap();
        a.test(cl, 1).unwrap();
        a.jne(odd).unwrap();
        a.sbb(edx, ebx).unwrap();
        a.jmp(join).unwrap();
        a.set_label(&mut odd).unwrap();
        a.adc(ebx, eax).unwrap();
        a.set_label(&mut join).unwrap();
        a.call(callee).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.jmp(end).unwrap();
        a.set_label(&mut callee).unwrap();
        a.push(eax).unwrap();
        a.ror(ax, cl).unwrap();
        a.setb(ah).unwrap();
        a.mov(word_ptr(esi), ax).unwrap();
        a.pop(eax).unwrap();
        if corrupt_return {
            a.mov(dword_ptr(esp), BASE + 0x1000).unwrap();
        }
        a.ret().unwrap();
        a.set_label(&mut end).unwrap();
        a.pushfd().unwrap();
        a.pop(edx).unwrap();
        a.mov(eax, dword_ptr(edi)).unwrap();
    })
}
#[test]
fn recovery_dag_sharing_and_new_verifier_invariants() {
    use super::recovery::{Dag, MAX_DEPTH, MAX_NODES, Plan, Sharing, Source};
    let bytes = cfg_fixture(1, false);
    let r = graph(&bytes);
    let plan = Plan::build(&r, Sharing::Compatible);
    plan.verify(&r).unwrap();
    assert!(plan.groups < plan.sites.len());
    let mut missing = Plan::build(&r, Sharing::Compatible);
    missing.sites[0].arguments.pop();
    assert!(missing.verify(&r).is_err());
    let mut depth = r.clone();
    depth.blocks[0].inline_depth = depth.inline_limit + 1;
    assert!(verify::verify(&depth).is_err());
    let mut broken = r.clone();
    let pack = broken
        .values
        .iter()
        .position(|v| matches!(v.kind, ValueKind::Pure(Pure::FlagPack { .. })))
        .unwrap();
    if let ValueKind::Pure(Pure::FlagPack { bits, .. }) = &mut broken.values[pack].kind {
        bits[0] = ValueId(usize::MAX);
    }
    assert!(verify::verify(&broken).is_err());
    let mut dag = Dag::build(&r, &r.snapshots[0].state);
    dag.nodes.resize(MAX_NODES + 1, dag.nodes[0].clone());
    assert!(dag.verify(&r, &r.snapshots[0].state).is_err());
    let mut dag = Dag::build(&r, &r.snapshots[0].state);
    dag.nodes[0].source = Source::Pure(ValueKind::Load(MemoryId(0)));
    assert!(dag.verify(&r, &r.snapshots[0].state).is_err());
    let mut deep = r.clone();
    let start = deep.snapshots[0].state.gpr[0];
    let mut previous = start;
    for _ in 0..=MAX_DEPTH {
        let id = ValueId(deep.values.len());
        deep.values.push(Value {
            width: Width::B32,
            kind: ValueKind::Pure(Pure::Binary(Binary::Add, previous, previous)),
        });
        previous = id;
    }
    let mut dag = Dag {
        nodes: vec![super::recovery::Node {
            value: start,
            source: Source::Capture,
        }],
        roots: vec![MAX_DEPTH + 1; 9],
    };
    for n in 0..=MAX_DEPTH {
        dag.nodes.push(super::recovery::Node {
            value: ValueId(r.values.len() + n),
            source: Source::Pure(ValueKind::Pure(Pure::Binary(
                Binary::Add,
                ValueId(n),
                ValueId(n),
            ))),
        });
    }
    assert!(dag.verify(&deep, &deep.snapshots[0].state).is_err());
}
#[test]
fn bounded_call_discovery_and_recursive_boundary() {
    let bytes = cfg_fixture(2, false);
    let map = decoded(&bytes);
    for limits in [
        super::cfg::Limits {
            depth: 0,
            instructions: 2048,
        },
        super::cfg::Limits {
            depth: 4,
            instructions: map.len(),
        },
    ] {
        let l = lift::lift_with_limits(&map, BASE, limits).unwrap();
        assert_eq!(l.inlined_calls, 0);
        assert!(!l.call_boundaries.is_empty());
        compare_graph(&l.region, &bytes, initial(), READ | WRITE, 100);
    }
    let bytes = assemble(|a| {
        let mut f = a.create_label();
        a.set_label(&mut f).unwrap();
        a.call(f).unwrap();
        a.ret().unwrap();
    });
    let l = lift::lift(&decoded(&bytes), BASE).unwrap();
    assert_eq!(l.inlined_calls, 0);
    assert!(
        l.call_boundaries
            .contains_key(&super::cfg::Boundary::Recursive)
    );
    compare_graph(&l.region, &bytes, initial(), READ | WRITE, 100);
}
