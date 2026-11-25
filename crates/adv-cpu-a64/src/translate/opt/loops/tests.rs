use super::*;
use crate::translate::opt::recovery::{IrRecord, Location};

struct Fixture {
    func: Func,
    body: Block,
    pointer: Value,
    bound: Value,
    loaded: Value,
}

fn fixture(cc: Cc) -> Fixture {
    let mut func = Func::default();
    let entry = func.block(None);
    let body = func.block(None);
    let split = func.block(None);
    let exit = func.block(None);
    let initial = func.param(entry, Type::I32);
    let bound = func.param(entry, Type::I32);
    let flags = func.param(entry, Type::Flags);
    func.entry = vec![
        Source::Reg(6),
        Source::Frame {
            offset: 0,
            width: 4,
        },
        Source::Eflags,
    ];
    let pointer = func.param(body, Type::I32);
    let record = func.record(IrRecord {
        eip: 42,
        refund: 4,
        regs: [None; 8],
        eflags: Some(Location::At(flags)),
        flag_groups: Vec::new(),
        x87: None,
        xmm: [None; 8],
        mmx: [None; 8],
        mmx_meta: None,
        frame: Vec::new(),
    });
    func.blocks[split.0 as usize].head = Some(Head { record });
    let load = Op::Load {
        scope: None,
        addr: pointer,
        width: 1,
        ty: Type::I32,
        pc: 42,
        checked: false,
        clock: false,
    };
    let loaded = func.push(body, load).unwrap();
    func.push(
        body,
        Op::Store {
            scope: None,
            addr: pointer,
            width: 1,
            value: loaded,
            pc: 43,
            checked: false,
            after: record,
        },
    );
    let one = func.push(body, Op::Constant(Type::I32, 1)).unwrap();
    let next = func.push(body, Op::Int(IntOp::Add, pointer, one)).unwrap();
    let flags = func
        .push(
            body,
            Op::Flags {
                kind: FlagKind::Sub,
                width: 4,
                a: next,
                b: bound,
                prior: Some(flags),
            },
        )
        .unwrap();
    let cond = func.push(body, Op::Test { flags, cond: cc }).unwrap();
    let edge = |block, args, charge| Target {
        block,
        args,
        charge,
    };
    func.end(entry, End::Jump(edge(body, vec![initial], 0)));
    func.end(split, End::Jump(edge(body, vec![next], 0)));
    func.end(
        body,
        End::Branch {
            cond,
            taken: edge(split, vec![], 4),
            fall: edge(exit, vec![], 0),
        },
    );
    func.end(exit, End::Exit(record));
    Fixture {
        func,
        body,
        pointer,
        bound,
        loaded,
    }
}

fn candidate(func: &Func) -> Result<Loop, Reject> {
    let mut loops = discover(func);
    assert_eq!(loops.len(), 1);
    let (pc, result) = loops.pop().unwrap();
    assert_eq!(pc, 42);
    result
}

struct TestKernel;

impl BatchBody for TestKernel {
    fn lower(&self, _: &mut BatchEmitter<'_, '_>) {}
}

impl LoopKernel for TestKernel {
    fn recognize(&self, _: &Func, _: &Loop) -> Option<Plan> {
        Some(Plan {
            name: "test",
            lanes: 16,
            inputs: vec![],
            conditional: false,
            body: Box::new(Self),
        })
    }
}

// spec: A64-OPT-10
#[test]
fn charged_latch_and_frame_bound() {
    for cond in [
        Cc::e,
        Cc::ne,
        Cc::b,
        Cc::ae,
        Cc::be,
        Cc::a,
        Cc::l,
        Cc::ge,
        Cc::le,
        Cc::g,
    ] {
        let fixture = fixture(cond);
        let found = candidate(&fixture.func).unwrap();
        assert_eq!(found.blocks, [fixture.body]);
        assert_eq!(found.latch, fixture.body);
        assert_eq!(found.carried.len(), 1);
        assert_eq!(found.charge, 4);
        assert_eq!(
            found.trip,
            Trip {
                value: Affine {
                    base: fixture.pointer,
                    offset: 1
                },
                stride: 1,
                bound: Some(fixture.bound),
                again: cond,
                sign: false,
            }
        );
    }
}

// spec: A64-OPT-10
#[test]
fn same_iteration_alias_requires_the_load_before_the_store() {
    let mut fixture = fixture(Cc::ne);
    let batch = plan(&fixture.func, &[&TestKernel]).pop().unwrap();
    assert_eq!(batch.spans.len(), 2);
    assert!(batch.spans.iter().all(|s| s.width == 1 && s.step == 16));
    assert!(batch.alias.is_empty());
    fixture.func.blocks[fixture.body.0 as usize]
        .insts
        .swap(0, 1);
    let batch = plan(&fixture.func, &[&TestKernel]).pop().unwrap();
    assert_eq!(batch.alias, [(0, 1, false)]);
}

#[test]
fn early_exit_rejects_control() {
    let mut f = fixture(Cc::ne);
    let body = &mut f.func.blocks[f.body.0 as usize];
    let end = std::mem::replace(&mut body.end, End::Unset);
    let End::Branch { cond, fall, .. } = &end else {
        unreachable!()
    };
    body.end = End::Branch {
        cond: *cond,
        taken: Target {
            block: Block(4),
            args: vec![],
            charge: 0,
        },
        fall: fall.clone(),
    };
    let latch = f.func.block(None);
    f.func.end(latch, end);
    assert!(matches!(candidate(&f.func), Err(Reject::Control)));
}

#[test]
fn invariant_compare_rejects_induction() {
    let mut f = fixture(Cc::ne);
    let (_, op) = &mut f.func.blocks[f.body.0 as usize].insts[4];
    let Op::Flags { a, .. } = op else {
        unreachable!()
    };
    *a = f.bound;
    assert!(matches!(candidate(&f.func), Err(Reject::Induction)));
}

#[test]
fn unsupported_condition_rejects_latch() {
    assert!(matches!(
        candidate(&fixture(Cc::p).func),
        Err(Reject::Latch)
    ));
}

#[test]
fn dependent_load_rejects_address() {
    let mut f = fixture(Cc::ne);
    let (_, op) = &mut f.func.blocks[f.body.0 as usize].insts[1];
    let Op::Store { addr, .. } = op else {
        unreachable!()
    };
    *addr = f.loaded;
    assert!(matches!(candidate(&f.func), Err(Reject::Address)));
}

#[test]
fn checked_load_rejects_access() {
    let mut f = fixture(Cc::ne);
    let (_, op) = &mut f.func.blocks[f.body.0 as usize].insts[0];
    let Op::Load { checked, .. } = op else {
        unreachable!()
    };
    *checked = true;
    assert!(matches!(candidate(&f.func), Err(Reject::Access)));
}

#[test]
fn unavailable_kernel_declines() {
    assert!(plan(&fixture(Cc::ne).func, &[]).is_empty());
}

// spec: A64-OPT-4, A64-OPT-5, A64-OPT-6
#[test]
fn wave2c_ir_exact_int_reassoc_and_bounded_exhaustive_proof() {
    use crate::translate::opt::front::passes::fp_range;

    // 1) Intra-block exact-integer 4-term sum reassociates to balanced tree,
    // while general float 4-term sum is NEVER reassociated.
    let make_4sum = |exact_ints: bool| {
        let mut f = Func::default();
        let b0 = f.block(None);
        let p = [
            f.param(b0, Type::I32),
            f.param(b0, Type::I32),
            f.param(b0, Type::I32),
            f.param(b0, Type::I32),
        ];
        let fl = [
            f.param(b0, Type::F64),
            f.param(b0, Type::F64),
            f.param(b0, Type::F64),
            f.param(b0, Type::F64),
        ];
        let efl = f.param(b0, Type::Flags);
        f.entry = vec![
            Source::Reg(0),
            Source::Reg(1),
            Source::Reg(2),
            Source::Reg(3),
            Source::X87(0),
            Source::X87(1),
            Source::X87(2),
            Source::X87(3),
            Source::Eflags,
        ];
        let rec = f.record(IrRecord {
            eip: 0,
            refund: 0,
            regs: [None; 8],
            eflags: Some(Location::At(efl)),
            flag_groups: Vec::new(),
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: Vec::new(),
        });
        f.blocks[0].head = Some(Head { record: rec });
        let terms: [Value; 4] = if exact_ints {
            let mut t = [p[0]; 4];
            for i in 0..4 {
                let b = f
                    .push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p[i]))
                    .unwrap();
                t[i] = f.push(b0, Op::FromInt(b)).unwrap();
            }
            t
        } else {
            fl
        };
        let s1 = f
            .push(b0, Op::Fp(FpOp::Add, terms[0], terms[1], false))
            .unwrap();
        let s2 = f.push(b0, Op::Fp(FpOp::Add, s1, terms[2], false)).unwrap();
        let s3 = f.push(b0, Op::Fp(FpOp::Add, s2, terms[3], false)).unwrap();
        let mut exit_rec = f.records[rec.0 as usize].clone();
        exit_rec.x87 = Some(crate::translate::opt::recovery::X87 {
            top: 7,
            valid: 1 << 7,
            control: 0x023f,
            stack: [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Location::At(s3)),
            ],
            status: 0x20,
            c1: None,
            ip: None,
            opcode: None,
        });
        let exit = f.record(exit_rec);
        f.end(b0, End::Exit(exit));
        (f, s1, s2, s3, terms)
    };

    let (mut f_int, s1, s2, s3, terms) = make_4sum(true);
    fp_range::run(&mut f_int);
    let s3_op = f_int.blocks[0]
        .insts
        .iter()
        .find_map(|&(r, op)| (r == Some(s3)).then_some(op))
        .unwrap();
    assert!(
        matches!(s3_op, Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == s1 && rhs == s2),
        "exact-integer (((a+b)+c)+d) should reassociate to (a+b)+(c+d), got {s3_op:?}"
    );
    let s2_op = f_int.blocks[0]
        .insts
        .iter()
        .find_map(|&(r, op)| (r == Some(s2)).then_some(op))
        .unwrap();
    assert!(
        matches!(s2_op, Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == terms[2] && rhs == terms[3]),
        "expected balanced right subtree (c+d), got {s2_op:?}"
    );

    let (mut f_gen, _, s2_g, s3_g, terms_g) = make_4sum(false);
    fp_range::run(&mut f_gen);
    let s3_gen_op = f_gen.blocks[0]
        .insts
        .iter()
        .find_map(|&(r, op)| (r == Some(s3_g)).then_some(op))
        .unwrap();
    assert!(
        matches!(s3_gen_op, Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == s2_g && rhs == terms_g[3]),
        "general float reduction MUST NOT reassociate, got {s3_gen_op:?}"
    );

    // 2) Bounded finite-domain exhaustive proof helper:
    // (a) Correlated small domain (<= 2^16): `x * 0.5 + (128.0 - x * 0.5) == 128.0`
    //     proves safe pow2 div->mul.
    // (b) Oversized 2-leaf domain (2 independent u16 inputs = 65536^2 > 2^16) bails out
    //     and increments `exhaustive_budget_exceeded()`.
    // (c) 3-leaf bounded integer DAG (`4^3 = 64 <= 2^16`) must return `None` cleanly
    //     without panicking.
    // (d) Directed-rounding / PC64 `Op::X87Sqrt` must not be unsafely refined via RN sqrt.
    #[derive(Clone, Copy)]
    enum ExhaustiveCase {
        SmallCorrelated,
        OversizedTwoU16,
        ThreeLeavesNoPanic,
        SqrtDirected,
        SqrtPc64,
    }
    let build_correlated = |case: ExhaustiveCase| {
        let mut f = Func::default();
        let b0 = f.block(None);
        let p0 = f.param(b0, Type::I32);
        let p1 = f.param(b0, Type::I32);
        let p2 = f.param(b0, Type::I32);
        let efl = f.param(b0, Type::Flags);
        f.entry = vec![
            Source::Reg(0),
            Source::Reg(1),
            Source::Reg(2),
            Source::Eflags,
        ];
        let rec = f.record(IrRecord {
            eip: 0,
            refund: 0,
            regs: [None; 8],
            eflags: Some(Location::At(efl)),
            flag_groups: Vec::new(),
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: Vec::new(),
        });
        f.blocks[0].head = Some(Head { record: rec });
        let out = match case {
            ExhaustiveCase::SmallCorrelated => {
                let u0 = f.push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p0)).unwrap();
                let x0 = f.push(b0, Op::FromInt(u0)).unwrap();
                let half = f
                    .push(b0, Op::Constant(Type::F64, u128::from(0.5f64.to_bits())))
                    .unwrap();
                let c128 = f
                    .push(b0, Op::Constant(Type::F64, u128::from(128.0f64.to_bits())))
                    .unwrap();
                let h = f.push(b0, Op::Fp(FpOp::Mul, x0, half, false)).unwrap();
                let d = f.push(b0, Op::Fp(FpOp::Sub, c128, h, false)).unwrap();
                let sum = f.push(b0, Op::Fp(FpOp::Add, h, d, false)).unwrap();
                let two = f
                    .push(b0, Op::Constant(Type::F64, u128::from(2.0f64.to_bits())))
                    .unwrap();
                f.push(b0, Op::Fp(FpOp::Div, sum, two, false)).unwrap()
            }
            ExhaustiveCase::OversizedTwoU16 => {
                let u0 = f
                    .push(b0, Op::IntUnary(IntUnary::ZeroExtend16, p0))
                    .unwrap();
                let u1 = f
                    .push(b0, Op::IntUnary(IntUnary::ZeroExtend16, p1))
                    .unwrap();
                let x0 = f.push(b0, Op::FromInt(u0)).unwrap();
                let x1 = f.push(b0, Op::FromInt(u1)).unwrap();
                let s = f.push(b0, Op::Fp(FpOp::Sub, x0, x1, false)).unwrap();
                let c03 = f
                    .push(b0, Op::Constant(Type::F64, u128::from(0.3f64.to_bits())))
                    .unwrap();
                let v = f.push(b0, Op::Fp(FpOp::Add, s, c03, false)).unwrap();
                let tiny = f
                    .push(
                        b0,
                        Op::Constant(Type::F64, u128::from(f64::MIN_POSITIVE.to_bits())),
                    )
                    .unwrap();
                f.push(b0, Op::Fp(FpOp::Mul, v, tiny, false)).unwrap()
            }
            ExhaustiveCase::ThreeLeavesNoPanic => {
                // 3 bounded leaves (`0..=3` each, `4^3 = 64 <= 2^16`): must bail cleanly without panicking.
                let mask = f.push(b0, Op::Constant(Type::I32, 3)).unwrap();
                let u0 = f.push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p0)).unwrap();
                let u1 = f.push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p1)).unwrap();
                let u2 = f.push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p2)).unwrap();
                let m0 = f.push(b0, Op::Int(IntOp::And, u0, mask)).unwrap();
                let m1 = f.push(b0, Op::Int(IntOp::And, u1, mask)).unwrap();
                let m2 = f.push(b0, Op::Int(IntOp::And, u2, mask)).unwrap();
                let x0 = f.push(b0, Op::FromInt(m0)).unwrap();
                let x1 = f.push(b0, Op::FromInt(m1)).unwrap();
                let x2 = f.push(b0, Op::FromInt(m2)).unwrap();
                let s = f.push(b0, Op::Fp(FpOp::Sub, x0, x1, false)).unwrap();
                let s = f.push(b0, Op::Fp(FpOp::Add, s, x2, false)).unwrap();
                let c03 = f
                    .push(b0, Op::Constant(Type::F64, u128::from(0.3f64.to_bits())))
                    .unwrap();
                let v = f.push(b0, Op::Fp(FpOp::Add, s, c03, false)).unwrap();
                let tiny = f
                    .push(
                        b0,
                        Op::Constant(Type::F64, u128::from(f64::MIN_POSITIVE.to_bits())),
                    )
                    .unwrap();
                f.push(b0, Op::Fp(FpOp::Mul, v, tiny, false)).unwrap()
            }
            ExhaustiveCase::SqrtDirected | ExhaustiveCase::SqrtPc64 => {
                let u0 = f.push(b0, Op::IntUnary(IntUnary::ZeroExtend8, p0)).unwrap();
                let x0 = f.push(b0, Op::FromInt(u0)).unwrap();
                let (pc, rounding) = if matches!(case, ExhaustiveCase::SqrtDirected) {
                    (2, 1)
                } else {
                    (3, 0)
                };
                let sq = f
                    .push(
                        b0,
                        Op::X87Sqrt {
                            value: x0,
                            pc,
                            rounding,
                            proved: false,
                        },
                    )
                    .unwrap();
                let tiny = f
                    .push(
                        b0,
                        Op::Constant(Type::F64, u128::from(f64::MIN_POSITIVE.to_bits())),
                    )
                    .unwrap();
                f.push(b0, Op::Fp(FpOp::Mul, sq, tiny, false)).unwrap()
            }
        };
        let mut exit_rec = f.records[rec.0 as usize].clone();
        exit_rec.x87 = Some(crate::translate::opt::recovery::X87 {
            top: 7,
            valid: 1 << 7,
            control: 0x023f,
            stack: [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Location::At(out)),
            ],
            status: 0x20,
            c1: None,
            ip: None,
            opcode: None,
        });
        let exit = f.record(exit_rec);
        f.end(b0, End::Exit(exit));
        f
    };

    let mut f_small = build_correlated(ExhaustiveCase::SmallCorrelated);
    fp_range::run(&mut f_small);
    assert!(
        !f_small.blocks[0]
            .insts
            .iter()
            .any(|(_, op)| matches!(op, Op::Fp(FpOp::Div, ..))),
        "small correlated 256-value domain should prove safe pow2 div->mul"
    );

    let before_exceeded = fp_range::exhaustive_budget_exceeded();
    let mut f_big = build_correlated(ExhaustiveCase::OversizedTwoU16);
    fp_range::run(&mut f_big);
    assert!(
        fp_range::exhaustive_budget_exceeded() > before_exceeded,
        "65536^2 > 2^16 domain must hit cap and increment exhaustive_budget_exceeded"
    );

    // Regression: 3 bounded integer leaves must not panic in `prove_finite_domain`.
    let mut f_three = build_correlated(ExhaustiveCase::ThreeLeavesNoPanic);
    fp_range::run(&mut f_three);
    assert!(
        f_three.blocks[0]
            .insts
            .iter()
            .any(|(_, op)| matches!(op, Op::Fp(FpOp::Mul, _, _, false))),
        "3-leaf unproved tiny product must remain unproved without panicking"
    );

    // Regression: directed-rounding and PC64 `X87Sqrt` must not be evaluated as RN f64 sqrt.
    for case in [ExhaustiveCase::SqrtDirected, ExhaustiveCase::SqrtPc64] {
        let mut f_sq = build_correlated(case);
        fp_range::run(&mut f_sq);
        assert!(
            f_sq.blocks[0]
                .insts
                .iter()
                .any(|(_, op)| matches!(op, Op::Fp(FpOp::Mul, _, _, false))),
            "directed-rounding/PC64 X87Sqrt tiny product must not be refined via RN sqrt"
        );
    }
}

// spec: A64-MEM-1, A64-OPT-4, A64-OPT-5, A64-OPT-6
#[test]
fn wave2c_ir_slp_aos_ld2_ld1r_and_compact_window() {
    use crate::translate::opt::ir::{SINGLE_A, SINGLE_B, SINGLE_EXACT, SINGLE_RESULT};

    let build_slp_aos = |load_proved: bool, stride_bytes: u32| {
        let mut f = Func {
            resident: true,
            kept: 0x00ff_ffff & !((1 << 3) | (1 << 6) | (1 << 7) | (1 << 24)),
            entry: vec![
                Source::Reg(6),
                Source::Reg(7),
                Source::Reg(3),
                Source::Eflags,
            ],
            ..Func::default()
        };
        let b0 = f.block(None);
        let p_esi = f.param(b0, Type::I32);
        let p_edi = f.param(b0, Type::I32);
        let p_ebx = f.param(b0, Type::I32);
        let efl = f.param(b0, Type::Flags);
        let mut regs = [None; 8];
        regs[6] = Some(Location::At(p_esi));
        regs[7] = Some(Location::At(p_edi));
        regs[3] = Some(Location::At(p_ebx));
        let base_rec = IrRecord {
            eip: 0x10000,
            refund: 0,
            regs,
            eflags: Some(Location::At(efl)),
            flag_groups: Vec::new(),
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: Vec::new(),
        };
        let rec = f.record(base_rec.clone());
        f.blocks[0].head = Some(Head { record: rec });
        let b_raw = f
            .push(
                b0,
                Op::Load {
                    ty: Type::I32,
                    width: 4,
                    addr: p_ebx,
                    checked: false,
                    pc: 0x10000,
                    scope: None,
                    clock: false,
                },
            )
            .unwrap();
        let b_f = f
            .push(
                b0,
                Op::X87Load {
                    bits: b_raw,
                    denormal: false,
                    single: true,
                    proved: load_proved,
                },
            )
            .unwrap();
        let mut pairs = Vec::new();
        for k in 0..4u32 {
            let off0 = f
                .push(b0, Op::Constant(Type::I32, u128::from(k * stride_bytes)))
                .unwrap();
            let a0 = f.push(b0, Op::Int(IntOp::Add, p_esi, off0)).unwrap();
            let r0 = f
                .push(
                    b0,
                    Op::Load {
                        ty: Type::I32,
                        width: 4,
                        addr: a0,
                        checked: false,
                        pc: 0x10004 + k * 8,
                        scope: None,
                        clock: false,
                    },
                )
                .unwrap();
            let x0 = f
                .push(
                    b0,
                    Op::X87Load {
                        bits: r0,
                        denormal: false,
                        single: true,
                        proved: load_proved,
                    },
                )
                .unwrap();
            let off1 = f
                .push(
                    b0,
                    Op::Constant(Type::I32, u128::from(k * stride_bytes + 4)),
                )
                .unwrap();
            let a1 = f.push(b0, Op::Int(IntOp::Add, p_esi, off1)).unwrap();
            let r1 = f
                .push(
                    b0,
                    Op::Load {
                        ty: Type::I32,
                        width: 4,
                        addr: a1,
                        checked: false,
                        pc: 0x10008 + k * 8,
                        scope: None,
                        clock: false,
                    },
                )
                .unwrap();
            let x1 = f
                .push(
                    b0,
                    Op::X87Load {
                        bits: r1,
                        denormal: false,
                        single: true,
                        proved: load_proved,
                    },
                )
                .unwrap();
            pairs.push((x0, x1));
        }
        let mut outs = Vec::new();
        for (x0, x1) in pairs {
            let prod = f
                .push(
                    b0,
                    Op::X87Single(
                        FpOp::Mul,
                        x0,
                        x1,
                        SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT,
                    ),
                )
                .unwrap();
            let sum = f
                .push(
                    b0,
                    Op::X87Single(
                        FpOp::Add,
                        prod,
                        b_f,
                        SINGLE_A | SINGLE_B | SINGLE_RESULT | SINGLE_EXACT,
                    ),
                )
                .unwrap();
            let st = f
                .push(
                    b0,
                    Op::X87Store {
                        value: sum,
                        width: 4,
                        rounding: 0,
                        integer: false,
                        proved: true,
                    },
                )
                .unwrap();
            outs.push(st);
        }
        for (k, st) in outs.into_iter().enumerate() {
            let off = f
                .push(b0, Op::Constant(Type::I32, u128::from(k as u32 * 4)))
                .unwrap();
            let dst = f.push(b0, Op::Int(IntOp::Add, p_edi, off)).unwrap();
            let after = f.record(base_rec.clone());
            f.push(
                b0,
                Op::Store {
                    width: 4,
                    addr: dst,
                    value: st,
                    checked: false,
                    pc: 0x10040 + k as u32 * 4,
                    scope: None,
                    after,
                },
            );
        }
        let exit = f.record(base_rec);
        f.end(b0, End::Exit(exit));
        assert_eq!(crate::translate::opt::verify::verify(&f), Ok(()));
        crate::translate::opt::front::passes::slp::run(&mut f);
        assert_eq!(crate::translate::opt::verify::verify(&f), Ok(()));
        let lowered = crate::translate::opt::back::lower(&f);
        (f, lowered)
    };

    if !adv_cpu_core::virtual_memory::GuestWindow::compact() {
        let (func_slp, lowered_slp) = build_slp_aos(true, 8);
        assert!(
            func_slp.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|(_, op)| matches!(op, Op::VecDeintLoad { channels: 2, .. }))
            }),
            "non-compact SLP must emit Op::VecDeintLoad for 2-channel AoS loads"
        );
        assert!(
            func_slp.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|(_, op)| matches!(op, Op::VecLoadBcast { .. }))
            }),
            "non-compact SLP must emit Op::VecLoadBcast for broadcast-from-memory"
        );
        assert!(
            lowered_slp
                .words
                .iter()
                .any(|&w| (w & 0xbfff_f000) == 0x0c40_8000),
            "expected LD2 emitted for SLP VecDeintLoad"
        );
        assert!(
            lowered_slp
                .words
                .iter()
                .any(|&w| (w & 0xbfff_f000) == 0x0d40_c000),
            "expected LD1R emitted for SLP VecLoadBcast"
        );

        // Regression: `X87Load { proved: false, denormal: false }` still checks for NaN/SNaN
        // and MUST NOT be replaced by raw `VecDeintLoad` / `VecLoadBcast`.
        let (func_unproved, _) = build_slp_aos(false, 8);
        assert!(
            !func_unproved.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|(_, op)| matches!(op, Op::VecDeintLoad { .. } | Op::VecLoadBcast { .. }))
            }),
            "unproved X87Load (even with denormal: false) must preserve NaN checks and not lower to VecDeintLoad/VecLoadBcast"
        );

        // Regression: partial 3-channel AoS (only 2 of 3 channels read at stride 12) MUST NOT
        // emit `VecDeintLoad { channels: 3 }` which would over-read unreferenced adjacent bytes.
        let (func_partial_ch3, _) = build_slp_aos(true, 12);
        assert!(
            !func_partial_ch3.blocks.iter().any(|b| {
                b.insts
                    .iter()
                    .any(|(_, op)| matches!(op, Op::VecDeintLoad { .. }))
            }),
            "partial 3-channel AoS must not emit over-reading LD3 VecDeintLoad"
        );
    }

    let (func_slp_compact, _) =
        crate::translate::opt::back::with_compact_window(true, || build_slp_aos(true, 8));
    assert!(
        !func_slp_compact.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|(_, op)| matches!(op, Op::VecDeintLoad { .. }))
        }),
        "compact window MUST NOT emit multi-element SLP VecDeintLoad"
    );

    // Regression: `Op::PageCross` preceding `v = Int(Add, base, disp)` -> `Store` MUST NOT
    // mark `v` as a skipped `folded_addr_inst` (the Store falls back to `gp(v)` when
    // `page_cross.is_some()`).
    let mut f_pc = Func {
        resident: true,
        kept: 0x00ff_ffff & !((1 << 0) | (1 << 7) | (1 << 24)),
        entry: vec![Source::Reg(0), Source::Reg(7), Source::Eflags],
        ..Func::default()
    };
    let b0 = f_pc.block(None);
    let val = f_pc.param(b0, Type::I32);
    let base = f_pc.param(b0, Type::I32);
    let efl = f_pc.param(b0, Type::Flags);
    let mut regs = [None; 8];
    regs[0] = Some(Location::At(val));
    regs[7] = Some(Location::At(base));
    let rec0 = IrRecord {
        eip: 0x10000,
        refund: 0,
        regs,
        eflags: Some(Location::At(efl)),
        flag_groups: Vec::new(),
        x87: None,
        xmm: [None; 8],
        mmx: [None; 8],
        mmx_meta: None,
        frame: Vec::new(),
    };
    let hrec = f_pc.record(rec0.clone());
    let after = f_pc.record(rec0.clone());
    let exit = f_pc.record(rec0);
    f_pc.blocks[0].head = Some(Head { record: hrec });
    let disp16 = f_pc.push(b0, Op::Constant(Type::I32, 16)).unwrap();
    let addr = f_pc.push(b0, Op::Int(IntOp::Add, base, disp16)).unwrap();
    f_pc.push(
        b0,
        Op::PageCross {
            addr,
            width: 4,
            record: hrec,
        },
    );
    f_pc.push(
        b0,
        Op::Store {
            width: 4,
            addr,
            value: val,
            checked: false,
            pc: 0x10004,
            scope: None,
            after,
        },
    );
    f_pc.end(b0, End::Exit(exit));
    assert_eq!(crate::translate::opt::verify::verify(&f_pc), Ok(()));
    let lowered_pc = crate::translate::opt::back::lower_resident(
        &f_pc,
        &crate::facts::Assumed {
            control: 0x023f,
            status: 0x20,
            mxcsr: 0x1fa0,
            top: 0,
            tags: 0xffff,
            esp_low: None,
            ebp: None,
            denormal: false,
            x87: false,
            mmx: false,
            mxcsr_mask: 0,
        },
        &f_pc.entry,
        false,
    );
    // `add wd, wn, #16` (32-bit immediate add: op=0x1100_0000, imm12=16 -> bits [21:10] == 16).
    let is_add_w_imm16 = |w: u32| (w & 0xffc0_0000) == 0x1100_0000 && ((w >> 10) & 0xfff) == 16;
    assert!(
        lowered_pc.words.iter().any(|&w| is_add_w_imm16(w)),
        "address Add after Op::PageCross must be materialized, not folded/skipped"
    );
}

// spec: A64-OPT-4, A64-OPT-5, A64-OPT-6
#[test]
fn exact_int_reassoc_rejects_boundary_rounding_overflow_and_accepts_exact_fit() {
    use crate::translate::opt::eval::{self, Outcome};
    use crate::translate::opt::front::passes::fp_range;

    let build_chain = |a_val: f64, is_single: bool| {
        let mut f = Func::default();
        let b0 = f.block(None);
        let p = [
            f.param(b0, Type::I32),
            f.param(b0, Type::I32),
            f.param(b0, Type::I32),
        ];
        let efl = f.param(b0, Type::Flags);
        f.entry = vec![
            Source::Reg(0),
            Source::Reg(1),
            Source::Reg(2),
            Source::Eflags,
        ];
        let rec = f.record(IrRecord {
            eip: 0,
            refund: 0,
            regs: [None; 8],
            eflags: Some(Location::At(efl)),
            flag_groups: Vec::new(),
            x87: None,
            xmm: [None; 8],
            mmx: [None; 8],
            mmx_meta: None,
            frame: Vec::new(),
        });
        f.blocks[0].head = Some(Head { record: rec });
        let a = f
            .push(b0, Op::Constant(Type::F64, u128::from(a_val.to_bits())))
            .unwrap();
        let one = f.push(b0, Op::Constant(Type::I32, 1)).unwrap();
        let mut bcd = [a; 3];
        for i in 0..3 {
            let bit = f.push(b0, Op::Int(IntOp::And, p[i], one)).unwrap();
            bcd[i] = f.push(b0, Op::FromInt(bit)).unwrap();
        }
        let mk_add = |lhs, rhs| {
            if is_single {
                Op::X87Single(FpOp::Add, lhs, rhs, 0)
            } else {
                Op::Fp(FpOp::Add, lhs, rhs, false)
            }
        };
        let s1 = f.push(b0, mk_add(a, bcd[0])).unwrap();
        let s2 = f.push(b0, mk_add(s1, bcd[1])).unwrap();
        let s3 = f.push(b0, mk_add(s2, bcd[2])).unwrap();
        let mut exit_rec = f.records[rec.0 as usize].clone();
        exit_rec.x87 = Some(crate::translate::opt::recovery::X87 {
            top: 7,
            valid: 1 << 7,
            control: if is_single { 0x003f } else { 0x023f },
            stack: [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(Location::At(s3)),
            ],
            status: 0x20,
            c1: None,
            ip: None,
            opcode: None,
        });
        let exit = f.record(exit_rec);
        f.end(b0, End::Exit(exit));
        (f, s1, s2, s3, bcd)
    };

    let op_of = |f: &Func, v: Value| {
        f.blocks[0]
            .insts
            .iter()
            .find_map(|&(r, op)| (r == Some(v)).then_some(op))
            .unwrap()
    };

    // 1) Binary64: a = 2^53, b/c/d in {0, 1}. Total max magnitude is 2^53 + 3 > 2^53.
    // Reassociation MUST be rejected because ((2^53 + 1) + 1) + 1 == 2^53 while
    // (2^53 + 1) + (1 + 1) == 2^53 + 2.
    let two_53 = (1u64 << 53) as f64;
    let (mut f_rej, _, s2_rej, s3_rej, bcd_rej) = build_chain(two_53, false);
    fp_range::run(&mut f_rej);
    assert!(
        matches!(
            op_of(&f_rej, s3_rej),
            Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == s2_rej && rhs == bcd_rej[2]
        ),
        "a = 2^53 with b/c/d in {{0,1}} MUST NOT reassociate, got {:?}",
        op_of(&f_rej, s3_rej)
    );
    let mut mem = adv_cpu_core::Memory::new(0, 0x1000).unwrap();
    let run_rej = eval::run(&f_rej, &[1, 1, 1, 0], &mut mem, 64);
    assert!(matches!(run_rej.outcome, Outcome::Exit(_)));
    let res = f64::from_bits(run_rej.values[s3_rej.0 as usize] as u64);
    assert_eq!(
        res.to_bits(),
        two_53.to_bits(),
        "((2^53 + 1) + 1) + 1 must remain 2^53, got {res}"
    );

    // 2) Binary64 exact boundary: a = 2^53 - 3, b/c/d in {0, 1}.
    // Total max magnitude is exactly 2^53 <= 2^53, so reassociation IS accepted.
    let (mut f_ok, s1_ok, s2_ok, s3_ok, bcd_ok) = build_chain(((1u64 << 53) - 3) as f64, false);
    fp_range::run(&mut f_ok);
    assert!(
        matches!(
            op_of(&f_ok, s3_ok),
            Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == s1_ok && rhs == s2_ok
        ),
        "a = 2^53 - 3 with b/c/d in {{0,1}} should reassociate, got {:?}",
        op_of(&f_ok, s3_ok)
    );
    assert!(
        matches!(
            op_of(&f_ok, s2_ok),
            Op::Fp(FpOp::Add, lhs, rhs, _) if lhs == bcd_ok[1] && rhs == bcd_ok[2]
        ),
        "expected balanced (c + d), got {:?}",
        op_of(&f_ok, s2_ok)
    );
    let run_ok = eval::run(&f_ok, &[1, 1, 1, 0], &mut mem, 64);
    assert!(matches!(run_ok.outcome, Outcome::Exit(_)));
    let res_ok = f64::from_bits(run_ok.values[s3_ok.0 as usize] as u64);
    assert_eq!(res_ok.to_bits(), two_53.to_bits());

    // 3) Binary32 (Op::X87Single): a = 2^24 rejected, a = 2^24 - 3 accepted.
    let two_24 = (1u64 << 24) as f64;
    let (mut f32_rej, _, s2_32r, s3_32r, bcd_32r) = build_chain(two_24, true);
    fp_range::run(&mut f32_rej);
    assert!(
        matches!(
            op_of(&f32_rej, s3_32r),
            Op::X87Single(FpOp::Add, lhs, rhs, _) if lhs == s2_32r && rhs == bcd_32r[2]
        ),
        "a = 2^24 with b/c/d in {{0,1}} MUST NOT reassociate, got {:?}",
        op_of(&f32_rej, s3_32r)
    );
    let (mut f32_ok, s1_32o, s2_32o, s3_32o, _) = build_chain(((1u64 << 24) - 3) as f64, true);
    fp_range::run(&mut f32_ok);
    assert!(
        matches!(
            op_of(&f32_ok, s3_32o),
            Op::X87Single(FpOp::Add, lhs, rhs, _) if lhs == s1_32o && rhs == s2_32o
        ),
        "a = 2^24 - 3 with b/c/d in {{0,1}} should reassociate, got {:?}",
        op_of(&f32_ok, s3_32o)
    );
}
