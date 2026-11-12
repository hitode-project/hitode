use super::super::{eval, verify};
use super::*;
use adv_cpu_core::{
    Memory,
    interpreter::{Interpreter, Step},
};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const FRAME: u32 = 0x3f000;

fn memory(code: &[u8]) -> Memory {
    let mut memory = Memory::new(CODE, 0x30000).unwrap();
    memory.protect(CODE, 0x30000, 3).unwrap();
    memory.write(CODE, code).unwrap();
    memory.protect(CODE, 4096, 5).unwrap();
    memory
}

fn initial() -> CpuState {
    let mut at = CpuState {
        regs: [0x87654321, 3, 0, 7, FRAME, DATA + 128, DATA, DATA + 256],
        eip: CODE,
        eflags: 0x202,
        mxcsr: 0x1fa0,
        ..CpuState::default()
    };
    at.x87.status = 0x20;
    at
}

fn assemble(body: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    body(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn built(code: &[u8], at: &CpuState) -> (Unit, Proof, Built) {
    let mut memory = memory(code);
    let mut unit = Unit::default();
    unit.build(
        &mut memory,
        CODE,
        &[CODE + code.len() as u32],
        &|_, _| None,
        &|_| None,
        true,
    );
    let proof = Proof::build(&unit.trace).expect("synthetic range proof");
    let built = build(&unit, &proof, at).unwrap();
    assert_eq!(verify::verify(&built.func), Ok(()));
    (unit, proof, built)
}

fn params(func: &Func, at: &CpuState, memory: &mut Memory) -> Vec<u128> {
    func.entry
        .iter()
        .map(|s| match *s {
            Source::Reg(i) => u128::from(at.regs[i as usize]),
            Source::Eflags => u128::from(at.eflags),
            Source::Xmm(i) => u128::from_le_bytes(at.xmm[i as usize]),
            Source::X87(i) => {
                let x = at.x87.regs[i as usize];
                let value = if x.significand() == 0 {
                    0.0
                } else {
                    (x.significand() as f64)
                        * 2.0f64.powi(i32::from(x.sign_exponent() & 0x7fff) - 0x3fff - 63)
                };
                u128::from(
                    if x.sign_exponent() & 0x8000 != 0 {
                        -value
                    } else {
                        value
                    }
                    .to_bits(),
                )
            }
            Source::Frame { offset, width } => {
                let mut bytes = [0; 16];
                memory
                    .read_into(
                        at.regs[ESP as usize].wrapping_add_signed(offset),
                        &mut bytes[..width as usize],
                    )
                    .unwrap();
                u128::from_le_bytes(bytes)
            }
        })
        .collect()
}

fn differential(code: &[u8], at: &CpuState, initialize: impl Fn(&mut Memory)) -> Built {
    compare(code, at, initialize, false)
}

fn compare(code: &[u8], at: &CpuState, initialize: impl Fn(&mut Memory), fail: bool) -> Built {
    let (_, _, built) = built(code, at);
    let mut optimized = memory(code);
    initialize(&mut optimized);
    let params = params(&built.func, at, &mut optimized);
    let run = eval::run(&built.func, &params, &mut optimized, 100_000);
    assert_eq!(
        matches!(run.outcome, eval::Outcome::Fail(_)),
        fail,
        "outcome {:?}",
        run.outcome
    );
    let id = match run.outcome {
        eval::Outcome::Exit(r) | eval::Outcome::Fail(r) => r,
        other => panic!("{other:?}"),
    };
    let record = &built.func.records[id.0 as usize];
    let mut reference = memory(code);
    initialize(&mut reference);
    let mut cpu = at.clone();
    let mut interpreter = Interpreter::new();
    let retired = run.charged - i64::from(record.refund);
    assert!((0..10_000).contains(&retired));
    for _ in 0..retired {
        assert_eq!(interpreter.step(&mut cpu, &mut reference), Step::Executed);
    }
    assert_eq!(cpu.eip, record.eip, "retirement");
    for (i, loc) in record.regs.iter().enumerate() {
        if let Some(loc) = loc {
            assert_eq!(
                eval::locate(loc, &run.values) as u32,
                cpu.regs[i],
                "reg {i}"
            );
        }
    }
    if let Some(loc) = &record.eflags {
        assert_eq!(eval::locate(loc, &run.values) as u32, cpu.eflags);
    }
    for (i, loc) in record.xmm.iter().enumerate() {
        if let Some(loc) = loc {
            assert_eq!(
                eval::locate(loc, &run.values).to_le_bytes(),
                cpu.xmm[i],
                "xmm {i}"
            );
        }
    }
    assert_eq!(record.x87.top, cpu.x87.top() as u8);
    assert_eq!(record.x87.control, cpu.x87.control);
    assert_eq!(record.x87.status, cpu.x87.status & !(0x3800 | 0x200));
    if let Some(loc) = &record.x87.c1 {
        assert_eq!(
            eval::locate(loc, &run.values) != 0,
            cpu.x87.status & 0x200 != 0,
            "C1"
        );
    }
    if let Some(loc) = &record.x87.ip {
        assert_eq!(eval::locate(loc, &run.values) as u32, cpu.x87.ip, "x87 IP");
    }
    if let Some(loc) = &record.x87.opcode {
        assert_eq!(
            eval::locate(loc, &run.values) as u16,
            cpu.x87.opcode,
            "x87 opcode"
        );
    }
    for i in 0..8 {
        assert_eq!(
            record.x87.valid >> i & 1 != 0,
            cpu.x87.physical_tag(i) != 3,
            "occupancy {i}"
        );
        if let Some(loc) = &record.x87.stack[i] {
            let value = f64::from_bits(eval::locate(loc, &run.values) as u64);
            assert_eq!(
                adv_cpu_core::floating::fast::extended(value),
                cpu.x87.regs[i],
                "ST physical {i}"
            );
        }
    }
    for &(offset, width, loc) in &record.frame {
        let bytes = eval::locate(&loc, &run.values).to_le_bytes();
        optimized
            .write(
                at.regs[ESP as usize].wrapping_add_signed(offset),
                &bytes[..width as usize],
            )
            .unwrap();
    }
    let mut lhs = vec![0; 0x20000];
    let mut rhs = lhs.clone();
    optimized.read_into(DATA, &mut lhs).unwrap();
    reference.read_into(DATA, &mut rhs).unwrap();
    assert_eq!(
        lhs.iter().zip(&rhs).position(|(a, b)| a != b),
        None,
        "first differing memory byte"
    );
    built
}

fn row(a: &mut CodeAssembler, body: impl FnOnce(&mut CodeAssembler)) {
    let mut head = a.create_label();
    a.set_label(&mut head).unwrap();
    body(a);
    a.add(esi, 4).unwrap();
    a.add(edi, 4).unwrap();
    a.dec(ecx).unwrap();
    a.jne(head).unwrap();
}

// spec: A64-OPT-2, A64-OPT-3, A64-OPT-6
#[test]
fn integer_loop_and_overlapping_frame_bytes() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(dword_ptr(esp - 8), 0x12345678u32).unwrap();
            a.mov(word_ptr(esp - 7), 0xabcd).unwrap();
            a.mov(byte_ptr(esp - 5), 0xee).unwrap();
            a.mov(eax, dword_ptr(esp - 8)).unwrap();
            a.add(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
    assert_eq!(built.assumed.frame, Some((-8, -4)));
    assert_eq!(built.assumed.esp_low, 0);
    assert_eq!(count(&built, |op| matches!(op, Op::Load { .. })), 1);
    assert_eq!(count(&built, |op| matches!(op, Op::Store { .. })), 1);
}

// spec: A64-OPT-2, A64-OPT-6
#[test]
fn integer_carry_shifts_and_division() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(eax, 31).unwrap();
            a.adc(eax, 7).unwrap();
            a.sub(eax, 9).unwrap();
            a.sbb(eax, 5).unwrap();
            a.and(eax, 0x7fff).unwrap();
            a.or(eax, 3).unwrap();
            a.xor(eax, 0x55).unwrap();
            a.test(eax, 3).unwrap();
            a.not(eax).unwrap();
            a.neg(eax).unwrap();
            a.inc(eax).unwrap();
            a.dec(eax).unwrap();
            a.shl(eax, 3).unwrap();
            a.shr(eax, 2).unwrap();
            a.sar(eax, 5).unwrap();
            a.imul_3(eax, eax, 9).unwrap();
            a.cdq().unwrap();
            a.mov(ebx, 11).unwrap();
            a.idiv(ebx).unwrap();
            a.movzx(edx, dl).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 255, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
}

// spec: A64-OPT-3
#[test]
fn inline_call_and_aligned_frame() {
    for residue in [0, 4] {
        let code = assemble(|a| {
            let mut callee = a.create_label();
            let mut done = a.create_label();
            a.push(ebp).unwrap();
            a.mov(ebp, esp).unwrap();
            a.and(esp, -8).unwrap();
            row(a, |a| {
                a.call(callee).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            });
            a.leave().unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut callee).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(eax, 7).unwrap();
            a.ret().unwrap();
            a.set_label(&mut done).unwrap();
            a.nop().unwrap();
        });
        let mut at = initial();
        at.regs[ESP as usize] += residue;
        differential(&code, &at, |_| {});
    }
}

// spec: A64-OPT-2, A64-OPT-5, A64-OPT-6
#[test]
fn x87_arithmetic_and_c1_on_an_exit() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fld1().unwrap();
            a.faddp(st1, st0).unwrap();
            a.fst(qword_ptr(edi)).unwrap();
            a.fmul(qword_ptr(esi)).unwrap();
            a.fchs().unwrap();
            a.fabs().unwrap();
            a.fdiv(qword_ptr(esi)).unwrap();
            a.fstp(st0).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    differential(&code, &at, |m| {
        m.write(DATA, &1.25f64.to_le_bytes()).unwrap();
    });
    let code = assemble(|a| {
        row(a, |a| {
            a.fadd(qword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), 7).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    at.x87.push(adv_cpu_core::floating::fast::extended(1.0));
    differential(&code, &at, |m| {
        m.write(DATA, &(0.75 * f64::EPSILON).to_le_bytes()).unwrap();
    });
}

// spec: A64-OPT-2, A64-OPT-3
#[test]
fn control_word_truncation_and_binary64_frame_bytes() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fnstcw(word_ptr(esp - 16)).unwrap();
            a.mov(ax, word_ptr(esp - 16)).unwrap();
            a.or(ax, 0xc00).unwrap();
            a.mov(word_ptr(esp - 14), ax).unwrap();
            a.fldcw(word_ptr(esp - 14)).unwrap();
            a.fld(qword_ptr(esi)).unwrap();
            a.fistp(dword_ptr(esp - 12)).unwrap();
            a.fldcw(word_ptr(esp - 16)).unwrap();
            a.fild(dword_ptr(esp - 12)).unwrap();
            a.fstp(qword_ptr(esp - 8)).unwrap();
            a.mov(eax, dword_ptr(esp - 8)).unwrap();
            a.mov(edx, dword_ptr(esp - 4)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.mov(dword_ptr(edi + 4), edx).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    let built = differential(&code, &at, |m| {
        m.write(DATA, &(-3.75f64).to_le_bytes()).unwrap();
    });
    assert_eq!(count(&built, |op| matches!(op, Op::Load { .. })), 1);
    assert_eq!(count(&built, |op| matches!(op, Op::Store { .. })), 2);
    assert_eq!(count(&built, |op| matches!(op, Op::Validate)), 1);
    assert!(
        built
            .func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|(_, op)| matches!(op, Op::FpUnary(FpUnary::RoundZero, _)))
    );
}

// spec: A64-OPT-2, A64-OPT-3
#[test]
fn mxcsr_mode_test_folds_to_one_path() {
    let code = assemble(|a| {
        row(a, |a| {
            let mut other = a.create_label();
            let mut done = a.create_label();
            a.stmxcsr(dword_ptr(esp - 4)).unwrap();
            a.mov(eax, dword_ptr(esp - 4)).unwrap();
            a.and(eax, 0x6000).unwrap();
            a.cmp(eax, 0).unwrap();
            a.jne(other).unwrap();
            a.mov(eax, 1).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut other).unwrap();
            a.mov(eax, 2).unwrap();
            a.set_label(&mut done).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |_| {});
    assert_eq!(
        built
            .func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .filter(|(_, op)| matches!(op, Op::Test { .. }))
            .count(),
        1,
        "only the loop condition remains"
    );
}

// spec: A64-OPT-5, A64-OPT-6
#[test]
fn sse2_operations_and_failure_recovery() {
    let code = assemble(|a| {
        row(a, |a| {
            a.movd(xmm0, eax).unwrap();
            a.movq(xmm1, qword_ptr(esi)).unwrap();
            a.movapd(xmm2, xmm1).unwrap();
            a.andpd(xmm2, xmm1).unwrap();
            a.andnpd(xmm2, xmm0).unwrap();
            a.orpd(xmm2, xmm1).unwrap();
            a.xorpd(xmm2, xmm0).unwrap();
            a.pand(xmm2, xmm1).unwrap();
            a.por(xmm2, xmm1).unwrap();
            a.pxor(xmm2, xmm0).unwrap();
            a.psrlq(xmm2, 3).unwrap();
            a.psllq(xmm2, xmm0).unwrap();
            a.psubd(xmm2, xmm1).unwrap();
            a.cmpltpd(xmm2, xmm1).unwrap();
            a.cvtsi2sd(xmm0, eax).unwrap();
            a.addsd(xmm0, xmm1).unwrap();
            a.subsd(xmm0, xmm1).unwrap();
            a.mulsd(xmm0, xmm1).unwrap();
            a.divsd(xmm0, xmm1).unwrap();
            a.ucomisd(xmm0, xmm1).unwrap();
            a.cvttsd2si(eax, xmm0).unwrap();
            a.movsd_2(qword_ptr(edi), xmm0).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    differential(&code, &at, |m| {
        m.write(DATA, &1.25f64.to_le_bytes()).unwrap();
    });
}

// spec: A64-OPT-5, A64-OPT-6
#[test]
fn overflow_and_denormal_leave_memory_at_the_region_start() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fmul(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(esi)).unwrap();
        })
    });
    for value in [f64::MAX, f64::from_bits(1)] {
        compare(
            &code,
            &initial(),
            |m| {
                m.write(DATA, &value.to_le_bytes()).unwrap();
            },
            true,
        );
    }
}

fn count(built: &Built, predicate: impl Fn(&Op) -> bool) -> usize {
    built
        .func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter(|(_, op)| predicate(op))
        .count()
}

// spec: A64-OPT-5
#[test]
fn independent_loads_hoist_and_stores_share_validation() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(ebp)).unwrap();
            a.fmul(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
            a.fld(qword_ptr(ebp)).unwrap();
            a.fmul(qword_ptr(edi + 8)).unwrap();
            a.fstp(qword_ptr(edi + 8)).unwrap();
            a.add(esi, 12).unwrap();
            a.add(edi, 12).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 3;
    let built = differential(&code, &at, |m| {
        for i in (0..64).step_by(8) {
            m.write(DATA + i, &1.25f64.to_le_bytes()).unwrap();
            m.write(DATA + 256 + i, &1.25f64.to_le_bytes()).unwrap();
        }
        m.write(DATA + 128, &2.0f64.to_le_bytes()).unwrap();
    });
    assert_eq!(count(&built, |op| matches!(op, Op::Validate)), 1);
    assert_eq!(
        built.func.blocks[0]
            .insts
            .iter()
            .filter(|(_, op)| matches!(op, Op::Load { .. }))
            .count(),
        1,
        "invariant loads execute at entry"
    );
}

// spec: A64-OPT-2, A64-OPT-3, A64-OPT-6
#[test]
fn a_frame_float_copy_needs_no_second_x87_load_check() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(esp - 8)).unwrap();
            a.fld(qword_ptr(esp - 8)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    let built = differential(&code, &at, |m| {
        m.write(DATA, &2.5f64.to_le_bytes()).unwrap();
    });
    assert_eq!(count(&built, |op| matches!(op, Op::X87Load { .. })), 1);
}

// spec: A64-OPT-5, A64-OPT-6
#[test]
fn a_load_failure_after_a_store_keeps_rounding_metadata() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fadd(qword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), 9).unwrap();
            a.fld(qword_ptr(esi + 8)).unwrap();
            a.fstp(st0).unwrap();
        })
    });
    let mut at = initial();
    at.x87.push(adv_cpu_core::floating::fast::extended(1.0));
    compare(
        &code,
        &at,
        |m| {
            m.write(DATA, &(0.75 * f64::EPSILON).to_le_bytes()).unwrap();
            m.write(DATA + 8, &f64::NAN.to_le_bytes()).unwrap();
        },
        true,
    );
}

// spec: A64-OPT-3
#[test]
fn nested_inline_frames_restore_the_callers_base_pointer() {
    let code = assemble(|a| {
        let mut callee = a.create_label();
        let mut done = a.create_label();
        a.push(ebp).unwrap();
        a.mov(ebp, esp).unwrap();
        row(a, |a| {
            a.call(callee).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        });
        a.leave().unwrap();
        a.jmp(done).unwrap();
        a.set_label(&mut callee).unwrap();
        a.push(ebp).unwrap();
        a.mov(ebp, esp).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
        a.set_label(&mut done).unwrap();
        a.nop().unwrap();
    });
    differential(&code, &initial(), |_| {});
}

fn rejected(code: &[u8], at: &CpuState) -> Reject {
    let mut memory = memory(code);
    let mut unit = Unit::default();
    unit.build(
        &mut memory,
        CODE,
        &[CODE + code.len() as u32],
        &|_, _| None,
        &|_| None,
        true,
    );
    let proof = Proof::build(&unit.trace).expect("rejection fixture is an ordinary computation");
    match build(&unit, &proof, at) {
        Err(reason) => reason,
        Ok(_) => panic!("expected rejection"),
    }
}

// spec: A64-OPT-2, A64-OPT-3, A64-OPT-6
#[test]
fn rejects_unproved_modes_escaping_frames_and_unsupported_forms() {
    let arithmetic = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fmul(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    for control in [0x037f, 0x067f] {
        let mut at = initial();
        at.x87.control = control;
        assert!(matches!(rejected(&arithmetic, &at), Reject::Mode(_)));
    }
    let m32 = assemble(|a| {
        row(a, |a| {
            a.fld(dword_ptr(esi)).unwrap();
            a.fstp(dword_ptr(edi)).unwrap();
        })
    });
    assert!(matches!(rejected(&m32, &initial()), Reject::Unsupported(_)));
    let escape = assemble(|a| {
        row(a, |a| {
            a.mov(eax, esp).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    assert!(matches!(rejected(&escape, &initial()), Reject::Escape(_)));
    let empty = assemble(|a| {
        row(a, |a| {
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    assert!(matches!(rejected(&empty, &initial()), Reject::StaticX87(_)));
    let control = assemble(|a| {
        row(a, |a| {
            a.fldcw(word_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), 0).unwrap();
        })
    });
    assert!(matches!(rejected(&control, &initial()), Reject::Mode(_)));
    let sse = assemble(|a| {
        row(a, |a| {
            a.movapd(xmm0, xmmword_ptr(esi)).unwrap();
            a.movq(qword_ptr(edi), xmm0).unwrap();
        })
    });
    assert!(matches!(rejected(&sse, &initial()), Reject::Unsupported(_)));
}

// spec: A64-OPT-2, A64-OPT-6
#[test]
fn x87_register_and_popping_arithmetic_forms() {
    for case in 0..6 {
        let code = assemble(|a| {
            row(a, |a| {
                a.fld(qword_ptr(esi)).unwrap();
                a.fld1().unwrap();
                a.fxch(st0, st1).unwrap();
                match case {
                    0 => a.fsubp(st1, st0).unwrap(),
                    1 => a.fsubrp(st1, st0).unwrap(),
                    2 => a.fdivp(st1, st0).unwrap(),
                    3 => a.fdivrp(st1, st0).unwrap(),
                    4 => a.fmulp(st1, st0).unwrap(),
                    _ => a.faddp(st1, st0).unwrap(),
                }
                a.fstp(qword_ptr(edi)).unwrap();
            })
        });
        let mut at = initial();
        at.regs[1] = 1;
        differential(&code, &at, |m| {
            m.write(DATA, &1.25f64.to_le_bytes()).unwrap();
        });
    }
    let code = assemble(|a| {
        row(a, |a| {
            a.fldz().unwrap();
            a.fld(st0).unwrap();
            a.fst(st1).unwrap();
            a.fstp(st0).unwrap();
            a.fchs().unwrap();
            a.fabs().unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    differential(&code, &initial(), |_| {});
}

// spec: A64-OPT-2, A64-OPT-3
#[test]
fn wide_integer_truncation_and_exact_fild64() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fnstcw(word_ptr(esp - 16)).unwrap();
            a.mov(word_ptr(esp - 14), 0x0e7f).unwrap();
            a.fldcw(word_ptr(esp - 14)).unwrap();
            a.fild(qword_ptr(esi)).unwrap();
            a.fistp(qword_ptr(esp - 8)).unwrap();
            a.fldcw(word_ptr(esp - 16)).unwrap();
            a.fild(qword_ptr(esp - 8)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    differential(&code, &at, |m| {
        m.write(DATA, &(-1234567890123i64).to_le_bytes()).unwrap();
    });
    compare(
        &code,
        &at,
        |m| {
            m.write(DATA, &(i64::MAX - 2).to_le_bytes()).unwrap();
        },
        true,
    );
}

// spec: A64-OPT-2, A64-OPT-6
#[test]
fn narrow_integer_forms_and_two_operand_product() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(al, byte_ptr(esi)).unwrap();
            a.add(al, 17).unwrap();
            a.sub(ah, 3).unwrap();
            a.and(ax, 0x7fff).unwrap();
            a.or(ah, 0x20).unwrap();
            a.xor(al, 0x55).unwrap();
            a.movsx(eax, ax).unwrap();
            a.movzx(edx, al).unwrap();
            a.imul_2(eax, edx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
}

// spec: A64-OPT-5, A64-OPT-6
#[test]
fn a_memory_fault_recovers_the_last_store_boundary() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(dword_ptr(edi), 13).unwrap();
            a.fld(qword_ptr(esi)).unwrap();
            a.fstp(st0).unwrap();
        })
    });
    let mut at = initial();
    at.regs[6] = 0x50000;
    compare(&code, &at, |_| {}, true);
}
