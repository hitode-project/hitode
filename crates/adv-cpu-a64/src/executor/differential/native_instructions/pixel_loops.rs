//! Authored pixel arithmetic; no engine code or fingerprints.
use super::*;

fn configs() -> impl Iterator<Item = Config> {
    [false, true].into_iter().flat_map(|flagm| {
        [false, true].into_iter().flat_map(move |first| {
            [Checks::Adaptive, Checks::All, Checks::Slow]
                .into_iter()
                .map(move |checks| (Features { flagm }, first, checks))
        })
    })
}

fn blend(a: &mut CodeAssembler, xmm: bool) {
    macro_rules! body {
        ($s:expr, $d:expr, $z:expr, $w:expr, $load:ident, $mem:ident) => {
            a.$load($s, $mem(esi)).unwrap();
            a.$load($d, $mem(edi)).unwrap();
            a.punpcklbw($s, $z).unwrap();
            a.punpcklbw($d, $z).unwrap();
            a.psubw($s, $d).unwrap();
            a.pmullw($s, $w).unwrap();
            a.psllw($d, 8).unwrap();
            a.paddw($s, $d).unwrap();
            a.psrlw($s, 8).unwrap();
            a.packuswb($s, $z).unwrap();
            a.$load($mem(edi), $s).unwrap();
        };
    }
    if xmm {
        body!(xmm3, xmm5, xmm1, xmm6, movq, qword_ptr);
    } else {
        body!(mm3, mm5, mm1, mm6, movd, dword_ptr);
    }
}

fn seeded(mut p: Program) -> Program {
    for (n, b) in p.data.iter_mut().enumerate() {
        *b = (n * 71 + n / 7) as u8;
    }
    p.state.regs[1] = 7;
    p.state.regs[6] = DATA + 64;
    p.state.regs[7] = DATA + 8192;
    p.state.xmm = std::array::from_fn(|n| [n as u8 * 17; 16]);
    p.state.mmx = std::array::from_fn(|n| 0x8172635445362718 ^ n as u64);
    p.state.x87.regs = std::array::from_fn(|n| {
        adv_cpu_core::X87Register::from_parts(0x8172635445362718 ^ n as u64, 0x3fff)
    });
    p.state.xmm[1] = [0; 16];
    p.state.mmx[1] = 0;
    p.state.xmm[6] = [93, 0].repeat(8).try_into().unwrap();
    p.state.mmx[6] = 93 * 0x0001000100010001;
    p.state.x87.status = 3 << 11;
    p
}

fn program(xmm: bool, side: bool) -> Program {
    seeded(fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        blend(a, xmm);
        if side {
            a.cmp(ecx, 3).unwrap();
            a.je(u64::from(CODE + 0x800)).unwrap();
        }
        a.add(esi, if xmm { 8 } else { 4 }).unwrap();
        a.add(edi, if xmm { 8 } else { 4 }).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    }))
}

// spec: A64-TR-5, A64-VER-1, A64-CTL-7, A64-REG-5, A64-FP-4
#[test]
fn pixel_blends_preserve_every_state_at_budget_cuts_overlap_and_side_exits() {
    for xmm in [false, true] {
        for side in [false, true] {
            let mut p = program(xmm, side);
            if side {
                let mut tail = CodeAssembler::new(32).unwrap();
                // Observe both register banks after leaving the steady body.
                tail.movq(qword_ptr((DATA + 0x3000) as i32), mm3).unwrap();
                tail.movdqu(xmmword_ptr((DATA + 0x3100) as i32), xmm5)
                    .unwrap();
                tail.jmp(u64::from(p.stop)).unwrap();
                p.code.resize(0x800, 0x90);
                p.code
                    .extend(tail.assemble(u64::from(CODE + 0x800)).unwrap());
            }
            for config in configs() {
                for budget in [1, 13, 15, 16, 17, 29, 30, 31, 32, 46, 47, 48, 75, 1000] {
                    let mut native = executor(&p, config, false);
                    let mut oracle = executor(&p, config, true);
                    assert_eq!(
                        native.run(p.stop, budget).unwrap(),
                        oracle.run(p.stop, budget).unwrap()
                    );
                    same(&native, &oracle);
                    assert_eq!(
                        native.run(p.stop, 1000).unwrap(),
                        oracle.run(p.stop, 1000).unwrap()
                    );
                    same(&native, &oracle);
                }
            }
            for delta in [-8, -1, 0, 1, 4, 8] {
                p.state.regs[7] = p.state.regs[6].wrapping_add_signed(delta);
                for config in configs() {
                    let mut native = executor(&p, config, false);
                    let mut oracle = executor(&p, config, true);
                    assert_eq!(
                        native.run(p.stop, 1000).unwrap(),
                        oracle.run(p.stop, 1000).unwrap()
                    );
                    same(&native, &oracle);
                }
            }
        }
    }
}

// spec: A64-TR-5, A64-MEM-5, A64-MEM-10, A64-FLAG-4, A64-VER-1
#[test]
fn steady_pixel_faults_recover_both_simd_banks_and_retry_the_original_instruction() {
    for xmm in [false, true] {
        for store in [false, true] {
            for crossing in [false, true] {
                let mut p = program(xmm, false);
                let width = if xmm { 8 } else { 4 };
                // Two complete iterations precede the fault, in the steady copy.
                p.state.regs[if store { 7 } else { 6 }] =
                    DATA + DATA_LEN as u32 - width * 2 - if crossing { 1 } else { 0 };
                for flagm in [false, true] {
                    let config = (Features { flagm }, false, Checks::Adaptive);
                    let mut native = executor(&p, config, false);
                    let mut oracle = executor(&p, config, true);
                    if store {
                        for cpu in [&mut native, &mut oracle] {
                            cpu.protect(DATA + DATA_LEN as u32, 0x4000, 1).unwrap();
                        }
                    }
                    let outcome = native.run(p.stop, 1000).unwrap();
                    assert!(matches!(
                        outcome,
                        RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                    ));
                    assert_eq!(outcome, oracle.run(p.stop, 1000).unwrap());
                    same(&native, &oracle);
                    assert!(native.native.as_ref().unwrap().unchecked_faults > 0);
                    for cpu in [&mut native, &mut oracle] {
                        cpu.protect(DATA + DATA_LEN as u32, 0x4000, 3).unwrap();
                        cpu.write(DATA + DATA_LEN as u32, &[0x93; 0x4000]).unwrap();
                    }
                    assert_eq!(
                        native.run(p.stop, 1000).unwrap(),
                        oracle.run(p.stop, 1000).unwrap()
                    );
                    same(&native, &oracle);
                    assert_eq!(
                        native
                            .memory()
                            .read(DATA + DATA_LEN as u32, 0x4000)
                            .unwrap(),
                        oracle
                            .memory()
                            .read(DATA + DATA_LEN as u32, 0x4000)
                            .unwrap()
                    );
                }
            }
        }
    }
}

// spec: A64-TR-5, A64-CTL-10, A64-VER-1
#[test]
fn pixel_code_changes_invalidate_the_steady_copy_and_preserve_pending_x87_exceptions() {
    for xmm in [false, true] {
        let p = program(xmm, false);
        let config = (Features { flagm: true }, false, Checks::Adaptive);
        let mut native = executor(&p, config, false);
        let mut oracle = executor(&p, config, true);
        for changed in [false, true] {
            for cpu in [&mut native, &mut oracle] {
                cpu.state = p.state.clone();
                cpu.write(DATA, &p.data).unwrap();
                if changed {
                    let mut replacement = CodeAssembler::new(32).unwrap();
                    if xmm {
                        replacement.paddw(xmm3, xmm6).unwrap();
                    } else {
                        replacement.paddw(mm3, mm6).unwrap();
                    }
                    let decoded: Vec<_> =
                        iced_x86::Decoder::with_ip(32, &p.code, u64::from(CODE), 0)
                            .iter()
                            .collect();
                    let mul = decoded
                        .iter()
                        .find(|i| i.mnemonic() == iced_x86::Mnemonic::Pmullw)
                        .unwrap();
                    let bytes = replacement.assemble(mul.ip()).unwrap();
                    assert_eq!(bytes.len(), mul.len());
                    cpu.write(mul.ip32(), &bytes).unwrap();
                }
            }
            assert_eq!(
                native.run(p.stop, 1000).unwrap(),
                oracle.run(p.stop, 1000).unwrap()
            );
            same(&native, &oracle);
        }
        assert!(native.stats.invalidations > 0);
        if !xmm {
            let mut p = p;
            p.state.x87.status |= 0x81;
            p.state.x87.control &= !1;
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            assert_eq!(
                native.run(p.stop, 1000).unwrap(),
                oracle.run(p.stop, 1000).unwrap()
            );
            same(&native, &oracle);
        }
    }
}

// spec: A64-TR-5, A64-VER-1
#[test]
fn fixed_point_gathers_saturation_and_unrolled_loops_are_register_and_address_independent() {
    for unroll in [1, 2, 4] {
        let mut p = seeded(fixture(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            for _ in 0..unroll {
                a.mov(eax, ebx).unwrap();
                a.sar(eax, 16).unwrap();
                a.movd(mm2, dword_ptr(ebp + eax * 4)).unwrap();
                a.paddusb(mm2, mm4).unwrap();
                a.movd(dword_ptr(edx), mm2).unwrap();
                a.add(ebx, esi).unwrap();
                a.add(edx, 4).unwrap();
            }
            a.cmp(edx, edi).unwrap();
            a.jb(top).unwrap();
        }));
        p.state.regs[2] = DATA + 8193;
        p.state.regs[5] = DATA + 128;
        p.state.regs[7] = p.state.regs[2] + unroll * 4 * 5;
        for step in [0u32, 0x8000, 0x18000, 0xffff8000] {
            p.state.regs[3] = 0x80000;
            p.state.regs[6] = step;
            for config in configs() {
                let mut native = executor(&p, config, false);
                let mut oracle = executor(&p, config, true);
                assert_eq!(
                    native.run(p.stop, 2000).unwrap(),
                    oracle.run(p.stop, 2000).unwrap()
                );
                same(&native, &oracle);
            }
        }
    }
}

// spec: A64-TR-5, A64-MEM-10, A64-VER-1
#[test]
fn mixed_register_banks_and_adjacent_pixel_loops_publish_the_same_architecture() {
    let p = seeded(fixture(|a| {
        for xmm in [false, true] {
            let mut top = a.create_label();
            a.mov(ecx, 3).unwrap();
            a.set_label(&mut top).unwrap();
            blend(a, xmm);
            if xmm {
                a.paddb(mm0, mm2).unwrap();
            } else {
                a.paddb(xmm0, xmm2).unwrap();
            }
            a.add(esi, 8).unwrap();
            a.add(edi, 8).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        }
        a.pushad().unwrap();
        a.popad().unwrap();
    }));
    for config in configs() {
        for budget in [1, 17, 18, 19, 36, 37, 38, 53, 54, 55, 70, 71, 1000] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            assert_eq!(
                native.run(p.stop, budget).unwrap(),
                oracle.run(p.stop, budget).unwrap()
            );
            same(&native, &oracle);
        }
        let mut p = seeded(fixture(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            a.movdqu(xmm4, xmmword_ptr(esi)).unwrap();
            a.paddb(mm0, mm2).unwrap();
            a.paddusb(xmm4, xmm2).unwrap();
            a.movdqu(xmmword_ptr(edi), xmm4).unwrap();
            a.add(esi, 16).unwrap();
            a.add(edi, 16).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        }));
        p.state.regs[6] = DATA + DATA_LEN as u32 - 32;
        let mut native = executor(&p, config, false);
        let mut oracle = executor(&p, config, true);
        assert_eq!(
            native.run(p.stop, 1000).unwrap(),
            oracle.run(p.stop, 1000).unwrap()
        );
        same(&native, &oracle);
    }
}

// spec: A64-REG-5, A64-FP-4, A64-VER-1
#[test]
fn pixel_weight_broadcasts_preserve_every_lane_and_both_register_widths() {
    for lane in 0..4 {
        let p = seeded(fixture(|a| {
            a.pshufw(mm4, mm6, lane * 0x55).unwrap();
            a.pshufd(xmm4, xmm6, lane * 0x55).unwrap();
            a.pshufw(mm6, mm6, lane * 0x55).unwrap();
            a.pshufd(xmm6, xmm6, lane * 0x55).unwrap();
        }));
        let mut p = p;
        p.state.mmx[6] = 0x8172635445362718;
        p.state.xmm[6] = std::array::from_fn(|n| n as u8 * 17);
        for config in configs() {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            assert_eq!(
                native.run(p.stop, 1000).unwrap(),
                oracle.run(p.stop, 1000).unwrap()
            );
            same(&native, &oracle);
        }
    }
}
