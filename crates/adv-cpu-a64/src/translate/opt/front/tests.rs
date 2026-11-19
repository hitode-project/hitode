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
    let mut proof = Proof::build(&unit.trace).expect("synthetic range proof");
    prove(&mut proof, &unit, code);
    let built = build(&unit, &proof, at).unwrap();
    assert_eq!(verify::verify(&built.func), Ok(()));
    super::super::back::lower(&built.func);
    #[cfg(emulator)]
    crate::translate::tests::opt_back::inline::check_guest_snapshots(
        &built.func,
        &built.assumed,
        at,
    );
    (unit, proof, built)
}

/// Decodes the unit's instructions from `code` for the proof.
fn prove(proof: &mut Proof, unit: &Unit, code: &[u8]) {
    proof.prove_split(&unit.trace, |pc, len| {
        let bytes = code
            .get(usize::try_from(pc - CODE).ok()?..)?
            .get(..len as usize)?;
        Some(iced_x86::Decoder::with_ip(32, bytes, u64::from(pc), 0).decode())
    });
}

fn params(func: &Func, at: &CpuState, memory: &mut Memory) -> Vec<u128> {
    func.entry
        .iter()
        .map(|s| match *s {
            Source::Reg(i) => u128::from(at.regs[i as usize]),
            Source::FsBase => u128::from(at.fs_base),
            Source::Eflags => u128::from(at.eflags),
            Source::X87Ip => u128::from(at.x87.ip),
            Source::X87Opcode => u128::from(at.x87.opcode),
            Source::Xmm(i) => u128::from_le_bytes(at.xmm[i as usize]),
            Source::Mmx(i) => u128::from(at.mmx[i as usize]),
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
    #[cfg(emulator)]
    if !fail {
        crate::translate::tests::opt_back::inline::compare_guest_exit(&built, at, &reference, id);
    }
    let mut cpu = at.clone();
    let mut interpreter = Interpreter::new();
    let retired = run.charged - i64::from(record.refund);
    assert!((0..10_000).contains(&retired));
    for _ in 0..retired {
        assert_eq!(interpreter.step(&mut cpu, &mut reference), Step::Executed);
    }
    assert_eq!(cpu.eip, record.eip, "retirement");
    // An exit leaves the registers it does not hold at their entry values.
    let exit = matches!(run.outcome, eval::Outcome::Exit(_));
    for (i, loc) in record.regs.iter().enumerate() {
        match loc {
            Some(loc) => assert_eq!(
                eval::locate(loc, &run.values) as u32,
                cpu.regs[i],
                "reg {i}"
            ),
            None if exit => assert_eq!(at.regs[i], cpu.regs[i], "kept reg {i}"),
            None => {}
        }
    }
    if let Some(loc) = &record.eflags {
        assert_eq!(eval::locate(loc, &run.values) as u32, cpu.eflags);
    }
    for (i, loc) in record.xmm.iter().enumerate() {
        match loc {
            Some(loc) => assert_eq!(
                eval::locate(loc, &run.values).to_le_bytes(),
                cpu.xmm[i],
                "xmm {i}"
            ),
            None if exit => assert_eq!(at.xmm[i], cpu.xmm[i], "kept xmm {i}"),
            None => {}
        }
    }
    let meta = record
        .mmx_meta
        .as_ref()
        .map_or(0, |loc| eval::locate(loc, &run.values) as u32);
    for (i, loc) in record.mmx.iter().enumerate() {
        match loc {
            Some(loc) => {
                let value = eval::locate(loc, &run.values) as u64;
                assert_eq!(value, cpu.mmx[i], "mmx {i}");
                if meta >> i & 1 != 0 {
                    assert_eq!(
                        adv_cpu_core::X87Register::from_parts(value, 0xffff),
                        cpu.x87.regs[i],
                        "mmx alias {i}"
                    );
                } else if exit {
                    assert_eq!(at.x87.regs[i], cpu.x87.regs[i], "unwritten mmx alias {i}");
                }
            }
            None if exit && record.x87.is_none() => {
                assert_eq!(at.mmx[i], cpu.mmx[i], "kept mmx {i}");
                assert_eq!(at.x87.regs[i], cpu.x87.regs[i], "kept mmx alias {i}");
            }
            None => {}
        }
    }
    match &record.x87 {
        Some(x87) => {
            assert_eq!(x87.top, cpu.x87.top() as u8);
            assert_eq!(x87.control, cpu.x87.control);
            assert_eq!(x87.status, cpu.x87.status & !(0x3800 | 0x200));
            if let Some(loc) = &x87.c1 {
                assert_eq!(
                    eval::locate(loc, &run.values) != 0,
                    cpu.x87.status & 0x200 != 0,
                    "C1"
                );
            }
            if let Some(loc) = &x87.ip {
                assert_eq!(eval::locate(loc, &run.values) as u32, cpu.x87.ip, "x87 IP");
            }
            if let Some(loc) = &x87.opcode {
                assert_eq!(
                    eval::locate(loc, &run.values) as u16,
                    cpu.x87.opcode,
                    "x87 opcode"
                );
            }
            for i in 0..8 {
                assert_eq!(
                    x87.valid >> i & 1 != 0,
                    cpu.x87.physical_tag(i) != 3,
                    "occupancy {i}"
                );
                if let Some(loc) = &x87.stack[i] {
                    let value = f64::from_bits(eval::locate(loc, &run.values) as u64);
                    assert_eq!(
                        adv_cpu_core::floating::fast::extended(value),
                        cpu.x87.regs[i],
                        "ST physical {i}"
                    );
                }
            }
        }
        None if record.mmx_meta.is_some() => {
            let expected_top = if meta >> 10 & 1 != 0 { 0 } else { at.x87.top() };
            let expected_tag = match meta >> 8 & 3 {
                1 => 0,
                2 => 0xffff,
                _ => at.x87.tag,
            };
            assert_eq!(expected_top, cpu.x87.top(), "mmx TOP");
            assert_eq!(expected_tag, cpu.x87.tag, "mmx tag");
            assert_eq!(at.x87.control, cpu.x87.control, "mmx x87 control");
            assert_eq!(
                at.x87.status & !0x3800,
                cpu.x87.status & !0x3800,
                "mmx x87 status"
            );
        }
        None => assert_eq!(
            (at.x87.control, at.x87.status, at.x87.tag),
            (cpu.x87.control, cpu.x87.status, cpu.x87.tag),
            "x87 state kept"
        ),
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
    if fail {
        // Execution resumed from the record, whose absent registers keep
        // their entry values, runs as the reference does.
        let mut resumed = cpu.clone();
        for i in 0..8 {
            resumed.regs[i] =
                record.regs[i].map_or(at.regs[i], |loc| eval::locate(&loc, &run.values) as u32);
            resumed.xmm[i] = record.xmm[i].map_or(at.xmm[i], |loc| {
                eval::locate(&loc, &run.values).to_le_bytes()
            });
            if let Some(loc) = record.mmx[i] {
                let value = eval::locate(&loc, &run.values) as u64;
                resumed.mmx[i] = value;
                if meta >> i & 1 != 0 {
                    resumed.x87.regs[i] = adv_cpu_core::X87Register::from_parts(value, 0xffff);
                }
            }
        }
        if meta >> 10 & 1 != 0 {
            resumed.x87.set_top(0);
        }
        match meta >> 8 & 3 {
            1 => resumed.x87.tag = 0,
            2 => resumed.x87.tag = 0xffff,
            _ => {}
        }
        resumed.eflags = record
            .eflags
            .map_or(at.eflags, |loc| eval::locate(&loc, &run.values) as u32);
        let end = CODE + code.len() as u32;
        let finish = |cpu: &mut CpuState, memory: &mut Memory| {
            let mut interpreter = Interpreter::new();
            for _ in 0..10_000 {
                if cpu.eip == end {
                    return None;
                }
                let step = interpreter.step(cpu, memory);
                if step != Step::Executed {
                    return Some(format!("{step:?}"));
                }
            }
            panic!("no end");
        };
        let stopped = finish(&mut resumed, &mut optimized);
        assert_eq!(stopped, finish(&mut cpu, &mut reference), "resumed stop");
        assert_eq!(resumed.eip, cpu.eip, "resumed EIP");
        assert_eq!(resumed.regs, cpu.regs, "resumed registers");
        assert_eq!(resumed.xmm, cpu.xmm, "resumed XMM");
        optimized.read_into(DATA, &mut lhs).unwrap();
        reference.read_into(DATA, &mut rhs).unwrap();
        assert!(lhs == rhs, "resumed memory");
    }
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

// spec: A64-OPT-1, A64-OPT-2, A64-OPT-3, A64-OPT-6
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
    // An integer unit that never aligns ESP assumes only MXCSR's controls.
    assert_eq!(built.assumed.esp_low, None);
    assert!(!built.assumed.x87);
    assert_eq!(built.assumed.mxcsr_mask, 0xffc0);
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

// spec: A64-OPT-1
#[test]
fn a_computing_form_assumes_precision_raised_and_takes_the_last_instruction() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fmul(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    let mut at = initial();
    at.x87.status = 0;
    let (_, _, built) = built(&code, &at);
    assert_eq!(built.assumed.status & 0x20, 0x20);
    let admit =
        |cpu: &CpuState| super::super::admit(&built.func.entry, &built.assumed, cpu, |_, _| 0);
    assert!(
        admit(&at).is_none(),
        "a clear precision flag runs the ordinary form"
    );
    at.x87.status = 0x20;
    at.x87.ip = 0x1234;
    at.x87.opcode = 0x5a;
    let params = admit(&at).expect("the last instruction is a parameter");
    let ip = built
        .func
        .entry
        .iter()
        .position(|s| *s == Source::X87Ip)
        .unwrap();
    assert_eq!(params[ip], 0x1234);
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
    // A fraction of a negative number reloads as positive zero.
    for value in [-3.75f64, -0.5] {
        let built = differential(&code, &at, |m| {
            m.write(DATA, &value.to_le_bytes()).unwrap();
        });
        assert_eq!(count(&built, |op| matches!(op, Op::Load { .. })), 1);
        assert_eq!(count(&built, |op| matches!(op, Op::Store { .. })), 2);
        assert_eq!(count(&built, |op| matches!(op, Op::Validate)), 1);
    }
}

// spec: A64-OPT-2, A64-OPT-3
#[test]
fn control_word_through_an_extended_register() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fnstcw(word_ptr(esp - 16)).unwrap();
            a.movzx(eax, word_ptr(esp - 16)).unwrap();
            a.or(eax, 0xc00).unwrap();
            a.mov(dword_ptr(esp - 12), eax).unwrap();
            a.fldcw(word_ptr(esp - 12)).unwrap();
            a.fld(qword_ptr(esi)).unwrap();
            a.fistp(qword_ptr(esp - 8)).unwrap();
            a.fldcw(word_ptr(esp - 16)).unwrap();
            a.movsx(edx, byte_ptr(esp - 8)).unwrap();
            a.mov(dword_ptr(edi), edx).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    differential(&code, &at, |m| {
        m.write(DATA, &(-3.75f64).to_le_bytes()).unwrap();
    });
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

// spec: A64-OPT-5
#[test]
fn reads_that_a_store_may_reach_keep_their_order() {
    let forms: [fn(&mut CodeAssembler); 3] = [
        // Accumulates into one dword: its read must not leave the loop.
        |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(eax, dword_ptr(edi)).unwrap();
            a.mov(dword_ptr(esi), eax).unwrap();
        },
        // Reads back the dword it just stored.
        |a| {
            a.mov(dword_ptr(esi), ecx).unwrap();
            a.movzx(eax, byte_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        },
        // Strides that meet on the second iteration.
        |a| {
            a.mov(dword_ptr(ebx + ecx * 4), ecx).unwrap();
            a.movzx(eax, word_ptr(ebx + ecx * 2 + 4)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        },
    ];
    for form in forms {
        let code = assemble(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            form(a);
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(top).unwrap();
        });
        let mut at = initial();
        at.regs = [0, 5, 0, DATA + 0x200, FRAME, FRAME, DATA, DATA + 0x100];
        differential(&code, &at, |m| {
            for i in 0..8u32 {
                m.write(DATA + 0x100 + 4 * i, &(i + 1).to_le_bytes())
                    .unwrap();
            }
        });
    }
}

// spec: A64-OPT-2
#[test]
fn a_constant_shift_by_zero_keeps_the_flags() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.xor(ecx, ecx).unwrap();
        a.mov(eax, 2).unwrap();
        a.cmp(eax, 5).unwrap();
        a.shl(eax, cl).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.lea(edi, dword_ptr(edi + 4)).unwrap();
        a.dec(edx).unwrap();
        a.jne(top).unwrap();
    });
    let mut at = initial();
    at.regs[2] = 3;
    differential(&code, &at, |_| {});
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
    assert!(
        built
            .func
            .blocks
            .iter()
            .flat_map(|b| &b.params)
            .all(|&p| built.func.ty(p) != Type::I64),
        "the frame cell is held as the F64 stored, not also as its bits"
    );
}

#[test]
fn a_division_keeping_both_results_divides_once() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(ebx, 11).unwrap();
            a.cdq().unwrap();
            a.idiv(ebx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.mov(dword_ptr(edi + 4), edx).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &(-17i32).to_le_bytes()).unwrap();
    });
    assert_eq!(count(&built, |op| matches!(op, Op::Div { .. })), 1);
    assert_eq!(
        count(&built, |op| matches!(
            op,
            Op::IntUnary(IntUnary::SignWiden, _)
        )),
        1,
        "the dividend CDQ builds is EAX sign-extended"
    );
}

// spec: A64-OPT-3
#[test]
fn a_double_copied_by_dwords_reads_back_as_the_stored_value() {
    let code = assemble(|a| {
        row(a, |a| {
            a.fld(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(esp - 8)).unwrap();
            a.mov(eax, dword_ptr(esp - 8)).unwrap();
            a.mov(edx, dword_ptr(esp - 4)).unwrap();
            a.mov(dword_ptr(esp - 16), eax).unwrap();
            a.mov(dword_ptr(esp - 12), edx).unwrap();
            a.fld(qword_ptr(esp - 16)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    let built = differential(&code, &at, |m| {
        m.write(DATA, &2.5f64.to_le_bytes()).unwrap();
    });
    assert_eq!(
        count(&built, |op| matches!(
            op,
            Op::Int(IntOp::Or | IntOp::Shl, ..)
        )),
        0,
        "the dwords rejoin to the stored binary64 itself"
    );
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
    // SSE floating point needs MXCSR's default control, here truncating
    // rounding or unmasked precision.
    let convert = assemble(|a| {
        row(a, |a| {
            a.cvtsi2sd(xmm0, dword_ptr(esi)).unwrap();
            a.movsd_2(qword_ptr(edi), xmm0).unwrap();
        })
    });
    for mxcsr in [0x7fa0, 0x0f20] {
        let mut at = initial();
        at.mxcsr = mxcsr;
        assert!(matches!(rejected(&convert, &at), Reject::Mode(_)));
    }
}

// spec: A64F-FP-5, A64-OPT-6
#[test]
fn an_ordered_compare_of_a_quiet_nan_leaves_for_the_interpreter() {
    let code = |ordered: bool| {
        assemble(move |a| {
            row(a, |a| {
                let mut unordered = a.create_label();
                let mut done = a.create_label();
                a.movsd_2(xmm0, qword_ptr(esi)).unwrap();
                a.movsd_2(xmm1, qword_ptr(esi + 8)).unwrap();
                if ordered {
                    a.comisd(xmm0, xmm1).unwrap();
                } else {
                    a.ucomisd(xmm0, xmm1).unwrap();
                }
                a.jp(unordered).unwrap();
                a.mov(eax, 0).unwrap();
                a.jmp(done).unwrap();
                a.set_label(&mut unordered).unwrap();
                a.mov(eax, 1).unwrap();
                a.set_label(&mut done).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            })
        })
    };
    let mut at = initial();
    at.regs[1] = 1;
    for ordered in [false, true] {
        compare(
            &code(ordered),
            &at,
            |m| {
                m.write(DATA, &f64::NAN.to_le_bytes()).unwrap();
                m.write(DATA + 8, &1.0f64.to_le_bytes()).unwrap();
            },
            ordered,
        );
    }
}

// spec: A64-OPT-6
#[test]
fn a_failure_after_a_refunded_branch_keeps_retirement_exact() {
    let code = assemble(|a| {
        row(a, |a| {
            let mut cold = a.create_label();
            let mut done = a.create_label();
            a.mov(eax, dword_ptr(esi + 8)).unwrap();
            a.test(eax, eax).unwrap();
            a.jnz(cold).unwrap();
            a.xor(eax, eax).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut cold).unwrap();
            a.fld(qword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
            a.set_label(&mut done).unwrap();
        })
    });
    let mut at = initial();
    at.regs[1] = 1;
    compare(
        &code,
        &at,
        |m| {
            m.write(DATA, &f64::NAN.to_le_bytes()).unwrap();
            m.write(DATA + 8, &1u32.to_le_bytes()).unwrap();
        },
        true,
    );
}

// spec: A64-FLAG-3, A64-OPT-6
#[test]
fn loop_heads_take_unobserved_flags_from_entry() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |_| {});
    let func = &built.func;
    let eflags = func
        .entry
        .iter()
        .position(|s| *s == Source::Eflags)
        .unwrap();
    let base = func.blocks[0].params[eflags];
    let mut splits = 0;
    for block in &func.blocks {
        for t in block.end.targets().filter(|t| t.charge > 0) {
            let head = func.blocks[t.block.0 as usize].head.unwrap();
            let Some(Location::Recipe(Recipe::Eflags(v))) =
                func.records[head.record.0 as usize].eflags
            else {
                panic!("a checkpoint record names its arrival's flags");
            };
            assert_ne!(v, base, "a checkpoint keeps the arrival's exact flags");
            splits += 1;
        }
    }
    assert!(splits > 0);

    // ADC reads the carry the previous iteration's ADD wrote.
    let carried = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.adc(eax, 0).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    assert!(matches!(rejected(&carried, &initial()), Reject::Flags(_)));
}

// spec: A64-OPT-9
#[test]
fn resident_pointer_loop_keeps_its_fast_path_instruction_count() {
    let code = [0x8b, 0x36, 0x49, 0x75, 0xfb]; // mov esi,[esi]; dec ecx; jne CODE
    let mut unit = Unit::default();
    unit.build(
        &mut memory(&code),
        CODE,
        &[CODE + code.len() as u32],
        &|_, _| None,
        &|_| None,
        true,
    );
    let built = build_with(&unit, None, &initial(), &|_| false, &|_| false).unwrap();
    let lowered =
        super::super::back::lower_resident(&built.func, &built.assumed, &built.func.entry, false);
    assert_eq!((lowered.iteration_retired, lowered.iteration_words), (3, 9));
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

// spec: A64-OPT-6
#[test]
fn a_failure_after_a_grouped_store_resumes_with_the_registers_it_reads() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(ebx, eax).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            // Grouping moves the store past these, but a failure after it
            // runs them again from its record.
            a.mov(edx, ebx).unwrap();
            a.mov(ebx, 5).unwrap();
            a.mov(eax, edx).unwrap();
            a.cdq().unwrap();
            a.idiv(dword_ptr(esi + 4)).unwrap();
            a.mov(dword_ptr(edi + 4), eax).unwrap();
        })
    });
    compare(
        &code,
        &initial(),
        |m| m.write(DATA, &[9, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
        true,
    );
}

// spec: A64-OPT-6
#[test]
fn loop_heads_hold_only_live_registers_the_form_changes() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |_| {});
    let func = &built.func;
    let head = func.blocks[1..]
        .iter()
        .filter_map(|b| b.head)
        .map(|h| &func.records[h.record.0 as usize])
        .find(|r| r.eip == CODE)
        .expect("loop head");
    assert_eq!(head.regs[0], None, "EAX is written before it is read");
    assert_eq!(head.regs[3], None, "EBX keeps its entry value");
    assert!(head.regs[1].is_some(), "ECX counts the rows");
    assert!(head.xmm.iter().all(Option::is_none));
}

// spec: A64-OPT-2
#[test]
fn masks_of_narrow_loads_and_self_tests_fold_away() {
    let code = assemble(|a| {
        row(a, |a| {
            a.movzx(eax, byte_ptr(esi)).unwrap();
            a.and(eax, 0xff).unwrap();
            a.movzx(edx, al).unwrap();
            a.add(edx, eax).unwrap();
            a.test(edx, edx).unwrap();
            a.mov(dword_ptr(edi), edx).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[0x81, 0, 0, 0, 0xff, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
    let masks = count(&built, |op| {
        matches!(
            op,
            Op::Int(IntOp::And, ..) | Op::IntUnary(IntUnary::ZeroExtend8, _)
        )
    });
    assert_eq!(
        masks, 0,
        "a byte load clears the upper bits; TEST of one register ANDs nothing"
    );
}

// spec: A64-OPT-3
#[test]
fn a_frame_dword_accessed_whole_stays_one_value() {
    let code = assemble(|a| {
        a.mov(dword_ptr(esp - 8), 5).unwrap();
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.add(dword_ptr(esp - 8), eax).unwrap();
            a.mov(edx, dword_ptr(esp - 8)).unwrap();
            a.mov(dword_ptr(edi), edx).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
    let shifts = count(&built, |op| {
        matches!(op, Op::Int(IntOp::Shl | IntOp::Shr | IntOp::Or, ..))
    });
    assert_eq!(shifts, 0, "no byte splitting or reassembly");
}

// spec: A64-OPT-6
#[test]
fn a_refused_instruction_on_a_cold_path_leaves_there() {
    let code = assemble(|a| {
        let mut cold = a.create_label();
        let mut back = a.create_label();
        let mut done = a.create_label();
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.js(cold).unwrap();
            a.set_label(&mut back).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        });
        a.jmp(done).unwrap();
        a.set_label(&mut cold).unwrap();
        a.cvttss2si(eax, xmm0).unwrap();
        a.jmp(back).unwrap();
        a.set_label(&mut done).unwrap();
        a.nop().unwrap();
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
    });
    assert_eq!(built.cuts, 1);
    // A negative value takes the cold path, and the form leaves exactly at
    // the refused instruction.
    differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff, 3, 0, 0, 0])
            .unwrap();
    });
}

// spec: A64-FLAG-3, A64-OPT-6
#[test]
fn nested_loop_heads_take_unobserved_flags_from_entry() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(edx, 2).unwrap();
            let mut inner = a.create_label();
            a.set_label(&mut inner).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(edx).unwrap();
            a.jne(inner).unwrap();
        })
    });
    differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
            .unwrap();
    });
}

// spec: A64-OPT-2
#[test]
fn displacements_go_where_immediates_encode() {
    let code = assemble(|a| {
        row(a, |a| {
            a.mov(eax, dword_ptr(esi + 4)).unwrap();
            a.mov(dword_ptr(edi - 4), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0])
            .unwrap();
    });
    let defs: BTreeMap<Value, Op> = built
        .func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|&(r, op)| Some((r?, op)))
        .collect();
    let left = count(
        &built,
        |op| matches!(op, Op::Int(_, a, _) if matches!(defs.get(a), Some(Op::Constant(..)))),
    );
    assert_eq!(left, 0, "a constant operand is the right one");
}

// spec: A64-OPT-2
#[test]
fn loads_on_side_paths_stay_in_the_loop() {
    let code = assemble(|a| {
        let mut skip = a.create_label();
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.js(skip).unwrap();
            a.add(eax, dword_ptr(ebp)).unwrap();
            a.set_label(&mut skip).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff, 3, 0, 0, 0])
            .unwrap();
        m.write(DATA + 128, &[5, 0, 0, 0]).unwrap();
    });
    assert!(
        !built.func.blocks[0]
            .insts
            .iter()
            .any(|(_, op)| matches!(op, Op::Load { .. })),
        "a load that not every iteration runs is not hoisted"
    );
}

// spec: A64-OPT-3, A64-OPT-4
#[test]
fn a_binary64_stored_on_one_path_stays_f64_past_the_merge() {
    let code = assemble(|a| {
        let mut skip = a.create_label();
        row(a, |a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, eax).unwrap();
            a.js(skip).unwrap();
            a.fild(dword_ptr(esi)).unwrap();
            a.fstp(qword_ptr(esp - 8)).unwrap();
            a.set_label(&mut skip).unwrap();
            a.fld(qword_ptr(esp - 8)).unwrap();
            a.fstp(qword_ptr(edi)).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff, 3, 0, 0, 0])
            .unwrap();
        m.write(FRAME - 8, &1.5f64.to_le_bytes()).unwrap();
    });
    // The merge holds the cell as F64; only entry reads its bits.
    assert!(
        built.func.blocks[1..]
            .iter()
            .flat_map(|b| &b.params)
            .all(|&p| built.func.ty(p) != Type::I64)
    );
    // Not every arrival stored it, so the load after the merge checks.
    assert_eq!(count(&built, |op| matches!(op, Op::X87Load { .. })), 1);
}

// spec: A64-OPT-3
#[test]
fn a_loop_short_of_registers_commits_cells_it_does_not_read_on() {
    let code = assemble(|a| {
        row(a, |a| {
            // A division may raise, which reads every frame cell.
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.cdq().unwrap();
            a.mov(ebx, 11).unwrap();
            a.idiv(ebx).unwrap();
            for k in 0..24u32 {
                a.mov(dword_ptr(esp - 8 - 4 * k as i32), eax).unwrap();
                a.add(eax, k + 1).unwrap();
            }
            a.add(dword_ptr(esp - 4), eax).unwrap();
            a.mov(eax, dword_ptr(esp - 28)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        })
    });
    let built = differential(&code, &initial(), |m| {
        m.write(DATA, &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0])
            .unwrap();
        m.write(FRAME - 4, &[9, 0, 0, 0]).unwrap();
    });
    // Offsets count from the lowest cell, at ESP - 100.
    let commits: Vec<i32> = built
        .func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|(_, op)| match *op {
            Op::FrameStore { offset, .. } => Some(offset),
            _ => None,
        })
        .collect();
    assert_eq!(
        commits.len(),
        24,
        "each cell written and not read on commits once"
    );
    assert!(!commits.contains(&96), "the running sum stays a value");
}

/// Builds the optimized form of the unit at an entry of a PE image and
/// prints the instruction the front end refuses, if any:
/// `ADV_OPT_PROBE=<image>:<hex entry>`. Faster than a run per refusal.
#[test]
#[ignore = "needs ADV_OPT_PROBE"]
fn probe_an_image_unit() {
    let spec = std::env::var("ADV_OPT_PROBE").expect("ADV_OPT_PROBE=<image>:<hex entry>");
    let (path, entry) = spec.rsplit_once(':').unwrap();
    let entry = u32::from_str_radix(entry.trim_start_matches("0x"), 16).unwrap();
    let image = std::fs::read(path).unwrap();
    let word = |at: usize| u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(image[at..at + 2].try_into().unwrap());
    let pe = word(0x3c) as usize;
    let optional = pe + 24;
    let base = word(optional + 28);
    let size = word(optional + 56) as usize;
    let mut memory = Memory::new(base, size).unwrap();
    memory.protect(base, size, 3).unwrap();
    let mut section = optional + usize::from(half(pe + 20));
    let mut executable = Vec::new();
    for _ in 0..half(pe + 6) {
        let (virtual_size, address) = (word(section + 8), word(section + 12));
        let (raw_size, raw) = (word(section + 16) as usize, word(section + 20) as usize);
        let bytes = &image[raw..raw + raw_size.min(virtual_size as usize)];
        memory.write(base + address, bytes).unwrap();
        if word(section + 36) & 0x2000_0000 != 0 {
            executable.push((base + address, virtual_size as usize));
        }
        section += 40;
    }
    for (address, len) in executable {
        memory
            .protect(address, len.next_multiple_of(4096), 5)
            .unwrap();
    }
    let mut unit = Unit::default();
    unit.build(&mut memory, entry, &[], &|_, _| None, &|_| None, true);
    let mut proof = Proof::build(&unit.trace).expect("a computation proof");
    proof.prove_split(&unit.trace, |pc, len| {
        let mut bytes = [0; 15];
        let bytes = bytes.get_mut(..len as usize)?;
        memory.read_into(pc, bytes).ok()?;
        Some(iced_x86::Decoder::with_ip(32, bytes, u64::from(pc), 0).decode())
    });
    let mut at = initial();
    at.x87.control = 0x27f;
    at.x87.tag = 0xffff;
    at.regs[ESP as usize] = 0x0019_f000;
    println!(
        "{} trace instructions, primary {}, {} merges",
        unit.trace.len(),
        unit.trace.primary,
        unit.trace.merge.iter().filter(|&&m| m).count()
    );
    if std::env::var_os("ADV_OPT_DEBUG").is_some() {
        for (i, inst) in unit.trace.insts.iter().enumerate() {
            let next = match inst.next {
                crate::translate::trace::Next::Edge(e) => Some(e),
                _ => None,
            };
            for e in [next, inst.taken].into_iter().flatten() {
                if let crate::translate::trace::Edge::Internal(t) = e
                    && t <= i
                {
                    println!(
                        "backedge {i} ({:#x}) -> {t} ({:#x})",
                        inst.pc, unit.trace.insts[t].pc
                    );
                }
            }
        }
    }
    match build(&unit, &proof, &at) {
        Ok(built) if std::env::var_os("ADV_OPT_DEBUG").is_some() => {
            let lowered = super::super::back::lower(&built.func);
            println!(
                "lowered: {} words, {} spill cells",
                lowered.words.len(),
                lowered.spill_cells
            );
        }
        Ok(built) => println!(
            "built: {} blocks, {} heads, {} operations, {} validations, {} cuts",
            built.func.blocks.len(),
            built
                .func
                .blocks
                .iter()
                .filter(|b| b.head.is_some())
                .count(),
            built
                .func
                .blocks
                .iter()
                .map(|b| b.insts.len())
                .sum::<usize>(),
            built
                .func
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .filter(|(_, op)| matches!(op, Op::Validate))
                .count(),
            built.cuts
        ),
        Err(reject) => {
            if let Some(at) = reject.index() {
                for (i, inst) in unit.trace.insts.iter().enumerate() {
                    let into = |e: Option<crate::translate::trace::Edge>| matches!(e, Some(crate::translate::trace::Edge::Internal(t)) if t == at);
                    let fall = match inst.next {
                        crate::translate::trace::Next::Fall => {
                            Some(crate::translate::trace::Edge::Internal(i + 1))
                        }
                        crate::translate::trace::Next::Edge(e) => Some(e),
                        _ => None,
                    };
                    if i.abs_diff(at) < 6 || into(fall) || into(inst.taken) {
                        println!(
                            "{i:4} {:#x} {:?} next {:?} taken {:?} live_in {:?} observe_in {:?}",
                            inst.pc,
                            inst.op,
                            inst.next,
                            inst.taken,
                            unit.trace.live_in[i],
                            unit.trace.observe_in[i]
                        );
                    }
                }
            }
            let inst = reject.index().map(|i| unit.trace.insts[i]);
            println!(
                "{reject:?} at {:#x}: {:?}",
                inst.map_or(0, |i| i.pc),
                inst.map(|i| i.op)
            );
        }
    }
}

// spec: A64-TR-9, A64-OPT-2
#[test]
fn a_guarded_blend_with_counters_at_a_frame_pointer() {
    const TABLE: i32 = DATA as i32 + 0x8000;
    const BASE: u32 = FRAME + 0x40;
    // Rows of pixels: transparent ones are skipped, opaque ones copied and
    // the rest blended through a table. The pixel count lives in ESI and at
    // [ebp+0xc], and a guard skips rows without pixels.
    let code = assemble(|a| {
        let (mut row, mut pixel, mut skip, mut store, mut tail, mut blend) = (
            a.create_label(),
            a.create_label(),
            a.create_label(),
            a.create_label(),
            a.create_label(),
            a.create_label(),
        );
        a.set_label(&mut row).unwrap();
        a.test(ecx, ecx).unwrap();
        a.jle(tail).unwrap();
        a.mov(esi, ecx).unwrap();
        a.mov(dword_ptr(ebp + 0xc), ecx).unwrap();
        a.set_label(&mut pixel).unwrap();
        a.mov(dl, byte_ptr(eax + 3)).unwrap();
        a.test(dl, dl).unwrap();
        a.je(skip).unwrap();
        a.cmp(dl, 0xff).unwrap();
        a.jne(blend).unwrap();
        a.movzx(ecx, byte_ptr(eax)).unwrap();
        a.mov(byte_ptr(ebx), cl).unwrap();
        a.movzx(ecx, byte_ptr(eax + 2)).unwrap();
        a.jmp(store).unwrap();
        a.set_label(&mut blend).unwrap();
        a.movzx(esi, dl).unwrap();
        a.shl(esi, 8).unwrap();
        a.movzx(ecx, byte_ptr(ebx)).unwrap();
        a.movzx(ecx, byte_ptr(ecx + esi + TABLE)).unwrap();
        a.mov(byte_ptr(ebx), cl).unwrap();
        a.movzx(ecx, byte_ptr(eax + 2)).unwrap();
        a.mov(esi, dword_ptr(ebp + 0xc)).unwrap();
        a.set_label(&mut store).unwrap();
        a.mov(byte_ptr(ebx + 2), cl).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(ebx, 4).unwrap();
        a.add(eax, 4).unwrap();
        a.dec(esi).unwrap();
        a.mov(dword_ptr(ebp + 0xc), esi).unwrap();
        a.jne(pixel).unwrap();
        a.mov(edx, dword_ptr(ebp - 4)).unwrap();
        a.set_label(&mut tail).unwrap();
        a.mov(ecx, dword_ptr(ebp + 0x18)).unwrap();
        a.sub(ecx, edx).unwrap();
        a.add(ebx, ecx).unwrap();
        a.mov(ecx, dword_ptr(ebp + 0x1c)).unwrap();
        a.sub(ecx, edx).unwrap();
        a.add(eax, ecx).unwrap();
        a.dec(dword_ptr(ebp + 0x14)).unwrap();
        a.mov(ecx, dword_ptr(ebp + 0x10)).unwrap();
        a.jne(row).unwrap();
    });
    let mut at = initial();
    at.regs = [DATA, 4, 0, DATA + 0x800, FRAME, BASE, 0, 0];
    differential(&code, &at, |m| {
        for (i, alpha) in [0u8, 0xff, 0x80, 0x40].repeat(3).into_iter().enumerate() {
            let src = DATA + (i as u32 / 4) * 0x100 + (i as u32 % 4) * 4;
            m.write(src, &[i as u8 + 1, 0x20, i as u8 + 0x30, alpha])
                .unwrap();
        }
        let table: Vec<u8> = (0..0x1_0000u32).map(|i| ((i * 7) >> 3) as u8).collect();
        m.write(TABLE as u32, &table).unwrap();
        m.write(DATA + 0x800, &[0x55; 0x300]).unwrap();
        for (offset, value) in [
            (-4, 16u32),
            (0x10, 4),
            (0x14, 3),
            (0x18, 0x100),
            (0x1c, 0x100),
        ] {
            m.write(BASE.wrapping_add_signed(offset), &value.to_le_bytes())
                .unwrap();
        }
    });
}

// spec: A64-TR-9, A64-OPT-2
#[test]
fn rows_whose_loop_branches_back_to_the_pixel_loop_head() {
    const BASE: u32 = FRAME + 0x80;
    // The row loop resets the pixel count from a frame slot and branches
    // straight back to the pixel loop's head; transparent pixels are kept.
    let code = assemble(|a| {
        let (mut pixel, mut skip) = (a.create_label(), a.create_label());
        a.set_label(&mut pixel).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.test(eax, 0xff00_0000u32 as i32).unwrap();
        a.je(skip).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(pixel).unwrap();
        a.add(edi, dword_ptr(ebp - 0x5c)).unwrap();
        a.add(esi, dword_ptr(ebp - 0x54)).unwrap();
        a.mov(ecx, dword_ptr(ebp - 0x4c)).unwrap();
        a.dec(edx).unwrap();
        a.jne(pixel).unwrap();
    });
    let mut at = initial();
    at.regs = [0, 4, 3, 0, FRAME, BASE, DATA, DATA + 0x800];
    differential(&code, &at, |m| {
        for i in 0..12u32 {
            let src = DATA + (i / 4) * 0x100 + (i % 4) * 4;
            let alpha = [0u8, 0xff, 0x80, 0][i as usize % 4];
            m.write(src, &[i as u8 + 1, 0x20, 0x30, alpha]).unwrap();
        }
        m.write(DATA + 0x800, &[0x55; 0x300]).unwrap();
        for (offset, value) in [(-0x4c, 4u32), (-0x54, 0xf0), (-0x5c, 0xf0)] {
            m.write(BASE.wrapping_add_signed(offset), &value.to_le_bytes())
                .unwrap();
        }
    });
}

// spec: A64-OPT-1, A64-OPT-6, A64F-FP-4
#[test]
fn mmx_moves_emms_and_alias_exponents_recover_at_every_boundary() {
    let seeded = || {
        let mut at = initial();
        at.x87.status = 5 << 11;
        at.x87.tag = 0x5555;
        at.x87.regs = std::array::from_fn(|n| {
            adv_cpu_core::X87Register::from_parts(0x11223344_55667788 ^ n as u64, 0x3fff)
        });
        at.refresh_mmx();
        at
    };

    // Straight-line and loop with movd/movq/movq2dq/movdq2q/emms: mm2 is read-only
    // (must keep 0x3fff exponent), mm0 is written (must get 0xffff), and emms
    // sets tags to 0xffff while keeping TOP = 0.
    let loop_code = assemble(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movq(mm0, qword_ptr(esi)).unwrap();
        a.movq2dq(xmm0, mm2).unwrap();
        a.movdq2q(mm1, xmm0).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(head).unwrap();
        a.emms().unwrap();
    });
    let mut at = seeded();
    at.regs[1] = 4;
    differential(&loop_code, &at, |m| {
        for i in 0..8u32 {
            m.write(
                DATA + i * 8,
                &(0x01020304_05060708u64 + u64::from(i)).to_le_bytes(),
            )
            .unwrap();
        }
    });

    // Fault on iteration 1 before any MMX write (load fault): TOP=5, tag=0x5555,
    // and mm0's exponent must remain 0x3fff. Fault on iteration 2 after iter 1
    // wrote mm0/mm1: mm0/mm1 must have exponent 0xffff, TOP=0, tag=0, while mm2 keeps 0x3fff.
    let fault_loop = assemble(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movq(mm0, qword_ptr(esi)).unwrap();
        a.movq(mm1, mm2).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(head).unwrap();
        a.emms().unwrap();
    });
    at.regs[1] = 3;
    at.regs[6] = DATA + 0x20000;
    compare(&fault_loop, &at, |_| {}, true);

    at.regs[1] = 3;
    at.regs[6] = DATA + 0x20000 - 8;
    compare(&fault_loop, &at, |_| {}, true);

    // Conditional MMX write inside a loop: only taken iterations mark mm3 written.
    let cond_code = assemble(|a| {
        let (mut head, mut skip) = (a.create_label(), a.create_label());
        a.set_label(&mut head).unwrap();
        a.test(ecx, 1).unwrap();
        a.jnz(skip).unwrap();
        a.movd(mm3, dword_ptr(esi)).unwrap();
        a.movd(dword_ptr(edi), mm3).unwrap();
        a.set_label(&mut skip).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(head).unwrap();
    });
    for count in [1u32, 2, 3] {
        let mut at = seeded();
        at.regs[1] = count;
        differential(&cond_code, &at, |m| {
            m.write(DATA, &[0xaa; 64]).unwrap();
        });
    }
}

// spec: A64-OPT-1, A64F-FP-4
#[test]
fn mmx_rejects_and_denies_admission_when_x87_exception_pending() {
    let code = assemble(|a| {
        row(a, |a| {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();
        });
    });
    let mut pending = initial();
    pending.x87.status = 0x80;
    assert!(matches!(rejected(&code, &pending), Reject::Mode(_)));

    let clean = initial();
    let (_, _, built) = built(&code, &clean);
    assert!(built.assumed.mmx);
    let mem = memory(&code);
    assert!(
        super::super::admit(&built.func.entry, &built.assumed, &pending, |addr, w| {
            let mut b = [0u8; 8];
            mem.read_into(addr, &mut b[..w as usize]).unwrap();
            u64::from_le_bytes(b)
        })
        .is_none()
    );
    assert!(
        super::super::admit(&built.func.entry, &built.assumed, &clean, |addr, w| {
            let mut b = [0u8; 8];
            mem.read_into(addr, &mut b[..w as usize]).unwrap();
            u64::from_le_bytes(b)
        })
        .is_some()
    );
}

// spec: A64-OPT-1, A64-OPT-6
#[test]
fn mmx_and_xmm_packed_integer_ops_lower_and_match_interpreter() {
    let code = assemble(|a| {
        row(a, |a| {
            // MMX packed-integer suite (width 8 / 4).
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(eax, mm0).unwrap();
            a.movq(mm1, qword_ptr(esi)).unwrap();
            a.movq(mm2, qword_ptr(edi)).unwrap();
            a.punpcklbw(mm0, mm2).unwrap();
            a.punpckhbw(mm1, mm2).unwrap();
            a.punpcklwd(mm0, mm2).unwrap();
            a.punpckhwd(mm1, mm2).unwrap();
            a.punpckldq(mm0, mm2).unwrap();
            a.punpckhdq(mm1, mm2).unwrap();
            a.paddb(mm0, mm1).unwrap();
            a.paddw(mm0, mm2).unwrap();
            a.paddd(mm0, mm1).unwrap();
            a.paddq(mm0, mm2).unwrap();
            a.psubb(mm0, mm2).unwrap();
            a.psubw(mm0, mm1).unwrap();
            a.psubd(mm0, mm2).unwrap();
            a.psubq(mm0, mm1).unwrap();
            a.paddsb(mm0, mm2).unwrap();
            a.paddsw(mm0, mm1).unwrap();
            a.paddusb(mm0, mm2).unwrap();
            a.paddusw(mm0, mm1).unwrap();
            a.psubsb(mm0, mm2).unwrap();
            a.psubsw(mm0, mm1).unwrap();
            a.psubusb(mm0, mm2).unwrap();
            a.psubusw(mm0, mm1).unwrap();
            a.pmullw(mm0, mm2).unwrap();
            a.pmulhw(mm0, mm2).unwrap();
            a.pmulhuw(mm0, mm1).unwrap();
            a.pmaddwd(mm0, mm2).unwrap();
            a.pminsw(mm0, mm1).unwrap();
            a.pmaxsw(mm0, mm2).unwrap();
            a.pminub(mm0, mm1).unwrap();
            a.pmaxub(mm0, mm2).unwrap();
            a.pavgb(mm0, mm2).unwrap();
            a.pavgw(mm0, mm1).unwrap();
            a.psllw(mm0, 3).unwrap();
            a.pslld(mm0, 5).unwrap();
            a.psllq(mm0, 9).unwrap();
            a.psrlw(mm0, 2).unwrap();
            a.psrld(mm0, 4).unwrap();
            a.psrlq(mm0, 7).unwrap();
            a.psraw(mm0, 1).unwrap();
            a.psrad(mm0, 3).unwrap();
            a.psllw(mm0, mm3).unwrap();
            a.pslld(mm0, mm3).unwrap();
            a.psllq(mm0, mm3).unwrap();
            a.psrlw(mm0, mm3).unwrap();
            a.psrld(mm0, mm3).unwrap();
            a.psrlq(mm0, mm3).unwrap();
            a.psraw(mm0, mm3).unwrap();
            a.psrad(mm0, mm3).unwrap();
            a.pand(mm0, mm2).unwrap();
            a.por(mm0, mm1).unwrap();
            a.pxor(mm0, mm3).unwrap();
            a.pandn(mm0, mm2).unwrap();
            a.pcmpeqb(mm1, mm2).unwrap();
            a.pcmpeqw(mm0, mm1).unwrap();
            a.pcmpeqd(mm0, mm2).unwrap();
            a.pcmpgtb(mm1, mm2).unwrap();
            a.pcmpgtw(mm0, mm1).unwrap();
            a.pcmpgtd(mm0, mm2).unwrap();
            a.packsswb(mm0, mm1).unwrap();
            a.packssdw(mm0, mm2).unwrap();
            a.packuswb(mm0, mm1).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();

            // XMM packed-integer suite (128-bit lanes).
            a.movd(xmm0, eax).unwrap();
            a.movq(xmm1, qword_ptr(esi)).unwrap();
            a.movdqu(xmm2, xmmword_ptr(edi)).unwrap();
            a.punpcklbw(xmm0, xmm2).unwrap();
            a.punpckhbw(xmm1, xmm2).unwrap();
            a.punpcklwd(xmm0, xmm2).unwrap();
            a.punpckhwd(xmm1, xmm2).unwrap();
            a.punpckldq(xmm0, xmm2).unwrap();
            a.punpckhdq(xmm1, xmm2).unwrap();
            a.punpcklqdq(xmm0, xmm2).unwrap();
            a.punpckhqdq(xmm1, xmm2).unwrap();
            a.paddb(xmm0, xmm1).unwrap();
            a.paddw(xmm0, xmm2).unwrap();
            a.paddd(xmm0, xmm1).unwrap();
            a.paddq(xmm0, xmm2).unwrap();
            a.psubusb(xmm0, xmm2).unwrap();
            a.psubusw(xmm0, xmm1).unwrap();
            a.pmullw(xmm0, xmm2).unwrap();
            a.pmulhw(xmm0, xmm2).unwrap();
            a.pmulhuw(xmm0, xmm1).unwrap();
            a.pmaddwd(xmm0, xmm2).unwrap();
            a.pminsw(xmm0, xmm1).unwrap();
            a.pmaxub(xmm0, xmm2).unwrap();
            a.pavgb(xmm0, xmm2).unwrap();
            a.pavgw(xmm0, xmm1).unwrap();
            a.psllw(xmm0, 3).unwrap();
            a.psrlq(xmm0, 17).unwrap();
            a.psrad(xmm0, 5).unwrap();
            a.psllw(xmm0, xmm3).unwrap();
            a.psrlq(xmm0, xmm3).unwrap();
            a.psrad(xmm0, xmm3).unwrap();
            a.pand(xmm0, xmm2).unwrap();
            a.pandn(xmm0, xmm2).unwrap();
            a.pcmpeqb(xmm1, xmm2).unwrap();
            a.pcmpgtw(xmm0, xmm1).unwrap();
            a.packsswb(xmm0, xmm1).unwrap();
            a.packssdw(xmm0, xmm2).unwrap();
            a.packuswb(xmm0, xmm1).unwrap();
            a.movdqu(xmmword_ptr(edi + 16), xmm0).unwrap();
            a.emms().unwrap();
        });
    });
    let mut at = initial();
    at.regs[1] = 4;
    at.regs[6] = DATA + 0x100;
    at.regs[7] = DATA + 0x200;
    at.x87.regs[3] = adv_cpu_core::X87Register::from_parts(33, 0xffff);
    at.refresh_mmx();
    at.xmm[3] = 5u128.to_le_bytes();
    let built = differential(&code, &at, |mem| {
        for i in 0..64u32 {
            let v = i.wrapping_mul(0x1357_2469).wrapping_add(0x80ff_7f01);
            mem.write(DATA + 0x100 + i * 4, &v.to_le_bytes()).unwrap();
            mem.write(DATA + 0x200 + i * 4, &(!v).to_le_bytes())
                .unwrap();
        }
    });
    assert!(built.assumed.mmx);
}

// spec: A64-OPT-1, A64-OPT-3, A64-OPT-6
#[test]
fn mmx_pixel_blend_loop_builds_and_recovers_every_fault() {
    let code = assemble(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(mm1, dword_ptr(edi)).unwrap();
        a.punpcklbw(mm0, mm7).unwrap();
        a.punpcklbw(mm1, mm7).unwrap();
        a.pmullw(mm0, mm6).unwrap();
        a.psrlw(mm0, 8).unwrap();
        a.paddw(mm0, mm1).unwrap();
        a.packuswb(mm0, mm7).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.emms().unwrap();
    });
    let mut at = initial();
    at.x87.status = 5 << 11;
    at.x87.tag = 0x1234;
    at.x87.regs = std::array::from_fn(|n| {
        adv_cpu_core::X87Register::from_parts(
            if n == 7 { 0 } else { 0x0040_0040_0040_0040 },
            0x3fff,
        )
    });
    at.refresh_mmx();
    at.regs[1] = 8;
    at.regs[6] = DATA + 0x100;
    at.regs[7] = DATA + 0x200;
    at.mmx[6] = 0x0040_0040_0040_0040;
    at.mmx[7] = 0;
    let built = differential(&code, &at, |mem| {
        for i in 0..8u32 {
            mem.write(DATA + 0x100 + i * 4, &(i * 0x1122_3344).to_le_bytes())
                .unwrap();
            mem.write(
                DATA + 0x200 + i * 4,
                &(i * 0x0102_0304 + 0x1020_3040).to_le_bytes(),
            )
            .unwrap();
        }
    });
    assert!(built.assumed.mmx);
    // Fault on iteration 1 (before any MMX op) and on iteration 3 (after MMX ops).
    for unmapped_iter in [0u32, 2] {
        at.regs[1] = 4;
        at.regs[6] = DATA + 0x20000 - unmapped_iter * 4;
        at.regs[7] = DATA + 0x200;
        compare(
            &code,
            &at,
            |mem| {
                for i in 0..unmapped_iter {
                    mem.write(
                        DATA + 0x20000 - (unmapped_iter - i) * 4,
                        &0x4080_c0ffu32.to_le_bytes(),
                    )
                    .unwrap();
                    mem.write(DATA + 0x200 + i * 4, &0x1020_3040u32.to_le_bytes())
                        .unwrap();
                }
            },
            true,
        );
    }
}

// spec: A64-OPT-1, A64-OPT-6
#[test]
fn mmx_and_xmm_shuffles_extract_insert_masks_match_interpreter() {
    let code = assemble(|a| {
        row(a, |a| {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.movq(mm1, qword_ptr(edi)).unwrap();
            a.pshufw(mm2, mm0, 0x1b).unwrap();
            a.pshufw(mm3, mm1, 0xaa).unwrap();
            a.pinsrw(mm2, eax, 2).unwrap();
            a.pinsrw(mm3, word_ptr(esi + 2), 3).unwrap();
            a.pextrw(eax, mm2, 1).unwrap();
            a.pmuludq(mm2, mm3).unwrap();
            a.psadbw(mm2, mm1).unwrap();
            a.pmovmskb(ebx, mm3).unwrap();
            a.add(eax, ebx).unwrap();
            a.movq(qword_ptr(edi), mm2).unwrap();

            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.movdqu(xmm1, xmmword_ptr(edi)).unwrap();
            a.pshufd(xmm2, xmm0, 0x4e).unwrap();
            a.pshuflw(xmm2, xmm2, 0xd8).unwrap();
            a.pshufhw(xmm2, xmm2, 0x93).unwrap();
            a.shufps(xmm2, xmm1, 0xe4).unwrap();
            a.shufpd(xmm2, xmm0, 2).unwrap();
            a.pinsrw(xmm2, eax, 5).unwrap();
            a.pinsrw(xmm3, word_ptr(esi + 4), 6).unwrap();
            a.pextrw(edx, xmm2, 5).unwrap();
            a.pmuludq(xmm2, xmm3).unwrap();
            a.psadbw(xmm2, xmm1).unwrap();
            a.pslldq(xmm2, 3).unwrap();
            a.psrldq(xmm2, 1).unwrap();
            a.movss(xmm2, xmm0).unwrap();
            a.movsd_2(xmm2, xmm1).unwrap();
            a.movlps(xmm2, qword_ptr(esi)).unwrap();
            a.movhps(xmm2, qword_ptr(edi)).unwrap();
            a.movlps(qword_ptr(edi + 16), xmm2).unwrap();
            a.movhps(qword_ptr(edi + 24), xmm2).unwrap();
            a.movlhps(xmm2, xmm0).unwrap();
            a.movhlps(xmm2, xmm1).unwrap();
            a.pmovmskb(ebp, xmm2).unwrap();
            a.movmskps(ebx, xmm2).unwrap();
            a.xor(ebp, ebx).unwrap();
            a.movmskpd(ebx, xmm0).unwrap();
            a.add(edx, ebp).unwrap();
            a.add(edx, ebx).unwrap();
            a.mov(dword_ptr(edi + 32), edx).unwrap();
            a.movdqu(xmmword_ptr(edi + 48), xmm2).unwrap();
            a.emms().unwrap();
        });
    });
    let mut at = initial();
    at.regs[0] = 0x1234_5678;
    at.regs[1] = 4;
    at.regs[6] = DATA + 0x100;
    at.regs[7] = DATA + 0x200;
    let built = differential(&code, &at, |mem| {
        for i in 0..64u32 {
            let v = i.wrapping_mul(0x2468_1357).wrapping_add(0x80ff_7f81);
            mem.write(DATA + 0x100 + i * 4, &v.to_le_bytes()).unwrap();
            mem.write(DATA + 0x200 + i * 4, &(!v).to_le_bytes())
                .unwrap();
        }
    });
    assert!(built.assumed.mmx);
}

fn resident_unit(code: &[u8]) -> Unit {
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
    unit.trace.x86 = unit
        .trace
        .insts
        .iter()
        .map(|inst| {
            let from = (inst.pc - CODE) as usize;
            let to = (inst.next_pc - CODE) as usize;
            Some(iced_x86::Decoder::with_ip(32, &code[from..to], u64::from(inst.pc), 0).decode())
        })
        .collect();
    unit
}

fn resident_loop(body: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    assemble(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        body(a);
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
    })
}

fn resident(unit: &Unit, proof: Option<&Proof>) -> Built {
    build_with(unit, proof, &initial(), &|_| false, &|_| false).unwrap()
}

// spec: A64-OPT-3, A64-OPT-9
#[test]
fn an_inlined_call_push_resumes_at_the_callee() {
    let code = assemble(|a| {
        let mut head = a.create_label();
        let mut callee = a.create_label();
        let mut done = a.create_label();
        a.set_label(&mut head).unwrap();
        a.call(callee).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.jmp(done).unwrap();
        a.set_label(&mut callee).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.ret().unwrap();
        a.set_label(&mut done).unwrap();
        a.nop().unwrap();
    });
    let unit = resident_unit(&code);
    let call = unit.trace.insts[0];
    let decode::Op::Call(target) = call.op else {
        panic!("{:?}", call.op)
    };
    let built = resident(&unit, None);
    let eips: Vec<_> = built.func.records.iter().map(|r| r.eip).collect();
    // The push's record is the only one at the callee entry, which
    // continues the push's region.
    assert!(eips.contains(&target), "{eips:x?}");
}

// spec: A64-OPT-2, A64-OPT-9
#[test]
fn a_folded_constant_branch_skips_its_dead_arm_and_unreachable_blocks() {
    let code = assemble(|a| {
        let mut head = a.create_label();
        let mut dead = a.create_label();
        let mut join = a.create_label();
        a.set_label(&mut head).unwrap();
        a.mov(eax, 0).unwrap();
        a.test(eax, eax).unwrap();
        a.jne(dead).unwrap();
        a.fld(qword_ptr(esi)).unwrap();
        a.jmp(join).unwrap();
        a.set_label(&mut dead).unwrap();
        a.nop().unwrap();
        a.set_label(&mut join).unwrap();
        a.fstp(qword_ptr(edi)).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
    });
    let unit = resident_unit(&code);
    let built = resident(&unit, None);
    assert_eq!(verify::verify(&built.func), Ok(()));
}

// spec: A64-OPT-9
#[test]
fn resident_accepts_unproved_pointer_reads_and_indexed_stores() {
    for store in [false, true] {
        let code = resident_loop(|a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            if store {
                a.mov(dword_ptr(edi + eax * 4), ebx).unwrap();
            } else {
                a.mov(ebx, dword_ptr(eax)).unwrap();
                a.mov(dword_ptr(edi), ebx).unwrap();
            }
            a.add(esi, 4).unwrap();
        });
        let unit = resident_unit(&code);
        let proof = Proof::build(&unit.trace).ok();
        let checked_pc = unit.trace.insts[1].pc;
        for proof in std::iter::once(None).chain(proof.as_ref().map(Some)) {
            let built =
                build_with(&unit, proof, &initial(), &|pc| pc == checked_pc, &|_| false).unwrap();
            assert_eq!(
                count(
                    &built,
                    |op| matches!(op, Op::Load { checked: true, pc, .. } | Op::Store { checked: true, pc, .. } if *pc == checked_pc)
                ),
                1
            );
            assert_eq!(
                count(
                    &built,
                    |op| matches!(op, Op::Load { checked: false, pc, .. } if *pc == CODE)
                ),
                1
            );
        }
    }
}

// spec: A64-OPT-5, A64-OPT-9
#[test]
fn resident_hoists_only_with_entry_page_facts_within_budget() {
    for step in [4, 4096] {
        let code = resident_loop(|a| {
            for i in 0..10 {
                a.mov(eax, dword_ptr(esi + 4088 + i * step)).unwrap();
            }
            a.mov(dword_ptr(esi + 64), ebx).unwrap();
        });
        let unit = resident_unit(&code);
        let proof = Proof::build(&unit.trace).unwrap();
        for proof in [None, Some(&proof)] {
            let refused = unit.trace.insts[1].pc;
            let built = build_with(&unit, proof, &initial(), &|_| false, &|pc| {
                step == 4096 && pc == refused
            })
            .unwrap();
            let entry = &built.func.blocks[0].insts;
            let sites: Vec<_> = entry
                .iter()
                .filter_map(|(_, op)| match op {
                    Op::Load { pc, .. } => Some(*pc),
                    _ => None,
                })
                .collect();
            let facts: Vec<_> = entry
                .iter()
                .filter_map(|(_, op)| match op {
                    Op::PlainPage {
                        bytes,
                        write: false,
                        ..
                    } => Some(*bytes),
                    _ => None,
                })
                .collect();
            if step == 4 {
                assert_eq!(built.plain_sites, [CODE]);
                assert_eq!(facts, [40]);
                assert_eq!(sites.len(), 10);
                let rebuilt =
                    build_with(&unit, proof, &initial(), &|_| false, &|pc| pc == CODE).unwrap();
                assert!(rebuilt.plain_sites.is_empty());
            } else {
                assert_eq!(built.plain_sites, sites);
                assert_eq!(facts, [4; 8]);
                assert!(!sites.contains(&refused));
            }
            assert_eq!(count(&built, |op| matches!(op, Op::Load { .. })), 10);
        }
    }
}

// spec: A64-OPT-5, A64-OPT-6, A64-OPT-9
#[test]
fn resident_page_cross_checks_overlap_in_the_current_region() {
    for (offset, earlier_store, expected) in [(0, false, 1), (4, false, 0), (0, true, 0)] {
        let code = resident_loop(|a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            if earlier_store {
                a.mov(dword_ptr(esi + 8), ebx).unwrap();
            }
            a.mov(dword_ptr(esi + offset), eax).unwrap();
        });
        let unit = resident_unit(&code);
        let proof = Proof::build(&unit.trace).unwrap();
        for proof in [None, Some(&proof)] {
            let built = resident(&unit, proof);
            assert_eq!(
                count(&built, |op| matches!(op, Op::PageCross { .. })),
                expected
            );
            if expected == 0 {
                continue;
            }
            let mut memory = memory(&code);
            let mut at = initial();
            at.regs[6] = DATA + 4095;
            memory.write(DATA + 4095, &123u32.to_le_bytes()).unwrap();
            let run = eval::run(
                &built.func,
                &params(&built.func, &at, &mut memory),
                &mut memory,
                1000,
            );
            let eval::Outcome::Fail(region) = run.outcome else {
                panic!("{:?}", run.outcome)
            };
            let record = &built.func.records[region.0 as usize];
            assert_eq!(record.eip, CODE);
            assert_eq!(run.charged - i64::from(record.refund), 0);
        }
    }
}

// spec: A64-OPT-3, A64-OPT-6, A64-OPT-9
#[test]
fn resident_frame_tiers_keep_cells_and_publish_write_through_stores() {
    for tier in 0..3 {
        let code = resident_loop(|a| {
            a.mov(dword_ptr(esp - 4), 7).unwrap();
            if tier >= 1 {
                a.mov(ebx, dword_ptr(esi)).unwrap();
            }
            if tier >= 2 {
                a.mov(dword_ptr(esi), 9).unwrap();
            }
            a.mov(eax, dword_ptr(esp - 4)).unwrap();
        });
        let unit = resident_unit(&code);
        // A detached proof requires a non-frame output.
        let proof = Proof::build(&unit.trace).ok();
        assert_eq!(proof.is_some(), tier == 2);
        for proof in std::iter::once(None).chain(proof.as_ref().map(Some)) {
            let built = resident(&unit, proof);
            assert_eq!(built.plain_sites, [u32::MAX]);
            assert_eq!(count(&built, |op| matches!(op, Op::Store { .. })), tier);
            assert_eq!(count(&built, |op| matches!(op, Op::FrameLoad { .. })), 0);
            assert_eq!(
                count(&built, |op| matches!(op, Op::FrameDisjoint { .. })),
                usize::from(tier == 2)
            );
            let mut memory = resident_memory();
            let mut at = initial();
            at.regs[6] = if tier == 2 { DATA } else { FRAME - 4 };
            let run = eval::run(
                &built.func,
                &params(&built.func, &at, &mut memory),
                &mut memory,
                1000,
            );
            let eval::Outcome::Exit(exit) = run.outcome else {
                panic!("{:?}", run.outcome)
            };
            let record = &built.func.records[exit.0 as usize];
            assert_eq!(
                eval::locate(record.regs[0].as_ref().unwrap(), &run.values),
                7
            );
            if tier == 0 {
                assert_eq!(record.frame.len(), 1);
            } else {
                assert!(built.func.records.iter().all(|r| r.frame.is_empty()));
                let mut bytes = [0; 4];
                memory.read_into(FRAME - 4, &mut bytes).unwrap();
                assert_eq!(u32::from_le_bytes(bytes), 7);
                if tier == 1 {
                    assert_eq!(
                        eval::locate(record.regs[3].as_ref().unwrap(), &run.values),
                        7
                    );
                }
            }
        }
    }
}

fn resident_memory() -> Memory {
    #[cfg(unix)]
    let mut memory = Memory::window(CODE, 0x30000).unwrap();
    #[cfg(not(unix))]
    let mut memory = Memory::new(CODE, 0x30000).unwrap();
    memory.map(CODE, 0x30000, 3).unwrap();
    memory
}

// spec: A64-OPT-3, A64-OPT-6, A64-OPT-9
#[test]
fn resident_ebp_cells_survive_an_indexed_store_until_it_aliases() {
    let code = resident_loop(|a| {
        a.mov(eax, dword_ptr(ebp - 4)).unwrap();
        a.add(eax, ecx).unwrap();
        a.mov(dword_ptr(ebp - 4), eax).unwrap();
        a.mov(esi, eax).unwrap();
        a.mov(dword_ptr(edi + eax * 4), ebx).unwrap();
        a.mov(edx, dword_ptr(ebp - 4)).unwrap();
    });
    let unit = resident_unit(&code);
    let proof = Proof::build(&unit.trace).ok();
    for proof in std::iter::once(None).chain(proof.as_ref().map(Some)) {
        let mut at = initial();
        at.regs[EBP as usize] = FRAME + 16;
        let built = build_with(&unit, proof, &at, &|_| false, &|_| false).unwrap();
        assert_eq!(built.assumed.ebp, Some(16));
        assert!(built.func.entry.contains(&Source::Frame {
            offset: 12,
            width: 4
        }));
        assert_eq!(
            count(&built, |op| matches!(
                op,
                Op::Load { .. } | Op::FrameLoad { .. }
            )),
            0
        );
        assert_eq!(
            count(&built, |op| matches!(
                op,
                Op::FrameDisjoint { lo: 12, hi: 16, .. }
            )),
            1
        );
        for (destination, expected) in [(DATA, 7u32), (FRAME - 4, 4)] {
            at.regs[7] = destination;
            let mut memory = resident_memory();
            memory.write(FRAME + 12, &1u32.to_le_bytes()).unwrap();
            let run = eval::run(
                &built.func,
                &params(&built.func, &at, &mut memory),
                &mut memory,
                1000,
            );
            match run.outcome {
                eval::Outcome::Exit(id) if destination == DATA => {
                    let record = &built.func.records[id.0 as usize];
                    assert_eq!(
                        eval::locate(record.regs[2].as_ref().unwrap(), &run.values),
                        7
                    );
                }
                eval::Outcome::Fail(id) if destination != DATA => {
                    let record = &built.func.records[id.0 as usize];
                    assert_eq!(record.eip, unit.trace.insts[3].pc);
                    assert_eq!(run.charged - i64::from(record.refund), 3);
                }
                other => panic!("{other:?}"),
            }
            let mut bytes = [0; 4];
            memory.read_into(FRAME + 12, &mut bytes).unwrap();
            assert_eq!(u32::from_le_bytes(bytes), expected);
        }
    }
}

// spec: A64-OPT-3, A64-OPT-9
#[test]
fn resident_dynamic_esp_disables_cells_and_escape_uses_write_through() {
    for dynamic in [false, true] {
        let code = resident_loop(|a| {
            a.mov(dword_ptr(esp - 4), 7).unwrap();
            if dynamic {
                a.sub(esp, ebx).unwrap();
            } else {
                a.mov(eax, esp).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            }
        });
        let unit = resident_unit(&code);
        let proof = Proof::build(&unit.trace).ok();
        for proof in std::iter::once(None).chain(proof.as_ref().map(Some)) {
            let built = resident(&unit, proof);
            assert_eq!(built.plain_sites.is_empty(), dynamic);
            assert_eq!(
                count(&built, |op| matches!(op, Op::Store { .. })),
                if dynamic { 1 } else { 2 }
            );
            assert!(built.func.records.iter().all(|r| r.frame.is_empty()));
        }
    }
}

// spec: A64-OPT-9
#[test]
fn resident_division_uses_unconditional_local_facts() {
    for divisor in [-1, 0, 5] {
        let code = resident_loop(|a| {
            a.cdq().unwrap();
            a.mov(ebx, divisor).unwrap();
            a.idiv(ebx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
        });
        let unit = resident_unit(&code);
        let proof = Proof::build(&unit.trace).ok();
        for proof in std::iter::once(None).chain(proof.as_ref().map(Some)) {
            let built = resident(&unit, proof);
            assert_eq!(
                count(&built, |op| matches!(op, Op::Div { proved: true, .. })),
                usize::from(divisor == 5)
            );
        }
    }
}
