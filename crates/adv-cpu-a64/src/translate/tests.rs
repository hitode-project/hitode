//! Execute whole generated units, including entry/exit flag mapping, on any host.
use super::*;
use crate::emulator::{CODE as HOST_CODE, Emulator};
use crate::runtime;
use adv_cpu_core::{CpuState, Memory, interpreter::Interpreter};
use iced_x86::code_asm::*;
use std::mem::{offset_of, size_of};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const WINDOW_BASE: u64 = 7 << 32;
const CONTEXT: u64 = 0x20000000;
const CPU: u64 = CONTEXT + 0x1000;
const HOST_STACK: u64 = 0x30010000;

fn bytes(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn memory(code: &[u8], data: &[u8]) -> Memory {
    let mut memory = Memory::new(CODE, 0x30000).unwrap();
    memory.protect(CODE, 0x30000, 3).unwrap();
    memory.write(CODE, code).unwrap();
    memory.write(DATA, data).unwrap();
    memory.protect(CODE, 4096, 5).unwrap();
    memory
}

fn initial() -> CpuState {
    CpuState {
        regs: [
            0x81234567, 1, 0x12345678, DATA, 0x3f000, 0x20000, DATA, 0x76543210,
        ],
        eip: CODE,
        eflags: 0xed7,
        ..Default::default()
    }
}

// spec: A64F-FP-7
#[test]
fn full_x87_units_keep_short_branches_to_cold_code_in_reach() {
    // A whole trace of x87 divisions gives every instruction a long generic
    // path; the parity edge and the backedge refusal still use tbz.
    for parity in [false, true] {
        let code = bytes(|a| {
            let mut top = a.create_label();
            a.fld1().unwrap();
            a.fld1().unwrap();
            a.set_label(&mut top).unwrap();
            for _ in 0..90 {
                a.fdiv_2(st0, st1).unwrap();
            }
            a.dec(ecx).unwrap();
            if parity {
                a.jp(top).unwrap();
            } else {
                a.jne(top).unwrap();
            }
        });
        let mut mem = memory(&code, &[]);
        let mut work = Workspace::default();
        let host = HOST_CODE as usize;
        let stubs = Stubs {
            leaf: host,
            exit: host,
            dispatch: host,
            lookup: host,
            lookup_slow: host,
            ret_miss: host,
            access: host,
            interpret: host,
            fault: host,
        };
        let features = Features { flagm: false };
        let leaf = |_, _| false;
        translate(
            &mut work,
            &mut mem,
            CODE + 4,
            &[],
            &|_| false,
            0,
            features,
            host,
            stubs,
            &leaf,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
        );
        assert!(
            work.words().len() * 4 > 1 << 15,
            "the unit outgrows tbz reach"
        );
    }
}

// spec: A64-CTL-6
#[test]
fn bytecode_vm_dispatch_instruction_gate() {
    // Authored VM: eight byte opcodes and eight handlers, each returning to
    // the fetch/dispatch pair. Compare the existing emitter with a resolved
    // site-table prototype; this deliberately adds no production mechanism.
    let guest_table = DATA + 0x100;
    let code = bytes(|a| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.jmp(dword_ptr(eax * 4 + guest_table as i32)).unwrap();
        for _ in 0..8 {
            a.inc(edx).unwrap();
            a.jmp(CODE as u64).unwrap();
        }
    });
    let mut mem = memory(&code, &[]);
    let mut work = Workspace::default();
    let stubs = Stubs {
        leaf: HOST_CODE as usize,
        exit: HOST_CODE as usize,
        dispatch: HOST_CODE as usize,
        lookup: HOST_CODE as usize,
        lookup_slow: HOST_CODE as usize,
        ret_miss: HOST_CODE as usize,
        access: HOST_CODE as usize,
        interpret: HOST_CODE as usize,
        fault: HOST_CODE as usize,
    };
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[],
        &|_| false,
        0,
        Features { flagm: false },
        HOST_CODE as usize,
        stubs,
        &|_, _| false,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
    );
    let branch = work.words().iter().position(|&w| w == br(X17)).unwrap();
    let target_load = work.sites().last().unwrap().0 / 4;
    let before = branch + 1 - target_load;
    assert_eq!(before, 9, "guest target load through the hit-path br");
    // LDR (register), option 011: [x17, x19, lsl #3]. The existing
    // encoder's option 010 is UXTW; bit 13 selects the X-register form.
    let resolved = ldr_reg(X, X17, X17, host(0), true) | (1 << 13);
    let baseline = work.words()[2..=branch].to_vec();
    work.unit.images.push((CODE, 0x30000));
    mem.protect(DATA, 4096, 1).unwrap();
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[],
        &|_| false,
        0,
        Features { flagm: false },
        HOST_CODE as usize,
        stubs,
        &|_, _| false,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
    );
    assert_eq!(work.unit.tables.len(), 1);
    let hot = work.words().iter().position(|&w| w == br(X17)).unwrap();
    assert_eq!(work.words()[hot - 1], resolved);
    let after = 3;
    assert!(before - after >= 4);
    let mut results = Vec::new();
    for direct in [false, true] {
        let mut asm = crate::asm::Asm::new();
        let top = asm.label();
        let table = asm.label();
        let next = asm.label();
        let handlers: Vec<_> = (0..8).map(|_| asm.label()).collect();
        asm.bind(top);
        if direct {
            asm.emit(ldr_narrow(Narrow::Byte, host(0), WINDOW, host(6)));
            asm.adr(X17, table);
            asm.emit_all(&[resolved, br(X17)]);
        } else {
            // Skip only the unit's two-instruction budget head. The rest
            // is the real emitter, including opcode fetch and guest load.
            asm.emit_all(&baseline);
        }
        for (opcode, &handler) in handlers.iter().enumerate() {
            asm.bind(handler);
            asm.emit(add_imm(W, X0, X0, opcode as u32 + 1, false));
            asm.b(next);
        }
        asm.bind(next);
        asm.emit_all(&[
            add_imm(W, host(6), host(6), 1, false),
            subs_imm(W, X9, X9, 1, false),
        ]);
        asm.b_cond(Cond::Ne, top);
        asm.emit(ret(Reg::LR));
        if !asm.offset().is_multiple_of(8) {
            asm.emit(NOP);
        }
        asm.bind(table);
        for &handler in &handlers {
            let address = HOST_CODE + asm.position(handler) as u64;
            asm.emit_all(&[address as u32, (address >> 32) as u32]);
        }
        let mut emu = Emulator::new();
        emu.map(WINDOW_BASE + u64::from(DATA), 0x1000);
        emu.map(CONTEXT, 0x1000);
        let indirect = CONTEXT + 0x10000;
        emu.map(indirect, (runtime::TABLE_LEN * 16) as u64);
        emu.write(CONTEXT + u64::from(runtime::TABLE), &indirect.to_le_bytes());
        emu.write(WINDOW_BASE + u64::from(DATA), &[0, 1, 2, 3, 4, 5, 6, 7]);
        for (opcode, &handler) in handlers.iter().enumerate() {
            let target = CODE + 0x100 + opcode as u32 * 16;
            emu.write(
                WINDOW_BASE + u64::from(guest_table) + opcode as u64 * 4,
                &target.to_le_bytes(),
            );
            let entry = indirect + (runtime::table_slot(target) * 16) as u64;
            emu.write(entry, &u64::from(target).to_le_bytes());
            emu.write(
                entry + 8,
                &(HOST_CODE + asm.position(handler) as u64).to_le_bytes(),
            );
        }
        emu.set_x(WINDOW, WINDOW_BASE);
        emu.set_x(RUNTIME, CONTEXT);
        emu.set_x(host(6), u64::from(DATA));
        emu.set_x(X9, 8);
        emu.set_x(X0, 0);
        emu.call(&asm.finish()).unwrap();
        results.push((emu.x(X0), emu.x(host(6))));
    }
    assert_eq!(results, vec![(36, u64::from(DATA + 8)); 2]);
    eprintln!("VM dispatch gate: {before} -> {after} A64 instructions; dependent loads 2 -> 1");
    eprintln!(
        "VM complete fetch-to-br: {} -> {} A64 instructions",
        baseline.len(),
        after + 1
    );
}

fn reference(code: &[u8], data: &[u8], initial: &CpuState) -> CpuState {
    let mut memory = memory(code, data);
    let mut state = initial.clone();
    let mut interpreter = Interpreter::new();
    while state.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            adv_cpu_core::interpreter::Step::Executed
        );
    }
    state
}

struct Harness {
    emu: Emulator,
    loaded: Vec<u32>,
    stubs: runtime::Stubs,
    prefix: Vec<u32>,
    dispatch: usize,
    forbidden: usize,
}

impl Harness {
    fn new() -> Self {
        let stubs = runtime::stubs();
        let mut prefix = stubs.code.clone();
        // Dispatch returns to this test instead of invoking a Rust owner. An
        // unexpected helper must trap, so a semantic match cannot hide fallback.
        let dispatch = prefix.len() * 4;
        prefix.push(movz(W, X0, Exit::Dispatch as u16, 0));
        prefix.push(b(stubs.exit as i32 - (dispatch as i32 + 4)));
        let forbidden = prefix.len() * 4;
        prefix.push(brk(0x101));
        let mut emu = Emulator::new();
        emu.map(CONTEXT, 0x4000);
        emu.map(HOST_STACK - 0x10000, 0x10000);
        emu.map(WINDOW_BASE + u64::from(DATA), 0x10000);
        Self {
            emu,
            loaded: Vec::new(),
            stubs,
            prefix,
            dispatch,
            forbidden,
        }
    }

    fn run(
        &mut self,
        code: &[u8],
        data: &[u8],
        initial: &CpuState,
        flagm: bool,
    ) -> (CpuState, Exit, u64) {
        let mut memory = memory(code, data);
        let at = self.prefix.len() * 4;
        let end = CODE + code.len() as u32;
        let mut work = Workspace::default();
        let addr = |offset| HOST_CODE as usize + offset;
        translate(
            &mut work,
            &mut memory,
            CODE,
            &[end],
            &|_| false,
            0,
            Features { flagm },
            addr(at),
            Stubs {
                leaf: addr(self.forbidden),
                exit: addr(self.stubs.exit),
                dispatch: addr(self.dispatch),
                lookup: addr(self.forbidden),
                lookup_slow: addr(self.forbidden),
                ret_miss: addr(self.forbidden),
                access: addr(self.forbidden),
                interpret: addr(self.forbidden),
                fault: addr(self.forbidden),
            },
            &|_, _| false,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
        );
        assert!(
            !work
                .unit
                .trace
                .insts
                .iter()
                .any(|i| matches!(i.op, decode::Op::Helper(_))),
            "form has no native plan"
        );
        let mut host_code = self.prefix.clone();
        host_code.extend_from_slice(work.words());
        let mut context = vec![0; size_of::<runtime::Runtime>()];
        let mut put = |offset: usize, value: u64| {
            context[offset..offset + 8].copy_from_slice(&value.to_le_bytes())
        };
        put(offset_of!(runtime::Runtime, cpu), CPU);
        put(offset_of!(runtime::Runtime, window), WINDOW_BASE);
        put(offset_of!(runtime::Runtime, countdown), 100);
        self.emu.write(CONTEXT, &context);
        let mut state = vec![0; size_of::<CpuState>()];
        for (n, value) in initial.regs.iter().enumerate() {
            let at = offset_of!(CpuState, regs) + n * 4;
            state[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [
            (offset_of!(CpuState, eip), initial.eip),
            (offset_of!(CpuState, eflags), initial.eflags),
            (offset_of!(CpuState, mxcsr), initial.mxcsr),
            (offset_of!(CpuState, fs_base), initial.fs_base),
        ] {
            state[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let at_xmm = offset_of!(CpuState, xmm);
        state[at_xmm..at_xmm + 128].copy_from_slice(initial.xmm.as_flattened());
        self.emu.write(CPU, &state);
        self.emu.write(WINDOW_BASE + u64::from(DATA), data);
        self.emu.set_sp(HOST_STACK);
        self.emu.set_x(X0, CONTEXT);
        self.emu.set_x(X1, HOST_CODE + at as u64);
        // Registers/data vary much more often than generated code. Reuse only
        // byte-identical units; replacements still flush Unicorn's old blocks.
        if self.loaded == host_code {
            self.emu.call_loaded_at(self.stubs.enter)
        } else {
            let result = self.emu.call_at(&host_code, self.stubs.enter);
            self.loaded = host_code;
            result
        }
        .expect("native unit returns without a helper");
        assert_eq!(self.emu.sp(), HOST_STACK);
        let state = self.emu.read(CPU, size_of::<CpuState>());
        let read32 = |at| u32::from_le_bytes(state[at..at + 4].try_into().unwrap());
        let mut actual = initial.clone();
        actual.regs = std::array::from_fn(|n| read32(offset_of!(CpuState, regs) + n * 4));
        actual.eip = read32(offset_of!(CpuState, eip));
        actual.eflags = read32(offset_of!(CpuState, eflags));
        actual.mxcsr = read32(offset_of!(CpuState, mxcsr));
        actual
            .xmm
            .as_flattened_mut()
            .copy_from_slice(&state[at_xmm..at_xmm + 128]);
        let context = self.emu.read(CONTEXT, size_of::<runtime::Runtime>());
        let at = offset_of!(runtime::Runtime, exit);
        let exit =
            Exit::from_raw(u32::from_le_bytes(context[at..at + 4].try_into().unwrap())).unwrap();
        let at = offset_of!(runtime::Runtime, countdown);
        let left = u64::from_le_bytes(context[at..at + 8].try_into().unwrap());
        (actual, exit, left)
    }
}

// spec: A64-TR-5, A64-VER-2
#[test]
fn resident_pixel_loops_cross_emulated_four_kib_pages_without_reordering_accesses() {
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
        a.paddusb(xmm0, xmm1).unwrap();
        a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        a.add(esi, 16).unwrap();
        a.add(edi, 16).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let data: Vec<u8> = (0..0x4000usize).map(|n| (n * 71 + n / 7) as u8).collect();
    let mut state = initial();
    state.regs[1] = 4;
    state.regs[6] = DATA + 4090;
    state.regs[7] = DATA + 8188;
    state.xmm[1] = std::array::from_fn(|n| (n * 13) as u8);
    let mut expected = state.clone();
    let mut mem = memory(&code, &data);
    let mut interpreter = Interpreter::new();
    while expected.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut expected, &mut mem),
            adv_cpu_core::interpreter::Step::Executed
        );
    }
    for flagm in [false, true] {
        let mut h = Harness::new();
        let (actual, exit, left) = h.run(&code, &data, &state, flagm);
        assert_eq!(actual.regs, expected.regs);
        assert_eq!(actual.eflags, expected.eflags);
        assert_eq!(actual.xmm, expected.xmm);
        assert_eq!(actual.eip, expected.eip);
        assert_eq!(exit, Exit::Dispatch);
        assert_eq!(left, 72);
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
            mem.read(DATA, data.len()).unwrap()
        );
    }
}

// spec: A64-FLAG-2
#[test]
fn bit_scans_preserve_undefined_flags_zero_destinations_and_word_halves() {
    let mut h = Harness::new();
    let values: Vec<u32> = [0, u32::MAX, 0x55555555, 0xaaaaaaaa]
        .into_iter()
        .chain((0..32).map(|bit| 1 << bit))
        .collect();
    // TZCNT and LZCNT run as BSF and BSR on this processor.
    for (reverse, counted) in [(false, false), (true, false), (false, true), (true, true)] {
        for word in [false, true] {
            for dst in 0..8 {
                for source in 0..3 {
                    let code = bytes(|a| {
                        // Exercise live flag recipes, not just a canonical entry.
                        a.add(eax, 0x1234).unwrap();
                        let regs = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
                        let words = [ax, cx, dx, bx, sp, bp, si, di];
                        macro_rules! scan {
                            ($d:expr, $s:expr) => {
                                match (reverse, counted) {
                                    (false, false) => a.bsf($d, $s),
                                    (true, false) => a.bsr($d, $s),
                                    (false, true) => a.tzcnt($d, $s),
                                    (true, true) => a.lzcnt($d, $s),
                                }
                                .unwrap()
                            };
                        }
                        if word {
                            match source {
                                0 => scan!(words[dst], words[2]),
                                1 => scan!(words[dst], words[dst]),
                                _ => scan!(words[dst], word_ptr(DATA as i32)),
                            }
                        } else {
                            match source {
                                0 => scan!(regs[dst], edx),
                                1 => scan!(regs[dst], regs[dst]),
                                _ => scan!(regs[dst], dword_ptr(DATA as i32)),
                            }
                        }
                    });
                    for &value in &values {
                        let mut state = initial();
                        state.regs[if source == 1 { dst } else { 2 }] = value;
                        let data = value.to_le_bytes();
                        let want = reference(&code, &data, &state);
                        for flagm in [false, true] {
                            let (got, exit, left) = h.run(&code, &data, &state, flagm);
                            assert_eq!(exit, Exit::Dispatch);
                            assert_eq!(left, 98);
                            assert_eq!(
                                got, want,
                                "reverse={reverse}, counted={counted}, word={word}, dst={dst}, source={source}, value={value}, flagm={flagm}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-FLAG-2
#[test]
fn shifts_by_cl_cover_every_register_and_count_with_live_and_dead_flags() {
    let mut h = Harness::new();
    for dst in 0..8 {
        for kind in 0..5 {
            for dead in [false, true] {
                let code = bytes(|a| {
                    a.add(edx, eax).unwrap();
                    let r = [eax, ecx, edx, ebx, esp, ebp, esi, edi][dst];
                    match kind {
                        0 => a.shl(r, cl),
                        1 => a.shr(r, cl),
                        2 => a.sar(r, cl),
                        3 => a.rol(r, cl),
                        _ => a.ror(r, cl),
                    }
                    .unwrap();
                    // SUB writes every flag, making the shift's flags dead.
                    if dead {
                        a.sub(edx, 19).unwrap();
                    }
                });
                for count in 0..=255 {
                    let mut state = initial();
                    state.regs[dst] = 0x8123ffff;
                    state.regs[1] = (state.regs[1] & !255) | count;
                    let want = reference(&code, &[], &state);
                    let (got, exit, left) = h.run(&code, &[], &state, false);
                    assert_eq!(exit, Exit::Dispatch);
                    assert_eq!(left, if dead { 97 } else { 98 });
                    assert_eq!(
                        got, want,
                        "dst={dst}, kind={kind}, dead={dead}, count={count}"
                    );
                }
            }
        }
    }
}

// spec: A64F-FP-6
#[test]
fn comparisons_with_identical_operands_ignore_scalar_upper_snan_lanes() {
    let mut h = Harness::new();
    for scalar in [false, true] {
        for double in [false, true] {
            let width = if double { 8 } else { 4 };
            let mut state = initial();
            // A subnormal low lane sets DE. Upper scalar lanes are never inputs.
            state.xmm[3][..width].copy_from_slice(&1u64.to_le_bytes()[..width]);
            if scalar {
                let snan = if double {
                    0x7ff0000000000001u64
                } else {
                    0x7f800001u64
                };
                for lane in 1..16 / width {
                    state.xmm[3][lane * width..(lane + 1) * width]
                        .copy_from_slice(&snan.to_le_bytes()[..width]);
                }
            }
            for predicate in 0..8 {
                let code = bytes(|a| {
                    a.add(eax, ecx).unwrap();
                    match (scalar, double) {
                        (false, false) => a.cmpps(xmm3, xmm3, predicate),
                        (true, false) => a.cmpss(xmm3, xmm3, predicate),
                        (false, true) => a.cmppd(xmm3, xmm3, predicate),
                        (true, true) => a.cmpsd_3(xmm3, xmm3, predicate),
                    }
                    .unwrap();
                });
                let want = reference(&code, &[], &state);
                let (got, exit, left) = h.run(&code, &[], &state, false);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(left, 98);
                assert_eq!(got, want);
            }
        }
    }
}

fn cmp_code(scalar: bool, double: bool, memory: bool, predicate: i32) -> Vec<u8> {
    bytes(|a| {
        a.add(eax, ecx).unwrap();
        macro_rules! compare {
            ($op:ident, $mem:expr) => {
                if memory {
                    a.$op(xmm0, $mem, predicate)
                } else {
                    a.$op(xmm0, xmm1, predicate)
                }
                .unwrap()
            };
        }
        match (scalar, double) {
            (false, false) => compare!(cmpps, xmmword_ptr(DATA as i32)),
            (true, false) => compare!(cmpss, dword_ptr(DATA as i32)),
            (false, true) => compare!(cmppd, xmmword_ptr(DATA as i32)),
            (true, true) => compare!(cmpsd_3, qword_ptr(DATA as i32)),
        }
    })
}

// spec: A64F-FP-6
#[test]
fn floating_comparison_masks_cover_predicates_lanes_nan_and_denormal_inputs() {
    let mut h = Harness::new();
    for double in [false, true] {
        let values: &[u64] = if double {
            &[
                0,
                1 << 63,
                1,
                0x000fffffffffffff,
                0x0010000000000000,
                0x3ff0000000000000,
                0xbff8000000000000,
                0x7ff0000000000000,
                0xfff0000000000000,
                0x7ff8000000000001,
            ]
        } else {
            &[
                0,
                1 << 31,
                1,
                0x007fffff,
                0x00800000,
                0x3f800000,
                0xbfc00000,
                0x7f800000,
                0xff800000,
                0x7fc00001,
            ]
        };
        let width = if double { 8 } else { 4 };
        for scalar in [false, true] {
            for memory in [false, true] {
                for predicate in 0..=7 {
                    let code = cmp_code(scalar, double, memory, predicate | 0xf8);
                    for (n, &value) in values.iter().enumerate() {
                        let mut state = initial();
                        state.mxcsr |= 0x21; // Preserve existing sticky exception flags.
                        for lane in 0..16 / width {
                            let a = values[(n + lane) % values.len()].to_le_bytes();
                            let b = values[(n + lane + 3) % values.len()].to_le_bytes();
                            state.xmm[0][lane * width..(lane + 1) * width]
                                .copy_from_slice(&a[..width]);
                            state.xmm[1][lane * width..(lane + 1) * width]
                                .copy_from_slice(&b[..width]);
                        }
                        let nan = |v: u64| {
                            if double {
                                v & 0x7ff0000000000000 == 0x7ff0000000000000
                                    && v & 0x000fffffffffffff != 0
                            } else {
                                v & 0x7f800000 == 0x7f800000 && v & 0x007fffff != 0
                            }
                        };
                        let invalid = [1, 2, 5, 6].contains(&predicate)
                            && (0..if scalar { 1 } else { 16 / width }).any(|lane| {
                                nan(values[(n + lane) % values.len()])
                                    || nan(values[(n + lane + 3) % values.len()])
                            });
                        let data = state.xmm[1];
                        let (got, exit, left) = h.run(&code, &data, &state, false);
                        if invalid {
                            // The owner will interpret the current instruction.
                            let prefix = bytes(|a| a.add(eax, ecx).unwrap());
                            let want = reference(&prefix, &data, &state);
                            assert_eq!(exit, Exit::Interpret);
                            assert_eq!(left, 99);
                            assert_eq!(got, want);
                        } else {
                            let want = reference(&code, &data, &state);
                            assert_eq!(exit, Exit::Dispatch);
                            assert_eq!(left, 98);
                            assert_eq!(
                                got, want,
                                "double={double}, scalar={scalar}, mem={memory}, predicate={predicate}, value={value}"
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-5
#[test]
fn floating_guard_and_snan_bail_before_destination_or_mxcsr_commit() {
    let mut h = Harness::new();
    for double in [false, true] {
        for scalar in [false, true] {
            for mxcsr in [0x1f80, 0x1fc0, 0x9f80, 0x1f00, 0x1d80, 0x3f80] {
                let mut state = initial();
                state.mxcsr = mxcsr;
                let snan = if double {
                    0x7ff0000000000001u64
                } else {
                    0x7f800001u64
                };
                state.xmm[1][..if double { 8 } else { 4 }]
                    .copy_from_slice(&snan.to_le_bytes()[..if double { 8 } else { 4 }]);
                let code = cmp_code(scalar, double, false, 0);
                let prefix = bytes(|a| a.add(eax, ecx).unwrap());
                let want = reference(&prefix, &[], &state);
                let (got, exit, left) = h.run(&code, &[], &state, false);
                assert_eq!(exit, Exit::Interpret);
                assert_eq!(left, 99);
                assert_eq!(got, want);
            }
        }
    }
}

// spec: A64-CTL-6, A64-CTL-10
#[test]
fn switch_table_recognition_checks_unsigned_bounds_pages_and_index_provenance() {
    for (n, ae, byte, clobber, expected) in [
        (7, false, false, false, 8),
        (8, true, false, false, 8),
        (1023, false, false, false, 1024),
        (1024, true, false, false, 1024),
        (1024, false, false, false, 0),
        (0, true, false, false, 0),
        (0, false, true, false, 256),
        (7, false, false, true, 0),
    ] {
        let table = DATA + 4096 - 16;
        let code = bytes(|a| {
            if byte {
                a.movzx(eax, byte_ptr(esi)).unwrap();
            } else {
                a.cmp(eax, n).unwrap();
                if ae {
                    a.jae(CODE as u64 + 0x800).unwrap();
                } else {
                    a.ja(CODE as u64 + 0x800).unwrap();
                }
            }
            a.mov(ecx, 13).unwrap();
            if clobber {
                a.mov(al, 1).unwrap();
            }
            a.jmp(dword_ptr(eax * 4 + table as i32)).unwrap();
        });
        let mut mem = memory(&code, &vec![0; 8192]);
        mem.protect(DATA, 8192, 1).unwrap();
        let mut work = Workspace::default();
        work.unit.images.push((CODE, 0x30000));
        let stubs = Stubs {
            leaf: HOST_CODE as usize,
            exit: HOST_CODE as usize,
            dispatch: HOST_CODE as usize,
            lookup: HOST_CODE as usize,
            lookup_slow: HOST_CODE as usize,
            ret_miss: HOST_CODE as usize,
            access: HOST_CODE as usize,
            interpret: HOST_CODE as usize,
            fault: HOST_CODE as usize,
        };
        for writable_tail in [false, true] {
            if writable_tail {
                mem.protect(DATA + 4096, 4096, 3).unwrap();
            }
            translate(
                &mut work,
                &mut mem,
                CODE,
                &[],
                &|_| false,
                0,
                Features { flagm: false },
                HOST_CODE as usize,
                stubs,
                &|_, _| false,
                &|_| None,
                &|_, _| Vec::new(),
                0x037f,
            );
            let count = work.unit.tables.first().map_or(0, |t| t.targets.len());
            assert_eq!(
                count,
                if writable_tail { 0 } else { expected },
                "n={n} ae={ae} byte={byte} clobber={clobber} writable_tail={writable_tail}"
            );
            if count != 0 {
                assert!(work.code().contains(&(table, (count * 4) as u32)));
            }
        }
    }
}

// spec: A64-FLAG-5
#[test]
fn narrow_immediate_moves_keep_their_value_when_pf_and_af_materialize() {
    // The register a lazy PF/AF recipe reads is overwritten by a narrow
    // immediate move, so the recipe materializes first; that must not
    // replace the moved value with the recipe's immediate.
    let mut h = Harness::new();
    let regs = [eax, ecx, edx, ebx, esp, ebp, esi, edi];
    let bytes8 = [al, cl, dl, iced_x86::code_asm::bl, ah, ch, dh, bh];
    let words = [ax, cx, dx, bx, sp, bp, si, di];
    for part in 0..16 {
        for recipe in 0..3 {
            let base = if part < 8 { part % 4 } else { part - 8 };
            let code = bytes(|a| {
                let reg = regs[base];
                match recipe {
                    0 => a.cmp(reg, 0x50).unwrap(),
                    1 => a.add(reg, 0x5a).unwrap(),
                    _ => a.sub(reg, 0x12345).unwrap(),
                }
                if part < 8 {
                    a.mov(bytes8[part], 0x90).unwrap();
                } else {
                    a.mov(words[part - 8], 0x9091).unwrap();
                }
            });
            let state = initial();
            let want = reference(&code, &[], &state);
            for flagm in [false, true] {
                let (got, exit, _) = h.run(&code, &[], &state, flagm);
                assert_eq!(exit, Exit::Dispatch);
                assert_eq!(got, want, "part={part}, recipe={recipe}, flagm={flagm}");
            }
        }
    }
}

fn j1_unit(code: &[u8]) -> Workspace {
    let mut mem = memory(code, &[]);
    let mut work = Workspace::default();
    let at = HOST_CODE as usize;
    translate(
        &mut work,
        &mut mem,
        CODE,
        &[CODE + code.len() as u32],
        &|_| false,
        0,
        Features { flagm: false },
        at,
        Stubs {
            leaf: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            interpret: at,
            fault: at,
        },
        &|_, _| false,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
    );
    work
}

// spec: A64-FLAG-3, A64-FLAG-5, A64-CTL-7
#[test]
fn j1_hot_paths_omit_dead_logic_tests_and_loop_materialization() {
    let code = bytes(|a| {
        a.or(eax, ebx).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(ecx, 1).unwrap();
    });
    let work = j1_unit(&code);
    assert!(
        !work
            .words()
            .contains(&tst(W, host(0), host(0), Shift::Lsl, 0))
    );
    assert_eq!(work.sites()[0].1.nz, Some(Operand::Guest(0)));
    let code = bytes(|a| {
        a.or(eax, ebx).unwrap();
        a.shl(eax, 3).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(ecx, 1).unwrap();
    });
    let work = j1_unit(&code);
    assert!(!work.words().contains(&mov(W, X10, host(0))));
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let work = j1_unit(&code);
    let start = work.code.asm.position(work.code.labels[0]) / 4;
    let end = work.code.asm.position(work.code.labels[1]) / 4;
    assert_eq!(
        &work.words()[start..end],
        &[sub_imm(W, host(1), host(1), 1, false)]
    );
    assert!(
        work.words()[end..]
            .iter()
            .any(|word| word & 0xff00_001f == cbz(W, true, host(1), 0))
    );
}

// spec: A64F-FP-7
#[test]
fn j1_x87_run_admits_sahf_and_guards_once_until_control_changes() {
    for change in [false, true] {
        let code = bytes(|a| {
            let mut done = a.create_label();
            a.fld1().unwrap();
            a.fld1().unwrap();
            a.fadd_2(st0, st1).unwrap();
            a.fadd_2(st0, st1).unwrap();
            if change {
                a.fldcw(word_ptr(ebx)).unwrap();
            }
            a.fadd_2(st0, st1).unwrap();
            a.fcomp_2(st0, st1).unwrap();
            a.fnstsw(ax).unwrap();
            a.sahf().unwrap();
            a.jb(done).unwrap();
            a.fld1().unwrap();
            a.set_label(&mut done).unwrap();
            a.nop().unwrap();
        });
        let work = j1_unit(&code);
        let hot_end = work
            .code
            .marks
            .iter()
            .find(|mark| mark.part == Part::Cold)
            .unwrap()
            .offset as usize
            / 4;
        let hot = &work.words()[..hot_end];
        let guard = sub_imm(W, X16, temp(1), 0x037f, false);
        assert_eq!(
            hot.iter().filter(|&&word| word == guard).count(),
            if change { 2 } else { 1 }
        );
        assert_eq!(
            hot.iter()
                .filter(|&&word| word == ldrh(X16, X5, runtime::X87_TAG))
                .count(),
            1
        );
    }
}

// spec: A64F-FP-5, A64-REG-5
#[test]
fn j1_sse_reuses_mxcsr_and_pe_checks_until_a_control_write() {
    for control_write in [false, true] {
        let code = bytes(|a| {
            a.addss(xmm0, xmm1).unwrap();
            if control_write {
                a.ldmxcsr(dword_ptr(ebx)).unwrap();
            }
            a.mulss(xmm0, xmm1).unwrap();
            a.subss(xmm0, xmm1).unwrap();
        });
        let work = j1_unit(&code);
        let hot_end = work
            .code
            .marks
            .iter()
            .find(|mark| mark.part == Part::Cold)
            .unwrap()
            .offset as usize
            / 4;
        let loads = work.words()[..hot_end]
            .iter()
            .filter(|&&word| word == ldr(W, X13, X5, runtime::MXCSR))
            .count();
        assert_eq!(loads, if control_write { 4 } else { 2 });
    }
}
