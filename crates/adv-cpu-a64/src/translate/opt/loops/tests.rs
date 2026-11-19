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
    fn lower(&self, _: &mut BatchEmitter<'_, '_>) -> Option<ClassCosts> {
        None
    }
}

impl LoopKernel for TestKernel {
    fn recognize(&self, _: &Func, _: &Loop) -> Option<Plan> {
        Some(Plan {
            name: "test",
            lanes: 16,
            inputs: vec![],
            conditional_stores: false,
            published: Vec::new(),
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
