mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const STACK: u32 = 0x12f00;
const TABLE: u32 = 0x20000;
const OUT: u32 = 0x30000;
const END: u32 = 0x40000;
const EDGES: [u32; 10] = [
    0,
    1,
    2,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xffff_fffe,
    0xffff_ffff,
    0x4000_0000,
    0x0000_80ff,
];

/// For every operand pair of the table, applies `op` and records each
/// condition, both as SETcc straight after it and through JLE/JL branches.
fn program(op: impl Fn(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, dword_ptr(esi)).unwrap();
    a.mov(ebx, dword_ptr(esi + 4)).unwrap();
    // A data-dependent incoming CF for ADC/SBB.
    a.mov(edx, eax).unwrap();
    a.shr(edx, 4).unwrap();
    op(&mut a);
    a.setl(byte_ptr(edi)).unwrap();
    a.setge(byte_ptr(edi + 1)).unwrap();
    a.setle(byte_ptr(edi + 2)).unwrap();
    a.setg(byte_ptr(edi + 3)).unwrap();
    a.setbe(byte_ptr(edi + 4)).unwrap();
    a.seta(byte_ptr(edi + 5)).unwrap();
    a.setb(byte_ptr(edi + 6)).unwrap();
    a.sete(byte_ptr(edi + 7)).unwrap();
    a.sets(byte_ptr(edi + 8)).unwrap();
    a.seto(byte_ptr(edi + 9)).unwrap();
    a.mov(dword_ptr(edi + 12), eax).unwrap();
    let mut skip = a.create_label();
    a.jle(skip).unwrap();
    a.mov(byte_ptr(edi + 10), 1).unwrap();
    a.set_label(&mut skip).unwrap();
    let mut less = a.create_label();
    let mut done = a.create_label();
    a.jl(less).unwrap();
    a.mov(byte_ptr(edi + 11), 1).unwrap();
    a.jmp(done).unwrap();
    a.set_label(&mut less).unwrap();
    a.mov(byte_ptr(edi + 11), 2).unwrap();
    a.set_label(&mut done).unwrap();
    a.add(esi, 8).unwrap();
    a.add(edi, 16).unwrap();
    a.dec(ecx).unwrap();
    a.jne(top).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, (END - CODE) as usize).unwrap();
    m.protect(CODE, (END - CODE) as usize, READ | WRITE)
        .unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    let mut table = Vec::new();
    for a in EDGES {
        for b in EDGES {
            table.extend(a.to_le_bytes());
            table.extend(b.to_le_bytes());
        }
    }
    m.write(TABLE, &table).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.state.eip = CODE;
    m.state.regs = [
        0,
        (EDGES.len() * EDGES.len()) as u32,
        0,
        0,
        STACK,
        0,
        TABLE,
        OUT,
    ];
    m.state.eflags = 0x202;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}

#[test]
fn order_conditions_after_arithmetic_and_logic_match_unicorn() {
    type Op = fn(&mut CodeAssembler);
    let ops: [(&str, Op); 17] = [
        ("cmp", |a| a.cmp(eax, ebx).unwrap()),
        ("sub", |a| a.sub(eax, ebx).unwrap()),
        ("add", |a| a.add(eax, ebx).unwrap()),
        ("inc", |a| a.inc(eax).unwrap()),
        ("dec", |a| a.dec(eax).unwrap()),
        ("test", |a| a.test(eax, ebx).unwrap()),
        ("and", |a| a.and(eax, ebx).unwrap()),
        ("or", |a| a.or(eax, ebx).unwrap()),
        ("xor", |a| a.xor(eax, ebx).unwrap()),
        ("adc", |a| a.adc(eax, ebx).unwrap()),
        ("sbb", |a| a.sbb(eax, ebx).unwrap()),
        ("neg", |a| a.neg(eax).unwrap()),
        ("cmp8", |a| a.cmp(al, bl).unwrap()),
        ("add8", |a| a.add(al, bl).unwrap()),
        ("sub16", |a| a.sub(ax, bx).unwrap()),
        ("and8", |a| a.and(al, bl).unwrap()),
        ("cmp-imm", |a| a.cmp(eax, 0x7fff_ffff).unwrap()),
    ];
    let len = EDGES.len() * EDGES.len() * 16;
    for (name, op) in ops {
        let bytes = program(op);
        let expected = {
            let mut uc = machine(&bytes, None);
            uc.run_unicorn(STOP, 10_000_000).unwrap();
            uc.memory().read(OUT, len).unwrap().to_vec()
        };
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            jit.run(STOP, 1 << 30).unwrap();
            let actual = jit.memory().read(OUT, len).unwrap().to_vec();
            for (index, (left, right)) in actual.chunks(16).zip(expected.chunks(16)).enumerate() {
                let (a, b) = (EDGES[index / EDGES.len()], EDGES[index % EDGES.len()]);
                assert_eq!(left, right, "{name} {strategy:?} a={a:#x} b={b:#x}");
            }
        }
    }
}
