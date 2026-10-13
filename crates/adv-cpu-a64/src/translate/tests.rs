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
                exit: addr(self.stubs.exit),
                dispatch: addr(self.dispatch),
                lookup: addr(self.forbidden),
                lookup_slow: addr(self.forbidden),
                ret_miss: addr(self.forbidden),
                access: addr(self.forbidden),
                interpret: addr(self.forbidden),
            },
        );
        assert!(
            !work
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

// spec: A64-FLAG-2
#[test]
fn bit_scans_preserve_undefined_flags_zero_destinations_and_word_halves() {
    let mut h = Harness::new();
    let values: Vec<u32> = [0, u32::MAX, 0x55555555, 0xaaaaaaaa]
        .into_iter()
        .chain((0..32).map(|bit| 1 << bit))
        .collect();
    for reverse in [false, true] {
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
                                if reverse {
                                    a.bsr($d, $s)
                                } else {
                                    a.bsf($d, $s)
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
                                "reverse={reverse}, word={word}, dst={dst}, source={source}, value={value}, flagm={flagm}"
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

// spec: A64-FP-6
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

// spec: A64-FP-6
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

// spec: A64-FP-5
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
