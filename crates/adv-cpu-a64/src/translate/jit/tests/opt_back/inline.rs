use super::*;
use crate::facts::Assumed;
use adv_cpu_core::X87Register;
use std::cell::RefCell;

const ALIAS_BASE: u64 = 8 << 32;
const X7: Reg = Reg::new(7);
const X8: Reg = Reg::new(8);
const SHADOW: u64 = 0x5000_1230;
const NORMAL: Exit = Exit::Dispatch;
const MISS: Exit = Exit::Budget;
const COLD: Exit = Exit::Recall;

fn test_harness() -> Harness {
    let mut h = harness();
    for base in [WINDOW_BASE, ALIAS_BASE] {
        for (start, len) in [(0, 0x20000), (0x20000, 0x10000), (0x30000, 0x20000)] {
            if base != WINDOW_BASE || start != 0x20000 {
                h.emu.map(base + start, len);
            }
        }
    }
    h
}

fn assumptions(cpu: &CpuState) -> Assumed {
    Assumed {
        control: cpu.x87.control,
        status: cpu.x87.status & !0x3800,
        mxcsr: cpu.mxcsr,
        top: cpu.x87.top() as u8,
        tags: cpu.x87.tag,
        esp_low: None,
        ebp: None,
        denormal: cpu.x87.status & 2 != 0,
        x87: false,
        mmx: false,
        mxcsr_mask: 0xffc0,
    }
}

struct InlineRun {
    code: Vec<u32>,
    target: usize,
}

impl InlineRun {
    /// Put Tier-1 after the inline form to exercise a patched normal link.
    /// With no successor, capture the unpatched dispatch prefix instead.
    fn new(h: &Harness, lowered: &back::Lowered, successor: &[u32]) -> Self {
        let mut code = h.prefix.clone();
        let dispatch = code.len() * 4;
        code.extend([
            mov(W, X7, X1),
            mov(X, X8, X2),
            ldr(X, X6, RUNTIME, offset_of!(runtime::Runtime, cpu) as u32),
            ldr(W, X6, X6, runtime::EIP),
            movz(W, X0, NORMAL.word(), 0),
            b(h.stubs.exit as i32 - (dispatch as i32 + 20)),
        ]);

        let mut leave = |reason: Exit| {
            let at = code.len() * 4;
            code.extend([
                movz(W, X0, reason.word(), 0),
                ldr(X, X1, RUNTIME, offset_of!(runtime::Runtime, cpu) as u32),
                ldr(W, X1, X1, runtime::EIP),
                b(h.stubs.exit as i32 - (at as i32 + 12)),
            ]);
            at
        };
        let miss = leave(MISS);
        let cold = leave(COLD);
        let at = code.len() * 4;
        code.extend_from_slice(&lowered.words);
        code[at / 4 + lowered.miss] = b(miss as i32 - (at + 4 * lowered.miss) as i32);
        code[at / 4 + lowered.cold] = b(cold as i32 - (at + 4 * lowered.cold) as i32);
        let next = code.len() * 4;
        for &(offset, _) in &lowered.links {
            let link = at + offset;
            if successor.is_empty() {
                code[link / 4 + 3] = b(dispatch as i32 - link as i32 - 12);
            } else {
                code[link / 4] = b(next as i32 - link as i32);
            }
        }
        code.extend_from_slice(successor);
        Self {
            code,
            target: at + 4 * lowered.entry,
        }
    }

    fn linked(
        h: &Harness,
        lowered: &back::Lowered,
        memory: &mut Memory,
        eip: u32,
        len: usize,
        control: u16,
    ) -> Self {
        let at = Self::new(h, lowered, &[]).code.len() * 4;
        let addr = |off| HOST_CODE as usize + off;
        let stubs = Stubs {
            call: addr(h.forbidden),
            exit: addr(h.stubs.exit),
            dispatch: addr(h.dispatch),
            lookup: addr(h.forbidden),
            lookup_slow: addr(h.forbidden),
            ret_miss: addr(h.forbidden),
            access: addr(h.forbidden),
            fault: addr(h.forbidden),
            opt_exit: addr(h.forbidden),
        };
        let mut work = Workspace::default();
        translate(
            &mut work,
            memory,
            eip,
            &[eip + len as u32],
            &|_| false,
            0,
            Features { flagm: false },
            addr(at),
            stubs,
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            control,
            false,
        );
        Self::new(h, lowered, work.words())
    }

    /// Models a Tier-1 predecessor with stale CPU GPRs and mapped flags.
    fn with_stale_integer_state(mut self) -> Self {
        let trampoline = self.code.len() * 4;
        self.code.push(ldr(
            X,
            X0,
            RUNTIME,
            offset_of!(runtime::Runtime, cpu) as u32,
        ));
        for offset in [0, 8, 16, 24] {
            self.code
                .push(stp(W, Index::Offset, Reg::ZR, Reg::ZR, X0, offset));
        }
        self.code.extend([
            ldr(W, X1, X0, runtime::EFLAGS),
            movz(W, X2, runtime::MAPPED_FLAGS as u16, 0),
            bic(W, X1, X1, X2, Lsl, 0),
            str(W, X1, X0, runtime::EFLAGS),
            // Two toggled bits preserve PF. Bit 7 is outside AF's source bit.
            eor_imm(W, Reg::new(10), Reg::new(10), 3),
            orr_imm(W, Reg::new(11), Reg::new(11), 0x80),
        ]);
        self.code
            .push(b(self.target as i32 - (self.code.len() * 4) as i32));
        self.target = trampoline;
        self
    }

    fn run(&self, h: &mut Harness, cpu: &CpuState, countdown: u64) -> Exit {
        let mut context = vec![0; size_of::<runtime::Runtime>()];
        let mut put = |offset: usize, bytes: &[u8]| {
            context[offset..offset + bytes.len()].copy_from_slice(bytes);
        };
        for (offset, value) in [
            (offset_of!(runtime::Runtime, cpu), CPU),
            (offset_of!(runtime::Runtime, window), WINDOW_BASE),
            (offset_of!(runtime::Runtime, shadow), SHADOW),
            (offset_of!(runtime::Runtime, alias), ALIAS_BASE),
            (offset_of!(runtime::Runtime, countdown), countdown),
            (offset_of!(runtime::Runtime, completed_limit), u64::MAX),
            (offset_of!(runtime::Runtime, recall), CONTEXT + 0x3f00),
            (
                offset_of!(runtime::Runtime, computation_pending),
                CONTEXT + 0x3f00,
            ),
        ] {
            put(offset, &value.to_le_bytes());
        }
        put(
            offset_of!(runtime::Runtime, rep_pending),
            &u32::from(cpu.rep.is_some()).to_le_bytes(),
        );
        h.emu.write(CONTEXT, &context);
        h.emu.write(CONTEXT + 0x3f00, &[0; 4]);
        h.emu.write(CPU, &state_bytes(cpu));
        h.emu.set_x(X0, CONTEXT);
        h.emu.set_x(X1, HOST_CODE + self.target as u64);
        h.emu.set_sp(HOST_STACK);
        for n in 19..30 {
            h.emu.set_x(Reg::new(n), 0xcafe_0000 + u64::from(n));
        }
        for n in 8..16 {
            h.emu.set_q(n, 0xbeef_0000 + u128::from(n));
        }
        if h.loaded == self.code {
            h.emu.call_loaded_at(h.stubs.enter)
        } else {
            let result = h.emu.call_at(&self.code, h.stubs.enter);
            h.loaded.clone_from(&self.code);
            result
        }
        .expect("inline form returns through its selected edge");
        assert_eq!(h.emu.sp(), HOST_STACK);
        assert_eq!(
            read64(h, CONTEXT + offset_of!(runtime::Runtime, shadow) as u64),
            SHADOW
        );
        for n in 19..30 {
            assert_eq!(h.emu.x(Reg::new(n)), 0xcafe_0000 + u64::from(n));
        }
        for n in 8..16 {
            assert_eq!(h.emu.q(n) as u64, 0xbeef_0000 + u64::from(n));
        }
        Exit::from_raw(h.emu.x(X0) as u32).unwrap()
    }

    fn link(&self, h: &Harness) -> (usize, u32) {
        (
            (h.emu.x(X8) - HOST_CODE - self.target as u64) as usize,
            h.emu.x(X7) as u32,
        )
    }
}

fn read64(h: &Harness, addr: u64) -> u64 {
    u64::from_le_bytes(h.emu.read(addr, 8).try_into().unwrap())
}

fn countdown(h: &Harness) -> u64 {
    h.emu.x(X1).wrapping_add(read64(
        h,
        CONTEXT + offset_of!(runtime::Runtime, completed) as u64,
    ))
}

fn assert_state(h: &Harness, expected: &CpuState) {
    let want = state_bytes(expected);
    let got = h.emu.read(CPU, want.len());
    let regs = offset_of!(CpuState, regs);
    let x87 = offset_of!(CpuState, x87);
    assert_eq!(&got[regs..regs + 32], &want[regs..regs + 32], "GPRs");
    assert_eq!(&got[..x87], &want[..x87], "integer and SIMD state");
    assert_eq!(&got[x87..], &want[x87..], "x87 and remaining state");
}

fn compare_exit(
    h: &mut Harness,
    func: &Func,
    assumed: &Assumed,
    cpu: &CpuState,
    params: &[u128],
    budget: u64,
) -> RecordId {
    let captured = h.run_opt_inner(func, params, budget, 0, 0);
    h.loaded.clear(); // The job runner installs its own image.
    let mut expected = cpu.clone();
    let mut frame = Vec::new();
    materialize(
        &captured.lowered.records[captured.record.0 as usize],
        &captured.registers,
        &captured.spills,
        &captured.lowered.flags,
        &mut expected,
        |offset, bytes| frame.push((offset, bytes.to_vec())),
    );
    if assumed.mmx {
        expected.refresh_mmx();
    }
    let lowered = back::lower_inline(func, assumed, &func.entry);
    let run = InlineRun::new(h, &lowered, &[]);
    assert_eq!(run.run(h, cpu, budget), NORMAL);
    let link = run.link(h);
    assert!(lowered.links.contains(&link), "link {link:?}");
    assert_eq!(link.1, expected.eip, "exit record");
    assert_eq!(
        h.emu.x(X6) as u32,
        cpu.eip,
        "inline link leaves CpuState::eip"
    );
    assert_eq!(countdown(h), captured.countdown);
    assert_eq!(
        read64(h, CONTEXT + offset_of!(runtime::Runtime, completed) as u64),
        captured.completed,
    );
    assert_state(h, &expected);
    for (offset, bytes) in frame {
        let addr = cpu.regs[4].wrapping_add_signed(offset);
        h.emu.write(WINDOW_BASE + u64::from(addr), &bytes);
        assert_eq!(h.emu.read(ALIAS_BASE + u64::from(addr), bytes.len()), bytes);
    }
    captured.record
}

// spec: A64-OPT-6, A64-OPT-7
#[test]
fn opt_inline_exit_equivalence() {
    let mut h = test_harness();
    let mut cpu = initial();
    let (mut f, b, p, _) = start(&[Type::F64, Type::F64]);
    f.entry[1] = Source::X87(1);
    cpu.x87.regs[0] = X87Register::from_parts(1 << 63, 16383);
    cpu.x87.regs[1] = X87Register::from_parts(10 << 60, 16386);
    let mut assumed = assumptions(&cpu);
    assumed.x87 = true;
    for op in [FpOp::Add, FpOp::Sub, FpOp::Mul, FpOp::Div] {
        let mut f = f.clone();
        let v = f.push(b, Op::Fp(op, p[0], p[1])).unwrap();
        f.push(b, Op::Validate);
        let exit = finish(&mut f, b, v);
        f.records[exit.0 as usize].x87.as_mut().unwrap().c1 = Some(Location::Recipe(Recipe::C1 {
            op,
            a: p[0],
            b: p[1],
        }));
        let params = opt::admit(&f.entry, &assumed, &cpu, |_, _| unreachable!()).unwrap();
        compare_exit(&mut h, &f, &assumed, &cpu, &params, 100);
    }
}

fn read_entry(h: &Harness) -> runtime::OptEntryState {
    let addr = CONTEXT + offset_of!(runtime::Runtime, recovery.entry) as u64;
    let data = h.emu.read(addr, size_of::<runtime::OptEntryState>());
    let word = |at| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    let wide = |at| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    runtime::OptEntryState {
        regs: std::array::from_fn(|i| word(offset_of!(runtime::OptEntryState, regs) + 4 * i)),
        nzcv: wide(offset_of!(runtime::OptEntryState, nzcv)),
        pf: wide(offset_of!(runtime::OptEntryState, pf)),
        af: wide(offset_of!(runtime::OptEntryState, af)),
        df: wide(offset_of!(runtime::OptEntryState, df)),
    }
}

// spec: A64A-ROLE-8, A64-OPT-6, A64-OPT-7, A64-REG-1
#[test]
fn opt_inline_entry_equivalence() {
    let mut h = test_harness();
    let mut seed = 0x7ddfa8612b3a69f3u64;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut cpu = initial();
    cpu.mxcsr = 0x1fa0;
    cpu.x87.status = 0x20 | (3 << 11);
    cpu.x87.tag = 0x0140;
    let mut assumed = assumptions(&cpu);
    assumed.esp_low = Some(0);
    assumed.x87 = true;

    let mut f = Func::default();
    let entry = f.record(record(CODE));
    let block = f.block(Some(Head { record: entry }));
    f.entry = (0..8)
        .map(Source::Reg)
        .chain([Source::Eflags, Source::X87Ip, Source::X87Opcode])
        .chain((0..8).map(Source::X87))
        .chain((0..4).map(Source::Xmm))
        .chain((4..8).map(Source::Mmx))
        .chain(
            [1, 2, 4, 8]
                .into_iter()
                .enumerate()
                .map(|(i, width)| Source::Frame {
                    offset: -32 + 8 * i as i32,
                    width,
                }),
        )
        .collect();
    let mut exit = record(CODE + 1);
    exit.x87 = None;
    for (i, source) in f.entry.clone().into_iter().enumerate() {
        let p = f.param(block, source.ty());
        match source {
            Source::Xmm(n) | Source::Mmx(n) => exit.xmm[n as usize] = Some(Location::At(p)),
            _ => exit.frame.push((i as i32 * 8, 8, Location::At(p))),
        }
    }
    // The frame separately exposes the entry EFLAGS cell.
    exit.eflags = Some(Location::At(f.blocks[0].params[8]));
    let exit = f.record(exit);
    f.end(block, End::Exit(exit));

    let encodings = [
        (0, 0),
        (0, 0x8000),
        (0, 1),
        (1 << 63, 15361),
        (1 << 63, 15360),
        (1 << 63, 15309),
        (1 << 63, 15308),
        ((1 << 63) | 1, 0x3fff),
        (1 << 63, 0x7fff),
        (1 << 63, 0xffff),
        (3 << 62, 0x7fff),
        (1 << 62, 0x3fff),
        (1 << 63, 17406),
        (1 << 63, 17407),
    ];
    let mut counts = [0; 3];
    for (mask, x87, mmx) in [
        (0xffc0, true, false),
        (u32::MAX, true, false),
        (0xffc0, false, true),
    ] {
        assumed.mxcsr_mask = mask;
        assumed.x87 = x87;
        assumed.mmx = mmx;
        let lowered = back::lower_inline(&f, &assumed, &f.entry);
        let save = [
            add_imm(
                X,
                X4,
                Reg::new(29),
                offset_of!(runtime::Runtime, recovery.entry) as u32,
                false,
            ),
            stp(W, Index::Offset, Reg::new(19), Reg::new(20), X4, 0),
            stp(W, Index::Offset, Reg::new(21), Reg::new(22), X4, 8),
            stp(W, Index::Offset, Reg::new(23), Reg::new(24), X4, 16),
            stp(W, Index::Offset, Reg::new(25), Reg::new(26), X4, 24),
            mrs_nzcv(X16),
            stp(X, Index::Offset, X16, Reg::new(10), X4, 32),
            stp(X, Index::Offset, Reg::new(11), Reg::new(12), X4, 48),
        ];
        assert!(lowered.words.windows(save.len()).any(|words| words == save));
        let run = InlineRun::new(&h, &lowered, &[]).with_stale_integer_state();
        for iteration in 0..192 {
            let mut cpu = cpu.clone();
            for reg in &mut cpu.regs {
                *reg = random() as u32;
            }
            cpu.regs[4] = 0x3f000;
            cpu.eflags = random() as u32 & 0xfff | 2;
            cpu.x87.ip = random() as u32;
            cpu.x87.opcode = random() as u16;
            for i in 0..8 {
                cpu.x87.regs[i] = X87Register::from_parts(
                    1 << 63 | random() & !0x7ff,
                    (random() as u16 & 0x8000) | 0x3fff,
                );
                cpu.xmm[i] = (u128::from(random()) << 64 | u128::from(random())).to_le_bytes();
                cpu.mmx[i] = random();
            }
            // Each boundary encoding is tested alone, so an unrelated
            // inexact value can't mask the outcome.
            let (m, se) = encodings[iteration % encodings.len()];
            cpu.x87.regs[iteration % 8] = X87Register::from_parts(m, se);
            match (random() % 11, iteration / encodings.len()) {
                (_, 0) => {}
                (0, _) => cpu.x87.tag ^= 1 << (random() % 16),
                (1, _) => cpu.x87.status ^= 1 << (11 + random() % 3),
                (2, _) => cpu.x87.status ^= 1 << (random() % 9),
                (3, _) => cpu.x87.control ^= 1 << (random() % 16),
                (4, _) => cpu.mxcsr ^= 1 << (random() % 16),
                (5, _) => cpu.regs[4] += 1 + (random() % 15) as u32,
                (6, _) => cpu.mmx_mode = 1 << (random() % 16),
                (7, _) => adv_cpu_core::interpreter::RepContinuation::commit_native_compare(
                    &mut cpu, 6, true,
                ),
                _ => {}
            }
            let base = ALIAS_BASE + u64::from(cpu.regs[4]);
            let input: [u8; 32] = std::array::from_fn(|_| random() as u8);
            h.emu.write(base - 32, &input);
            h.emu.write(base, &[0xa5; 256]);
            let expected = opt::admit(&f.entry, &assumed, &cpu, |addr, width| {
                let i = (addr - (cpu.regs[4] - 32)) as usize;
                let mut bytes = [0; 8];
                bytes[..width as usize].copy_from_slice(&input[i..i + width as usize]);
                u64::from_le_bytes(bytes)
            });
            let reason = run.run(&mut h, &cpu, 100);
            if cpu.rep.is_some() || cpu.mmx_mode != 0 {
                assert_eq!(reason, COLD);
                counts[0] += 1;
            } else if let Some(expected) = expected {
                assert_eq!(reason, NORMAL, "iteration {iteration}");
                counts[1] += 1;
                for (i, (&source, value)) in f.entry.iter().zip(expected).enumerate() {
                    match source {
                        Source::Xmm(n) | Source::Mmx(n) => {
                            assert_eq!(
                                h.emu
                                    .read(CPU + u64::from(runtime::XMM) + 16 * u64::from(n), 16),
                                value.to_le_bytes(),
                                "{source:?}",
                            );
                        }
                        _ => {
                            assert_eq!(read64(&h, base + i as u64 * 8), value as u64, "{source:?}")
                        }
                    }
                }
                let mut restored = CpuState {
                    eflags: cpu.eflags & !runtime::MAPPED_FLAGS,
                    ..CpuState::default()
                };
                restore_inline_entry(&read_entry(&h), &mut restored);
                assert_eq!(restored.regs, cpu.regs);
                assert_eq!(restored.eflags, cpu.eflags);
            } else {
                assert_eq!(reason, MISS, "iteration {iteration}");
                counts[2] += 1;
            }
            if reason != NORMAL {
                assert_state(&h, &cpu);
                assert_eq!(h.emu.read(base, 256), [0xa5; 256]);
                assert_eq!(read_entry(&h).regs, [0; 8]);
            }
            assert_eq!(countdown(&h), 100);
        }
    }
    assert!(counts.into_iter().all(|count| count > 0), "{counts:?}");
}

// spec: A64F-FP-4, A64-OPT-6
#[test]
fn opt_x87_exits_publish_the_alias_read_by_tier1_mmx() {
    let x87 = bytes(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.fst(st1).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
    });
    let mmx = bytes(|a| a.movq2dq(xmm0, mm1).unwrap());
    let successor = CODE + x87.len() as u32;
    let mut cpu = initial();
    cpu.regs[7] = DATA + 32;
    cpu.x87.status = 0x20;
    // Negative infinity has exponent 0xffff, so the Tier-1 MMX alias
    // check can pass even though x87 wrote the slot.
    cpu.x87.regs[0] = X87Register::from_parts(1 << 63, 0xffff);
    cpu.x87.regs[1] = X87Register::from_parts(3 << 62, 0x4000);
    cpu.x87.tag = 0xfff2;
    cpu.refresh_mmx();
    let built = guest_fp_form(&x87, &cpu);
    assert!(built.assumed.x87 && !built.assumed.mmx);
    let params = opt::admit(
        &built.func.entry,
        &built.assumed,
        &cpu,
        |_, _| unreachable!(),
    )
    .unwrap();
    let code = [x87.as_slice(), mmx.as_slice()].concat();
    let expected = reference(&code, &[], &cpu).xmm[0];

    let mut h = test_harness();
    let captured = h.run_opt_inner(&built.func, &params, 100, 0, 0);
    let mut job = cpu.clone();
    materialize(
        &captured.lowered.records[captured.record.0 as usize],
        &captured.registers,
        &captured.spills,
        &captured.lowered.flags,
        &mut job,
        |_, _| unreachable!(),
    );
    assert_eq!(job.eip, successor);
    let (job, exit, _) = h.run(&mmx, &[], &job, false);
    assert_eq!(exit, Exit::Dispatch);

    let lowered = back::lower_inline(&built.func, &built.assumed, &built.func.entry);
    let run = InlineRun::linked(
        &h,
        &lowered,
        &mut memory(&code, &[]),
        successor,
        mmx.len(),
        cpu.x87.control,
    );
    assert_eq!(run.run(&mut h, &cpu, 100), Exit::Dispatch);
    let inline = h.emu.read(CPU + u64::from(runtime::XMM), 16);
    assert_eq!(
        (job.xmm[0], inline.as_slice()),
        (expected, expected.as_slice())
    );
}

// spec: A64-CTL-7, A64-OPT-6
#[test]
fn opt_inline_negative_countdown_takes_the_tier1_budget_exit() {
    let mut h = test_harness();
    let cpu = initial();
    let guest = bytes(|a| a.inc(eax).unwrap());
    let successor = CODE + 0x20;
    let mut memory = memory(&[], &[]);
    memory.protect(CODE, 4096, 3).unwrap();
    memory.write(successor, &guest).unwrap();
    memory.protect(CODE, 4096, 5).unwrap();

    let mut f = Func::default();
    let mut r = record(CODE);
    r.regs = [None; 8];
    r.eflags = None;
    r.x87 = None;
    r.mmx = [None; 8];
    r.xmm = [None; 8];
    f.kept = 0xffffff;
    let entry = f.record(r.clone());
    let block = f.block(Some(Head { record: entry }));
    r.eflags = Some(Location::Constant(u64::from(cpu.eflags)));
    r.eip = successor;
    let exit = f.record(r);
    f.end(block, End::Exit(exit));

    let lowered = back::lower_inline(&f, &assumptions(&cpu), &f.entry);
    let run = InlineRun::linked(
        &h,
        &lowered,
        &mut memory,
        successor,
        guest.len(),
        cpu.x87.control,
    );
    assert_eq!(run.run(&mut h, &cpu, (-3i64) as u64), Exit::Budget);
    assert_eq!(countdown(&h) as i64, -3);
    assert_state(
        &h,
        &CpuState {
            eip: successor,
            ..cpu
        },
    );
}

thread_local! {
    static FIXTURES: RefCell<Option<(Harness, Option<Func>)>> = const { RefCell::new(None) };
}

fn with_fixture(run: impl FnOnce(&mut Harness, &mut Option<Func>)) {
    FIXTURES.with_borrow_mut(|stored| {
        let (h, last) = stored.get_or_insert_with(|| (test_harness(), None));
        run(h, last);
    });
}

pub(super) fn check_backend_snapshots(func: &Func) {
    let cpu = initial();
    let mut assumed = assumptions(&cpu);
    assumed.mmx = func.records.iter().any(|r| r.mmx_meta.is_some());
    check_guest_snapshots(func, &assumed, &cpu);
}

pub(in crate::translate) fn check_guest_snapshots(func: &Func, assumed: &Assumed, cpu: &CpuState) {
    with_fixture(|h, last| {
        if last.as_ref() == Some(func) {
            return;
        }
        *last = Some(func.clone());
        let mut entry = cpu.clone();
        entry.regs[4] = 0x3f000 | u32::from(assumed.esp_low.unwrap_or(0));
        entry.mmx_mode = 0;
        entry.rep = None;
        entry.x87.regs.fill(X87Register::from_parts(1 << 63, 16383));
        entry.x87.control = assumed.control;
        entry.x87.status = assumed.status | u16::from(assumed.top) << 11;
        entry.x87.tag = assumed.tags;
        entry.mxcsr = assumed.mxcsr;
        check_snapshots(h, func, assumed, &entry);
    });
}

pub(in crate::translate) fn compare_guest_exit(
    built: &opt::front::Built,
    at: &CpuState,
    memory: &Memory,
    want: RecordId,
) {
    with_fixture(|h, _| {
        for base in [WINDOW_BASE, ALIAS_BASE] {
            for page in (CODE..0x40000).step_by(4096) {
                if let Ok(bytes) = memory.read_data(page, 4096) {
                    h.emu.write(base + u64::from(page), &bytes);
                }
            }
        }
        let params = opt::admit(&built.func.entry, &built.assumed, at, |addr, width| {
            let mut buf = [0; 8];
            let bytes = memory.read_data(addr, width as usize).unwrap();
            buf[..bytes.len()].copy_from_slice(&bytes);
            u64::from_le_bytes(buf)
        })
        .unwrap();
        let got = compare_exit(h, &built.func, &built.assumed, at, &params, 10000);
        assert_eq!(got, want);
        let esp_page = u64::from(at.regs[4] & !0xfff);
        for page in [esp_page - 4096, esp_page] {
            assert_eq!(
                h.emu.read(ALIAS_BASE + page, 4096),
                h.emu.read(WINDOW_BASE + page, 4096)
            );
        }
    });
}

/// Exercise every emitted exit, including those the guest fixture cannot
/// reach and those whose synthetic parameter cells do not share a CPU context.
fn check_snapshots(h: &mut Harness, func: &Func, assumed: &Assumed, cpu: &CpuState) {
    let mut ids: Vec<_> = func
        .blocks
        .iter()
        .filter_map(|block| {
            if let End::Exit(id) = block.end {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    if ids.is_empty() {
        return;
    }
    ids.sort_by_key(|id| id.0);
    let lowered = back::lower_inline(func, assumed, &func.entry);
    let cold = lowered
        .marks
        .iter()
        .position(|mark| mark.part == Part::Cold)
        .unwrap();
    let exits = &lowered.marks[cold - lowered.links.len()..cold];
    let body = lowered.marks[1].offset as usize;
    assert_eq!(exits.len(), ids.len());

    const SNAPSHOT: u64 = AREA + 0x10000;
    let mut run = InlineRun::new(h, &lowered, &[]);
    let trampoline = run.code.len() * 4;
    let body = run.target + body;
    run.code[body / 4] = b(trampoline as i32 - body as i32);
    run.code.push(movz(X, X16, (SNAPSHOT >> 16) as u16, 16));
    let gprs = [
        6, 7, 8, 10, 11, 12, 13, 14, 15, 19, 20, 21, 22, 23, 24, 25, 26, 27, 30,
    ];
    for n in gprs {
        run.code.push(ldr(X, Reg::new(n), X16, 8 * u32::from(n)));
    }
    let vectors = 256;
    for n in 0..29 {
        run.code.push(vldr(
            VMem::Q,
            VReg::new(n),
            X16,
            vectors + 16 * u32::from(n),
        ));
    }
    let spill_base = vectors + 32 * 16;
    for i in 0..lowered.spill_cells as u32 {
        run.code.extend([
            vldr(VMem::Q, VReg::new(31), X16, spill_base + 16 * i),
            vstr(VMem::Q, VReg::new(31), Reg::new(31), 16 * i),
        ]);
    }
    let jump = run.code.len() * 4;
    run.code.push(0);
    for ((mark, id), &link) in exits.iter().zip(ids).zip(&lowered.links) {
        let record = &lowered.records[id.0 as usize];
        assert_eq!(record.eip, mark.eip);
        let target = run.target + mark.offset as usize;
        run.code[jump / 4] = b(target as i32 - jump as i32);

        let words = [
            0,
            1,
            0x81,
            0xffff,
            0x3ff0_0000_0000_0001,
            0xc008_0000_ffff_ffff,
        ];
        let bits = |i: usize| words[(i + id.0 as usize) % words.len()] ^ i as u64;
        let cell = |i| u128::from(bits(i)) | u128::from(bits(i + 1)) << 64;
        let mut regs = runtime::RegisterDump {
            x: std::array::from_fn(bits),
            v: std::array::from_fn(cell),
            sp: 0,
        };
        let mut spills: Vec<_> = (0..lowered.spill_cells).map(|i| cell(i + 32)).collect();
        if let Some(Location::At(flags) | Location::Recipe(Recipe::Eflags(flags))) = record.eflags {
            // A recipe's base can be the unmodified incoming EFLAGS cell.
            match flag_base(flags, &lowered.flags) {
                Host::Register(r) => regs.x[r.number() as usize] = u64::from(cpu.eflags),
                Host::Vector(r) => regs.v[r.number() as usize] = u128::from(cpu.eflags),
                Host::Spill(n, _) => spills[n as usize] = u128::from(cpu.eflags),
                _ => {}
            }
        }
        let mut image = vec![0; spill_base as usize + 16 * spills.len()];
        for (i, bits) in regs.x.into_iter().enumerate() {
            image[8 * i..8 * i + 8].copy_from_slice(&bits.to_le_bytes());
        }
        for (i, bits) in regs.v.into_iter().chain(spills.iter().copied()).enumerate() {
            let at = vectors as usize + 16 * i;
            image[at..at + 16].copy_from_slice(&bits.to_le_bytes());
        }
        h.emu.write(SNAPSHOT, &image);

        let mut expected = cpu.clone();
        let mut frame = Vec::new();
        materialize(
            record,
            &regs,
            &spills,
            &lowered.flags,
            &mut expected,
            |offset, bytes| {
                frame.push((offset, bytes.to_vec()));
            },
        );
        if assumed.mmx {
            expected.refresh_mmx();
        }
        let charge = u64::from(func.records[func.blocks[0].head.unwrap().record.0 as usize].refund);
        assert_eq!(run.run(h, cpu, charge + 100), NORMAL, "record {id:?}");
        assert_eq!(run.link(h), link);
        assert_eq!(countdown(h), 100 + u64::from(record.refund));
        assert_state(h, &expected);
        for (offset, bytes) in frame {
            let addr = cpu.regs[4].wrapping_add_signed(offset);
            assert_eq!(h.emu.read(ALIAS_BASE + u64::from(addr), bytes.len()), bytes);
        }
    }
}
