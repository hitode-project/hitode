//! Execute whole generated units, including entry/exit flag mapping, on any host.
use super::*;
use crate::emulator::{CODE as HOST_CODE, Emulator};
use crate::runtime;
use adv_cpu_core::{CpuState, Memory, X87State, interpreter::Interpreter};
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
            call: host,
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
        let no_calls = |_, _| None;
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
            &no_calls,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
            false,
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
        call: HOST_CODE as usize,
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
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
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
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
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
        self.run_form(code, data, initial, flagm, None)
    }

    fn run_form(
        &mut self,
        code: &[u8],
        data: &[u8],
        initial: &CpuState,
        flagm: bool,
        admitted: Option<u64>,
    ) -> (CpuState, Exit, u64) {
        let mut memory = memory(code, data);
        let at = self.prefix.len() * 4;
        let end = CODE + code.len() as u32;
        let mut work = Workspace::default();
        let addr = |offset| HOST_CODE as usize + offset;
        let stubs = Stubs {
            call: addr(self.forbidden),
            exit: addr(self.stubs.exit),
            dispatch: addr(self.dispatch),
            lookup: addr(self.forbidden),
            lookup_slow: addr(self.forbidden),
            ret_miss: addr(self.forbidden),
            access: addr(self.forbidden),
            interpret: addr(self.forbidden),
            fault: addr(self.forbidden),
        };
        translate(
            &mut work,
            &mut memory,
            CODE,
            &[end],
            &|_| false,
            0,
            Features { flagm },
            addr(at),
            stubs,
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            initial.x87.control,
            false,
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
        #[cfg(native)]
        let mut computation_code = Code::default();
        if admitted.is_some() {
            #[cfg(native)]
            {
                let proof = computation::Proof::build(&work.unit.trace).expect("range proof");
                assert!(
                    proof
                        .evaluate(initial, &mut computation::Evaluation::default(), |addr| {
                            Some(u32::from_le_bytes(
                                memory.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                            ))
                        })
                        .is_some(),
                    "entry satisfies proof"
                );
                let lowered = lower_computation(
                    &mut work.unit,
                    &mut computation_code,
                    CODE,
                    Features { flagm },
                    addr(at),
                    stubs,
                    (initial.x87.control, false, Some(&proof)),
                )
                .expect("detached form");
                host_code.extend_from_slice(lowered.words);
            }
            #[cfg(not(native))]
            panic!("admitted form requires native translation");
        } else {
            host_code.extend_from_slice(work.words());
            // Wide bodies trap, so a matching result proves the double path.
            let marks = work.marks();
            for (n, mark) in marks.iter().enumerate() {
                if mark.part == Part::X87Wide {
                    let end = marks
                        .get(n + 1)
                        .map_or(work.words().len(), |m| m.offset as usize / 4);
                    host_code[at / 4 + mark.offset as usize / 4..at / 4 + end].fill(brk(0x87));
                }
            }
        }
        let mut context = vec![0; size_of::<runtime::Runtime>()];
        let mut put = |offset: usize, value: &[u8]| {
            context[offset..offset + value.len()].copy_from_slice(value)
        };
        put(offset_of!(runtime::Runtime, cpu), &CPU.to_le_bytes());
        put(
            offset_of!(runtime::Runtime, window),
            &WINDOW_BASE.to_le_bytes(),
        );
        put(
            offset_of!(runtime::Runtime, countdown),
            &admitted.unwrap_or(100).to_le_bytes(),
        );
        if admitted.is_some() {
            put(
                offset_of!(runtime::Runtime, shadow),
                &WINDOW_BASE.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, recall),
                &(CONTEXT + 0x3f00).to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, recall_mask),
                &1u32.to_le_bytes(),
            );
            // The recall has spent its grace: leave at the first checkpoint.
            put(
                offset_of!(runtime::Runtime, deferred),
                &runtime::RECALL_GRACE.to_le_bytes(),
            );
        }
        let permissions = CONTEXT + 0x3000;
        put(
            offset_of!(runtime::Runtime, alias_classes),
            &permissions.to_le_bytes(),
        );
        self.emu.write(permissions, &vec![3; 0x100]);
        self.emu.write(CONTEXT + 0x3f00, &1u32.to_le_bytes());
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
        let at_mmx = offset_of!(CpuState, mmx);
        for (n, value) in initial.mmx.iter().enumerate() {
            state[at_mmx + n * 8..at_mmx + n * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        let at_x87 = offset_of!(CpuState, x87);
        let x87 = &initial.x87;
        for (n, r) in x87.regs.iter().enumerate() {
            state[at_x87 + n * 10..at_x87 + n * 10 + 10].copy_from_slice(&r.0);
        }
        for (at, value) in [
            (offset_of!(X87State, control), x87.control),
            (offset_of!(X87State, status), x87.status),
            (offset_of!(X87State, tag), x87.tag),
            (offset_of!(X87State, opcode), x87.opcode),
        ] {
            state[at_x87 + at..at_x87 + at + 2].copy_from_slice(&value.to_le_bytes());
        }
        for (at, value) in [
            (offset_of!(X87State, ip), x87.ip),
            (offset_of!(X87State, dp), x87.dp),
        ] {
            state[at_x87 + at..at_x87 + at + 4].copy_from_slice(&value.to_le_bytes());
        }
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
        actual.mmx = std::array::from_fn(|n| {
            u64::from_le_bytes(
                state[at_mmx + n * 8..at_mmx + n * 8 + 8]
                    .try_into()
                    .unwrap(),
            )
        });
        let read16 =
            |at: usize| u16::from_le_bytes(state[at_x87 + at..at_x87 + at + 2].try_into().unwrap());
        let x87 = &mut actual.x87;
        for (n, r) in x87.regs.iter_mut().enumerate() {
            r.0.copy_from_slice(&state[at_x87 + n * 10..at_x87 + n * 10 + 10]);
        }
        x87.control = read16(offset_of!(X87State, control));
        x87.status = read16(offset_of!(X87State, status));
        x87.tag = read16(offset_of!(X87State, tag));
        x87.opcode = read16(offset_of!(X87State, opcode));
        x87.ip = read32(at_x87 + offset_of!(X87State, ip));
        x87.dp = read32(at_x87 + offset_of!(X87State, dp));
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
fn shifts_by_cl_cover_registers_memory_and_every_count_with_live_and_dead_flags() {
    let mut h = Harness::new();
    for dst in 0..9 {
        for kind in 0..5 {
            for dead in [false, true] {
                let code = bytes(|a| {
                    a.add(edx, eax).unwrap();
                    macro_rules! shift {
                        ($dst:expr) => {
                            match kind {
                                0 => a.shl($dst, cl),
                                1 => a.shr($dst, cl),
                                2 => a.sar($dst, cl),
                                3 => a.rol($dst, cl),
                                _ => a.ror($dst, cl),
                            }
                            .unwrap()
                        };
                    }
                    if dst == 8 {
                        shift!(dword_ptr(ebp + 0x1c));
                        a.mov(edi, dword_ptr(ebp + 0x1c)).unwrap();
                    } else {
                        shift!([eax, ecx, edx, ebx, esp, ebp, esi, edi][dst]);
                    }
                    // SUB writes every flag, making the shift's flags dead.
                    if dead {
                        a.sub(edx, 19).unwrap();
                    }
                });
                for count in 0..=255 {
                    let mut state = initial();
                    if dst < 8 {
                        state.regs[dst] = 0x8123ffff;
                    }
                    state.regs[1] = (state.regs[1] & !255) | count;
                    let data = [0xff; 0x20];
                    let want = reference(&code, &data, &state);
                    let (got, exit, left) = h.run(&code, &data, &state, false);
                    assert_eq!(exit, Exit::Dispatch);
                    assert_eq!(left, 98 - u64::from(dead) - u64::from(dst == 8));
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
            call: HOST_CODE as usize,
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
                &|_, _| None,
                &|_| None,
                &|_, _| Vec::new(),
                0x037f,
                false,
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
            call: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            interpret: at,
            fault: at,
        },
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
        false,
    );
    work
}

#[cfg(native)]
fn proved_division_code(memory_divisor: bool, simd: bool) -> Vec<u8> {
    bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        if simd {
            a.paddd(xmm0, xmm1).unwrap();
        }
        for offset in [0, 16] {
            a.mov(eax, esi).unwrap();
            a.cdq().unwrap();
            if memory_divisor {
                a.idiv(dword_ptr(ebp)).unwrap();
            } else {
                a.idiv(ecx).unwrap();
            }
            a.mov(dword_ptr(edi + offset), eax).unwrap();
            a.mov(dword_ptr(edi + offset + 4), edx).unwrap();
            a.mov(eax, edx).unwrap();
            if memory_divisor {
                a.add(eax, dword_ptr(ebp)).unwrap();
            } else {
                a.add(eax, ecx).unwrap();
            }
            a.cdq().unwrap();
            if memory_divisor {
                a.idiv(dword_ptr(ebp)).unwrap();
            } else {
                a.idiv(ecx).unwrap();
            }
            a.mov(dword_ptr(edi + offset + 8), eax).unwrap();
            a.mov(dword_ptr(edi + offset + 12), edx).unwrap();
        }
        if simd {
            a.movdqu(xmmword_ptr(edi + 32), xmm0).unwrap();
        }
        a.add(edi, if simd { 48 } else { 32 }).unwrap();
        a.dec(ebx).unwrap();
        a.jne(top).unwrap();
    })
}

// spec: A64-TR-4 A64-TR-9 A64-VER-1
#[test]
#[cfg(native)]
fn proved_divisions_keep_both_results_flags_memory_and_budget_prefixes() {
    let mut h = Harness::new();
    for memory_divisor in [false, true] {
        let code = proved_division_code(memory_divisor, true);
        for numerator in [i32::MIN, -1_073_741_825, -1, 0, 1, i32::MAX] {
            for divisor in [1u32, 2, 1 << 30] {
                let mut data = vec![0; 0x500];
                data[..4].copy_from_slice(&divisor.to_le_bytes());
                let mut state = initial();
                state.regs[1] = divisor;
                state.regs[3] = 2;
                state.regs[5] = DATA;
                state.regs[6] = numerator as u32;
                state.regs[7] = DATA + 0x400;
                state.xmm[0] = [0x21; 16];
                state.xmm[1] = [0x67; 16];
                let mut reference_memory = memory(&code, &data);
                let mut want = state.clone();
                let mut interpreter = Interpreter::new();
                let mut retired = 0;
                while want.eip != CODE + code.len() as u32 {
                    assert_eq!(
                        interpreter.step(&mut want, &mut reference_memory),
                        adv_cpu_core::interpreter::Step::Executed
                    );
                    retired += 1;
                }
                let want_data = reference_memory.read_data(DATA, data.len()).unwrap();
                for flagm in [false, true] {
                    let (got, exit, left) = h.run_form(&code, &data, &state, flagm, Some(100));
                    assert_eq!((exit, 100 - left), (Exit::Dispatch, retired));
                    assert_eq!(got, want, "n={numerator} d={divisor} mem={memory_divisor}");
                    assert_eq!(
                        h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                        want_data.as_ref()
                    );
                    if numerator == i32::MIN && divisor == 1 << 30
                        || numerator == -1 && divisor == 2
                    {
                        for fuel in 0..retired {
                            let (got, exit, left) =
                                h.run_form(&code, &data, &state, flagm, Some(fuel));
                            assert_eq!(exit, Exit::Recall);
                            let mut want = state.clone();
                            let mut reference_memory = memory(&code, &data);
                            for _ in 0..fuel - left {
                                assert_eq!(
                                    interpreter.step(&mut want, &mut reference_memory),
                                    adv_cpu_core::interpreter::Step::Executed
                                );
                            }
                            assert_eq!(got, want, "fuel={fuel}, left={left}, mem={memory_divisor}");
                            assert_eq!(
                                h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                                reference_memory
                                    .read_data(DATA, data.len())
                                    .unwrap()
                                    .as_ref()
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4 A64-TR-9
#[test]
#[cfg(native)]
fn proved_divisions_leave_unadmitted_values_to_the_ordinary_form() {
    let mut h = Harness::new();
    for memory_divisor in [false, true] {
        let code = proved_division_code(memory_divisor, false);
        let work = j1_unit(&code);
        let proof = computation::Proof::build(&work.unit.trace).unwrap();
        for divisor in [0u32, u32::MAX, (1 << 30) + 1] {
            let mut data = vec![0; 0x500];
            data[..4].copy_from_slice(&divisor.to_le_bytes());
            let mut state = initial();
            state.regs[1] = divisor;
            state.regs[3] = 2;
            state.regs[5] = DATA;
            state.regs[6] = 10;
            state.regs[7] = DATA + 0x400;
            let mem = memory(&code, &data);
            assert!(
                proof
                    .evaluate(&state, &mut computation::Evaluation::default(), |addr| {
                        Some(u32::from_le_bytes(
                            mem.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                        ))
                    })
                    .is_none()
            );
            if divisor != 0 {
                let want = reference(&code, &data, &state);
                for flagm in [false, true] {
                    let (got, exit, _) = h.run(&code, &data, &state, flagm);
                    assert_eq!(exit, Exit::Dispatch);
                    assert_eq!(got, want);
                }
            }
        }
    }
}

// spec: A64-TR-4 A64-TR-9 A64-REG-3 A64-MEM-10
#[test]
#[cfg(native)]
fn proved_divisions_shrink_emitted_sequences_and_retain_dirty_simd_fault_sites() {
    for (memory_divisor, simd) in [(false, false), (true, true)] {
        let guest = proved_division_code(memory_divisor, simd);
        let mut work = j1_unit(&guest);
        let proof = computation::Proof::build(&work.unit.trace).unwrap();
        let at = HOST_CODE as usize;
        let stubs = Stubs {
            call: at,
            exit: at,
            dispatch: at,
            lookup: at,
            lookup_slow: at,
            ret_miss: at,
            access: at,
            interpret: at,
            fault: at,
        };
        let mut totals = Vec::new();
        for facts in [None, Some(&proof)] {
            let mut code = Code::default();
            lower_computation(
                &mut work.unit,
                &mut code,
                CODE,
                Features { flagm: false },
                at,
                stubs,
                (0x037f, false, facts),
            )
            .unwrap();
            let mut fixed = 0;
            let mut hot = 0;
            for (index, inst) in work.unit.trace.insts.iter().enumerate() {
                if !matches!(inst.op, Op::Cdq | Op::Integer(integer::Integer::Divide(..))) {
                    continue;
                }
                let begin = code.asm.position(code.labels[index]) / 4;
                let end = code.asm.position(code.labels[index + 1]) / 4;
                let words = &code.asm.words()[begin..end];
                if matches!(inst.op, Op::Cdq) {
                    assert!(words.contains(&sbfx(W, host(2), host(0), 31, 1)));
                    fixed += 1;
                    hot += words.len();
                } else if facts.is_none() {
                    let divide = words
                        .iter()
                        .position(|&w| w == sdiv(X, X17, X16, X14))
                        .unwrap();
                    assert_eq!(
                        words[divide - 5] & 0xff00_0000,
                        cbz(W, false, X0, 0) & 0xff00_0000
                    );
                    assert_eq!(words[divide + 6] & 0xfc00_0000, b(0));
                    fixed += 12;
                    hot += divide - 5 + 12;
                } else {
                    let fixed_words = match proof.division(index).unwrap() {
                        computation::Division::SignExtended => 3,
                        computation::Division::Normalized => 5,
                    };
                    fixed += fixed_words;
                    hot += words.len();
                }
            }
            if facts.is_some() && memory_divisor && simd {
                for inst in work
                    .unit
                    .trace
                    .insts
                    .iter()
                    .filter(|inst| matches!(inst.op, Op::Integer(integer::Integer::Divide(..))))
                {
                    let site = code
                        .sites
                        .iter()
                        .find(|(_, site)| site.eip == inst.pc)
                        .unwrap()
                        .1;
                    assert_ne!(
                        site.simd_dirty & (1 << 8),
                        0,
                        "live XMM0 at the divisor load"
                    );
                }
            }
            totals.push((fixed, hot));
        }
        assert_eq!((totals[0].0, totals[1].0), (52, 20));
        assert!(totals[0].1 - totals[1].1 >= 32);
        eprintln!(
            "four DIV + CDQ: fixed 52 -> 20 words; with wrappers {} -> {} (memory={memory_divisor}, simd={simd})",
            totals[0].1, totals[1].1
        );
    }
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

/// Runs `code` with ESP inside the data window and sticky PE, comparing x87,
/// MMX, integer and SSE state with the interpreter; wide bodies trap.
fn double_loop(h: &mut Harness, code: &[u8], data: &[u8], control: u16) {
    let mut state = initial();
    state.regs[1] = 2;
    state.regs[4] = DATA + 0xff00;
    state.x87.set_control(control);
    // Sticky PE as in a running program: inexact conversions stay native.
    state.x87.status |= 0x20;
    state.mxcsr |= 0x20;
    let expected = reference(code, data, &state);
    let (got, exit, _) = h.run(code, data, &state, false);
    assert_eq!(exit, Exit::Dispatch);
    assert_eq!(got.eip, expected.eip);
    assert_eq!(got.x87, expected.x87);
    assert_eq!(got.mmx, expected.mmx);
    assert_eq!(got.regs, expected.regs);
    assert_eq!(got.eflags, expected.eflags);
    assert_eq!(got.xmm, expected.xmm);
}

// spec: A64F-FP-3, A64F-FP-7
#[test]
fn double_arithmetic_stays_native_after_control_restores_in_loops() {
    // The cached guard decision travels with the control word; a later
    // comparison must not count it as a control bit.
    let code = bytes(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.fld(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fnstcw(word_ptr(ebx + 16)).unwrap();
        a.movzx(eax, word_ptr(ebx + 16)).unwrap();
        a.or(eax, 0xc00).unwrap();
        a.mov(dword_ptr(ebx + 20), eax).unwrap();
        a.fldcw(word_ptr(ebx + 20)).unwrap();
        a.fistp(qword_ptr(ebx + 24)).unwrap();
        a.fldcw(word_ptr(ebx + 16)).unwrap();
        a.fld(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fstp(qword_ptr(ebx + 32)).unwrap();
        a.movsd_2(xmm1, qword_ptr(ebx + 32)).unwrap();
        a.mov(edx, dword_ptr(ebx + 24)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let mut data = vec![0; 40];
    data[..8].copy_from_slice(&1.25f64.to_le_bytes());
    data[8..16].copy_from_slice(&(-0.5f64).to_le_bytes());
    let mut h = Harness::new();
    for control in [0x007f, 0x027f] {
        double_loop(&mut h, &code, &data, control);
    }
}

// spec: A64F-FP-7
#[test]
fn double_fraction_loops_stay_in_one_run_across_calls_sse_and_truncation() {
    let code = bytes(|a| {
        let (mut top, mut floor, mut convert) =
            (a.create_label(), a.create_label(), a.create_label());
        a.jmp(top).unwrap();
        a.set_label(&mut floor).unwrap();
        a.stmxcsr(dword_ptr(ebx + 40)).unwrap();
        a.fnstcw(word_ptr(ebx + 44)).unwrap();
        a.movsd_2(xmm0, qword_ptr(ebx)).unwrap();
        a.cvttsd2si(eax, xmm0).unwrap();
        a.cvtsi2sd(xmm0, eax).unwrap();
        a.movq(qword_ptr(ebx + 48), xmm0).unwrap();
        a.fld(qword_ptr(ebx + 48)).unwrap();
        a.ret().unwrap();
        a.set_label(&mut convert).unwrap();
        a.push(ebp).unwrap();
        a.mov(ebp, esp).unwrap();
        a.sub(esp, 8).unwrap();
        a.and(esp, -8).unwrap();
        a.fstp(qword_ptr(esp)).unwrap();
        a.cvttsd2si(eax, qword_ptr(esp)).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
        a.set_label(&mut top).unwrap();
        a.call(floor).unwrap();
        a.fsubr(qword_ptr(ebx)).unwrap();
        a.fmul(qword_ptr(ebx + 8)).unwrap();
        a.fld(qword_ptr(ebx + 16)).unwrap();
        a.fmulp(st1, st0).unwrap();
        a.fld1().unwrap();
        a.fsubrp(st1, st0).unwrap();
        a.fld(st0).unwrap();
        a.call(convert).unwrap();
        a.fnstcw(word_ptr(ebx + 24)).unwrap();
        a.movzx(eax, word_ptr(ebx + 24)).unwrap();
        a.or(eax, 0xc00).unwrap();
        a.mov(dword_ptr(ebx + 28), eax).unwrap();
        a.fldcw(word_ptr(ebx + 28)).unwrap();
        a.fistp(qword_ptr(ebx + 32)).unwrap();
        a.fldcw(word_ptr(ebx + 24)).unwrap();
        a.mov(edx, dword_ptr(ebx + 32)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    let mut h = Harness::new();
    for x in [0.0f64, -0.0, 2.0, 2.75] {
        let mut data = vec![0; 56];
        for (at, value) in [(0, x), (8, 0.5), (16, 1.25)] {
            data[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        double_loop(&mut h, &code, &data, 0x027f);
    }
    let work = j1_unit(&code);
    let marks = work.marks();
    let hot = marks.iter().take_while(|m| m.part != Part::Cold);
    assert_eq!(hot.clone().filter(|m| m.part == Part::X87Entry).count(), 1);
    assert!(!hot.clone().any(|m| m.part == Part::X87Publish));
}

// spec: A64F-FP-5, A64-REG-5
#[test]
fn j1_sse_reuses_mxcsr_and_pe_checks_until_a_control_write() {
    for control_write in [false, true] {
        let code = bytes(|a| {
            a.subss(xmm0, xmm1).unwrap();
            if control_write {
                a.ldmxcsr(dword_ptr(ebx)).unwrap();
                a.subss(xmm0, xmm1).unwrap();
            }
            a.mulss(xmm0, xmm1).unwrap();
            a.addss(xmm0, xmm1).unwrap();
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

// spec: A64-TR-9 A64-CTL-8
#[test]
#[cfg(native)]
fn admitted_frame_values_are_reloaded_at_each_outer_iteration() {
    let mut h = Harness::new();
    for overlapping_store in [false, true] {
        let code = bytes(|a| {
            let mut outer = a.create_label();
            let mut inner = a.create_label();
            let mut alternate = a.create_label();
            let mut output = a.create_label();
            a.set_label(&mut outer).unwrap();
            a.mov(dword_ptr(esp + 4), ebx).unwrap();
            a.mov(dword_ptr(esp + 8), 7).unwrap();
            a.mov(dword_ptr(esp + 12), 11).unwrap();
            a.mov(ecx, 4).unwrap();
            a.set_label(&mut inner).unwrap();
            a.mov(eax, dword_ptr(esp + 4)).unwrap();
            a.test(ebx, 1).unwrap();
            a.jz(alternate).unwrap();
            a.imul_2(eax, dword_ptr(esp + 8)).unwrap();
            a.jmp(output).unwrap();
            a.set_label(&mut alternate).unwrap();
            a.imul_2(eax, dword_ptr(esp + 12)).unwrap();
            if overlapping_store {
                a.mov(byte_ptr(esp + 5), al).unwrap();
            }
            a.set_label(&mut output).unwrap();
            a.mov(edx, dword_ptr(esp + 4)).unwrap();
            a.add(eax, edx).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(inner).unwrap();
            a.dec(ebx).unwrap();
            a.jne(outer).unwrap();
        });
        let work = j1_unit(&code);
        let proof = computation::Proof::build(&work.unit.trace).expect("nested proof");
        let cache = proof.frame_cache().expect("inner loop residence");
        assert!(cache.offsets.contains(&8) && cache.offsets.contains(&12));
        assert_eq!(cache.offsets.contains(&4), !overlapping_store);

        let mut state = initial();
        state.regs[3] = 3;
        state.regs[4] = DATA + 0x100;
        state.regs[7] = DATA + 0x200;
        let data = vec![0; 0x300];
        let mut interpreter = Interpreter::new();
        let mut want = state.clone();
        let mut mem = memory(&code, &data);
        let mut prefixes = vec![(want.clone(), data.clone())];
        while want.eip != CODE + code.len() as u32 {
            assert_eq!(
                interpreter.step(&mut want, &mut mem),
                adv_cpu_core::interpreter::Step::Executed
            );
            prefixes.push((
                want.clone(),
                mem.read_data(DATA, data.len()).unwrap().to_vec(),
            ));
        }
        let retired = prefixes.len() as u64 - 1;
        for fuel in 0..=retired {
            let (got, exit, left) = h.run_form(&code, &data, &state, false, Some(fuel));
            let completed = fuel - left;
            let (want, bytes) = &prefixes[completed as usize];
            assert_eq!(got, *want, "overlap={overlapping_store} fuel={fuel}");
            assert_eq!(
                h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
                *bytes
            );
            assert_eq!(
                exit,
                if completed == retired {
                    Exit::Dispatch
                } else {
                    Exit::Recall
                }
            );
        }
    }
}

// spec: A64-TR-9
#[test]
#[cfg(native)]
fn table_row_loops_admit_gathered_entries_mirrored_pointers_and_inclusive_bounds() {
    // Rows run from a nonzero first row to an inclusive last row. Each
    // reads its source through a row table, biased by a negative offset
    // that wraps the address sum, and its columns through a column table.
    // It updates its output row in place through a pointer that the loop
    // also keeps in a frame slot.
    let code = bytes(|a| {
        let mut outer = a.create_label();
        let mut inner = a.create_label();
        let mut skip = a.create_label();
        a.set_label(&mut outer).unwrap();
        a.mov(ebp, dword_ptr(esp)).unwrap();
        a.mov(edx, dword_ptr(esp + 20)).unwrap();
        a.mov(edx, dword_ptr(edx + ebp * 4)).unwrap();
        a.add(edx, dword_ptr(esp + 32)).unwrap();
        a.xor(ebp, ebp).unwrap();
        a.mov(ecx, dword_ptr(esp + 28)).unwrap();
        a.test(ecx, ecx).unwrap();
        a.jle(skip).unwrap();
        a.set_label(&mut inner).unwrap();
        a.mov(ebx, dword_ptr(esp + 24)).unwrap();
        a.mov(ebx, dword_ptr(ebx + ebp * 4)).unwrap();
        a.movzx(eax, byte_ptr(ebx + edx)).unwrap();
        a.mov(esi, dword_ptr(edi)).unwrap();
        a.add(eax, esi).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, dword_ptr(esp + 16)).unwrap();
        a.inc(ebp).unwrap();
        a.cmp(ebp, ecx).unwrap();
        a.jl(inner).unwrap();
        a.set_label(&mut skip).unwrap();
        a.mov(edx, dword_ptr(esp)).unwrap();
        a.mov(edi, dword_ptr(esp + 8)).unwrap();
        a.add(edi, dword_ptr(esp + 12)).unwrap();
        a.inc(edx).unwrap();
        a.cmp(edx, dword_ptr(esp + 4)).unwrap();
        a.mov(dword_ptr(esp), edx).unwrap();
        a.mov(dword_ptr(esp + 8), edi).unwrap();
        a.jle(outer).unwrap();
    });
    let work = j1_unit(&code);
    let proof = computation::Proof::build(&work.unit.trace).expect("table row proof");
    const FRAME: u32 = 0x100;
    const ROWS: u32 = 0x180;
    const COLUMNS: u32 = 0x1c0;
    const OUTPUT: u32 = 0x200;
    const SOURCE: u32 = 0x300;
    let setup = |first: u32, last: u32, mirror: u32, rows: u32| {
        let mut data = vec![0u8; 0x400];
        let mut put = |at: u32, value: u32| {
            data[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes())
        };
        let output = DATA + OUTPUT + first * 0x20;
        for (slot, value) in [
            first,
            last,
            output,
            0x20,
            4,
            DATA + rows,
            DATA + COLUMNS,
            3,
            0x10u32.wrapping_neg(),
        ]
        .into_iter()
        .enumerate()
        {
            put(FRAME + slot as u32 * 4, value);
        }
        for row in 0..8 {
            put(rows + row * 4, DATA + SOURCE + row * 0x10 + 0x10);
        }
        for (n, offset) in [2, 5, 1].into_iter().enumerate() {
            put(COLUMNS + n as u32 * 4, offset);
        }
        for at in SOURCE..0x400 {
            data[at as usize] = at as u8;
        }
        for at in OUTPUT..SOURCE {
            data[at as usize] = (at * 7) as u8;
        }
        let mut state = initial();
        state.regs[4] = DATA + FRAME;
        state.regs[7] = output + mirror;
        (state, data)
    };
    let admits = |(state, data): &(CpuState, Vec<u8>)| {
        let mem = memory(&code, data);
        proof
            .evaluate(state, &mut computation::Evaluation::default(), |addr| {
                Some(u32::from_le_bytes(
                    mem.read_data(addr, 4).ok()?.as_ref().try_into().ok()?,
                ))
            })
            .is_some()
    };
    // The pointer must start equal to its slot; an inclusive bound may not
    // be i32::MAX; a first row past the last still runs once in the guest.
    assert!(!admits(&setup(2, 4, 4, ROWS)));
    assert!(!admits(&setup(2, i32::MAX as u32, 0, ROWS)));
    assert!(!admits(&setup(5, 4, 0, ROWS)));
    // Rows may not write the table that locates them.
    assert!(!admits(&setup(2, 4, 0, OUTPUT + 0x40)));
    let mut h = Harness::new();
    let (state, data) = setup(2, 4, 0, ROWS);
    let mut interpreter = Interpreter::new();
    let mut want = state.clone();
    let mut mem = memory(&code, &data);
    let mut prefixes = vec![(want.clone(), data.clone())];
    while want.eip != CODE + code.len() as u32 {
        assert_eq!(
            interpreter.step(&mut want, &mut mem),
            adv_cpu_core::interpreter::Step::Executed
        );
        prefixes.push((
            want.clone(),
            mem.read_data(DATA, data.len()).unwrap().to_vec(),
        ));
    }
    let retired = prefixes.len() as u64 - 1;
    for fuel in 0..=retired {
        let (got, exit, left) = h.run_form(&code, &data, &state, false, Some(fuel));
        let completed = fuel - left;
        let (want, bytes) = &prefixes[completed as usize];
        assert_eq!(got, *want, "fuel={fuel}");
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), data.len()),
            *bytes
        );
        assert_eq!(
            exit,
            if completed == retired {
                Exit::Dispatch
            } else {
                Exit::Recall
            }
        );
    }
}

mod opt_back {
    use super::super::opt::{back, eval, ir::*, recovery::*};
    use super::super::opt::{ir::Op, recovery::Recipe};
    use super::*;
    use crate::encode::bl;

    const AREA: u64 = 0x4000_0000;
    const PARAMETER: u64 = AREA;
    const SAVED_FPCR: u64 = 1 << 25;

    struct Captured {
        cpu: CpuState,
        frame: Vec<(i32, Vec<u8>)>,
        record: RecordId,
        countdown: u64,
        completed: u64,
        registers: runtime::RegisterDump,
        spills: Vec<u128>,
        lowered: back::Lowered,
    }

    impl Harness {
        fn run_opt(
            &mut self,
            func: &Func,
            params: &[u128],
            budget: u64,
            recall: u32,
            head_exit: u32,
        ) -> Captured {
            let mut code = self.prefix.clone();
            let wrapper = code.len() * 4;
            code.extend_from_slice(&[
                stp(X, Index::Pre, Reg::LR, Reg::ZR, Reg::new(31), -16),
                movz(X, X16, (SAVED_FPCR >> 16) as u16, 16),
                msr_fpcr(X16),
                bl(self.stubs.enter as i32 - (wrapper + 12) as i32),
                mrs_fpcr(X16),
                ldp(X, Index::Post, Reg::LR, Reg::ZR, Reg::new(31), 16),
                ret(Reg::LR),
            ]);
            let at = code.len() * 4;
            let lowered = back::lower(func, at, &self.stubs);
            code.extend_from_slice(&lowered.words);
            let mut context = vec![0; size_of::<runtime::Runtime>()];
            let mut put = |offset: usize, value: &[u8]| {
                context[offset..offset + value.len()].copy_from_slice(value)
            };
            for (offset, value) in [
                (offset_of!(runtime::Runtime, cpu), CPU),
                (offset_of!(runtime::Runtime, window), WINDOW_BASE),
                (offset_of!(runtime::Runtime, shadow), WINDOW_BASE),
                (offset_of!(runtime::Runtime, countdown), budget),
                (offset_of!(runtime::Runtime, opt_params), PARAMETER),
                (offset_of!(runtime::Runtime, recall), CONTEXT + 0x3f00),
                (
                    offset_of!(runtime::Runtime, computation_pending),
                    CONTEXT + 0x3f04,
                ),
            ] {
                put(offset, &value.to_le_bytes());
            }
            put(
                offset_of!(runtime::Runtime, recall_mask),
                &1u32.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, computation_pending_mask),
                &2u32.to_le_bytes(),
            );
            put(
                offset_of!(runtime::Runtime, head_exit),
                &head_exit.to_le_bytes(),
            );
            self.emu.write(CONTEXT, &context);
            self.emu.write(CONTEXT + 0x3f00, &recall.to_le_bytes());
            self.emu
                .write(CONTEXT + 0x3f04, &(recall & 2).to_le_bytes());
            self.emu.write(
                PARAMETER,
                &params
                    .iter()
                    .flat_map(|p| p.to_le_bytes())
                    .collect::<Vec<_>>(),
            );
            let state = vec![0; size_of::<CpuState>()];
            self.emu.write(CPU, &state);
            self.emu.set_sp(HOST_STACK);
            self.emu.set_x(X0, CONTEXT);
            self.emu.set_x(X1, HOST_CODE + at as u64);
            self.emu.set_x(Reg::new(18), 0x1818);
            for n in 19..30 {
                self.emu.set_x(Reg::new(n), 0xcafe_0000 + u64::from(n));
            }
            for n in 8..16 {
                self.emu.set_q(n, 0xbeef_0000 + u128::from(n));
            }
            let result = self.emu.call_at(&code, wrapper);
            if result.is_err() {
                // Model signal-context capture and resume through opt_exit. This
                // test has a single faulting site; native signal wiring is separate.
                assert_eq!(lowered.sites.len(), 1);
                let id = lowered.sites[0].1.opt_record.unwrap();
                self.emu.write(
                    CONTEXT + offset_of!(runtime::Runtime, opt_record) as u64,
                    &id.to_le_bytes(),
                );
                let mut tail = code.clone();
                let fault = tail.len() * 4;
                tail.push(b(self.stubs.opt_exit as i32 - fault as i32));
                self.emu
                    .call_at(&tail, fault)
                    .expect("fault recovery returns");
            }
            assert_eq!(self.emu.x(X16), SAVED_FPCR, "FPCR restored");
            assert_eq!(self.emu.x(Reg::new(18)), 0x1818);
            for n in 19..30 {
                assert_eq!(
                    self.emu.x(Reg::new(n)),
                    0xcafe_0000 + u64::from(n),
                    "host x{n}"
                );
            }
            for n in 8..16 {
                assert_eq!(
                    self.emu.q(n) as u64,
                    0xbeef_0000 + u64::from(n),
                    "host d{n}"
                );
            }
            assert_eq!(self.emu.sp(), HOST_STACK);
            assert_eq!(
                self.emu.read(CPU, state.len()),
                state,
                "no canonical write-back"
            );
            let context = self.emu.read(CONTEXT, context.len());
            let word =
                |offset: usize| u32::from_le_bytes(context[offset..][..4].try_into().unwrap());
            let wide =
                |offset: usize| u64::from_le_bytes(context[offset..][..8].try_into().unwrap());
            assert_eq!(
                word(offset_of!(runtime::Runtime, exit)),
                Exit::Optimized as u32
            );
            let base = offset_of!(runtime::Runtime, opt_dump);
            let registers = runtime::RegisterDump {
                sp: wide(base + offset_of!(runtime::RegisterDump, sp)),
                x: std::array::from_fn(|i| wide(base + i * 8)),
                v: std::array::from_fn(|i| {
                    let offset = base + offset_of!(runtime::RegisterDump, v) + i * 16;
                    u128::from_le_bytes(context[offset..][..16].try_into().unwrap())
                }),
            };
            let spills = self
                .emu
                .read(registers.sp, lowered.spill_cells * 16)
                .as_chunks::<16>()
                .0
                .iter()
                .map(|b| u128::from_le_bytes(*b))
                .collect::<Vec<_>>();
            let record = RecordId(word(offset_of!(runtime::Runtime, opt_record)));
            let mut cpu = initial();
            let mut frame = Vec::new();
            let refund = materialize(
                &lowered.records[record.0 as usize],
                &registers,
                &spills,
                &lowered.flags,
                &mut cpu,
                |offset, bytes| frame.push((offset, bytes.to_vec())),
            );
            Captured {
                cpu,
                frame,
                record,
                countdown: wide(offset_of!(runtime::Runtime, countdown))
                    .wrapping_add(u64::from(refund)),
                completed: wide(offset_of!(runtime::Runtime, completed)),
                registers,
                spills,
                lowered,
            }
        }
    }

    fn harness() -> Harness {
        let mut h = Harness::new();
        h.emu.map(AREA, 0x20000);
        h
    }

    fn record(eip: u32) -> IrRecord {
        IrRecord {
            eip,
            refund: 0,
            regs: [Some(Location::Constant(0)); 8],
            eflags: Some(Location::Constant(0x202)),
            x87: X87 {
                top: 0,
                valid: 0,
                control: 0x027f,
                stack: [None; 8],
                status: 0x20,
                c1: Some(Location::Constant(0)),
                ip: Some(Location::Constant(0x1234)),
                opcode: Some(Location::Constant(0x321)),
            },
            xmm: [Some(Location::Constant(0)); 8],
            frame: Vec::new(),
        }
    }

    fn start(types: &[Type]) -> (Func, Block, Vec<Value>, RecordId) {
        let mut f = Func::default();
        let entry = f.record(record(0x1000));
        let b = f.block(Some(Head { record: entry }));
        let values = types.iter().map(|&ty| f.param(b, ty)).collect();
        f.entry = types
            .iter()
            .map(|ty| match ty {
                Type::I32 => Source::Reg(0),
                Type::I64 => Source::Frame {
                    offset: 0,
                    width: 8,
                },
                Type::F64 => Source::X87(0),
                Type::V128 => Source::Xmm(0),
                Type::Flags => Source::Eflags,
            })
            .collect();
        (f, b, values, entry)
    }

    fn finish(f: &mut Func, b: Block, value: Value) -> RecordId {
        let mut r = record(0x2000);
        match f.ty(value) {
            Type::I32 | Type::I64 => {
                r.regs[0] = Some(Location::At(value));
                r.frame.push((-8, 8, Location::At(value)));
            }
            Type::Flags => r.eflags = Some(Location::Recipe(Recipe::Eflags(value))),
            Type::F64 => {
                r.x87.valid = 1;
                r.x87.stack[0] = Some(Location::At(value));
            }
            Type::V128 => r.xmm[0] = Some(Location::At(value)),
        }
        let exit = f.record(r);
        f.end(b, End::Exit(exit));
        exit
    }

    fn compare(f: &Func, params: &[u128], h: &mut Harness) -> Captured {
        let mut memory = memory(&[], &[]);
        let run = eval::run(f, params, &mut memory, 10000);
        let got = h.run_opt(f, params, 10000, 0, 0);
        let want = match run.outcome {
            eval::Outcome::Exit(r) | eval::Outcome::Fail(r) => r,
            _ => panic!("reference terminates"),
        };
        assert_eq!(got.record, want);
        compare_record(f, &run.values, &got);
        assert_eq!(
            10000u64
                .wrapping_sub(got.countdown)
                .wrapping_add(got.completed),
            (run.charged - i64::from(f.records[want.0 as usize].refund)) as u64
        );
        got
    }

    fn compare_record(f: &Func, values: &[u128], got: &Captured) {
        let r = &f.records[got.record.0 as usize];
        let locate = |l: Location<Value>| eval::locate(&l, values);
        for (i, l) in r.regs.iter().enumerate() {
            if let Some(l) = l {
                assert_eq!(got.cpu.regs[i], locate(*l) as u32, "reg {i}");
            }
        }
        if let Some(l) = r.eflags {
            assert_eq!(got.cpu.eflags, locate(l) as u32, "eflags");
        }
        assert_eq!(got.cpu.eip, r.eip);
        assert_eq!(got.cpu.x87.top(), r.x87.top as usize);
        assert_eq!(got.cpu.x87.control, r.x87.control);
        for i in 0..8 {
            if r.x87.valid >> i & 1 != 0 {
                if r.x87.stack[i].is_some() {
                    assert_eq!(got.cpu.x87.tag >> (2 * i) & 3, got.cpu.x87.regs[i].tag());
                }
            } else {
                assert_eq!(
                    got.cpu.x87.tag >> (2 * i) & 3,
                    initial().x87.tag >> (2 * i) & 3
                );
            }
        }
        assert_eq!(
            got.cpu.x87.status & !((7 << 11) | (1 << 9)),
            r.x87.status & !((7 << 11) | (1 << 9))
        );
        if let Some(l) = r.x87.ip {
            assert_eq!(got.cpu.x87.ip, locate(l) as u32);
        }
        if let Some(l) = r.x87.opcode {
            assert_eq!(got.cpu.x87.opcode, locate(l) as u16);
        }
        if let Some(l) = r.x87.c1 {
            assert_eq!(u128::from(got.cpu.x87.status >> 9 & 1), locate(l));
        }
        for (i, l) in r.x87.stack.iter().enumerate() {
            if let Some(l) = l {
                let bits = locate(*l) as u64;
                let reg = got.cpu.x87.regs[i];
                // Independent exact-double representation check, including zeros,
                // infinities and denormals.
                let exponent = bits >> 52 & 0x7ff;
                let fraction = bits & ((1 << 52) - 1);
                assert_eq!(reg.sign_exponent() >> 15, (bits >> 63) as u16);
                if exponent == 0 && fraction == 0 {
                    assert_eq!(reg.significand(), 0);
                    assert_eq!(reg.sign_exponent() & 0x7fff, 0);
                } else if exponent == 0x7ff {
                    assert_eq!(reg.sign_exponent() & 0x7fff, 0x7fff);
                    assert_eq!(reg.significand(), 1 << 63 | fraction << 11);
                } else {
                    let exponent = i32::from(reg.sign_exponent() & 0x7fff) - 16383;
                    let mantissa = reg.significand() as f64 / 2f64.powi(63);
                    let actual = if exponent < -1022 {
                        (mantissa * 2f64.powi(exponent + 1022)) * f64::MIN_POSITIVE
                    } else {
                        mantissa * 2f64.powi(exponent)
                    };
                    assert_eq!(actual, f64::from_bits(bits).abs());
                }
            }
        }
        for (i, l) in r.xmm.iter().enumerate() {
            if let Some(l) = l {
                assert_eq!(got.cpu.xmm[i], locate(*l).to_le_bytes());
            }
        }
        assert_eq!(
            got.frame,
            r.frame
                .iter()
                .map(|&(o, w, l)| (o, locate(l).to_le_bytes()[..w as usize].to_vec()))
                .collect::<Vec<_>>()
        );
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_integer_operations_and_conversions_match_reference() {
        let mut h = harness();
        for ty in [Type::I32, Type::I64] {
            for op in [
                IntOp::Add,
                IntOp::Sub,
                IntOp::And,
                IntOp::Or,
                IntOp::Xor,
                IntOp::Shl,
                IntOp::Shr,
                IntOp::Sar,
                IntOp::Mul,
                IntOp::MulHighSigned,
                IntOp::MulHighUnsigned,
            ] {
                let (mut f, b, p, _) = start(&[ty, ty]);
                let v = f.push(b, Op::Int(op, p[0], p[1])).unwrap();
                finish(&mut f, b, v);
                for (a, b) in [
                    (0, 0),
                    (0xffff_ffff, 37),
                    (0x8000_0000, 31),
                    (0x8000_0000_0000_0000, 63),
                    (u64::MAX, u64::MAX),
                ] {
                    let mask = if ty == Type::I32 {
                        u32::MAX as u128
                    } else {
                        u64::MAX as u128
                    };
                    compare(&f, &[u128::from(a) & mask, u128::from(b) & mask], &mut h);
                }
            }
            for op in [
                IntUnary::Not,
                IntUnary::Neg,
                IntUnary::ZeroExtend8,
                IntUnary::ZeroExtend16,
                IntUnary::SignExtend8,
                IntUnary::SignExtend16,
                IntUnary::Low32,
                IntUnary::High32,
                IntUnary::Widen,
            ] {
                let (mut f, b, p, _) = start(&[ty]);
                let v = f.push(b, Op::IntUnary(op, p[0])).unwrap();
                finish(&mut f, b, v);
                compare(
                    &f,
                    &[if ty == Type::I32 {
                        0xdead_ba98
                    } else {
                        0x1234_5678_dead_ba98
                    }],
                    &mut h,
                );
            }
        }
        for signed in [false, true] {
            for remainder in [false, true] {
                for proved in [false, true] {
                    let (mut f, b, p, _) = start(&[Type::I64, Type::I32]);
                    let v = f
                        .push(
                            b,
                            Op::Div {
                                signed,
                                remainder,
                                dividend: p[0],
                                divisor: p[1],
                                proved,
                            },
                        )
                        .unwrap();
                    finish(&mut f, b, v);
                    for (n, d) in [(17u64, 3u32), ((-17i64) as u64, 3), (u64::MAX, 0xffff_ffff)] {
                        if proved && (!signed && n / u64::from(d) > u64::from(u32::MAX)) {
                            continue;
                        }
                        compare(&f, &[u128::from(n), u128::from(d)], &mut h);
                    }
                }
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_floating_vector_bits_and_select_match_reference() {
        let mut h = harness();
        for op in [FpOp::Add, FpOp::Sub, FpOp::Mul, FpOp::Div] {
            let (mut f, b, p, _) = start(&[Type::F64, Type::F64]);
            let v = f.push(b, Op::Fp(op, p[0], p[1])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for (a, b) in [
                (1.5f64, -2.0f64),
                (0.0, -0.0),
                (f64::INFINITY, 1.0),
                (f64::from_bits(0x7ff8_0000_0000_0123), 1.0),
            ] {
                compare(
                    &f,
                    &[u128::from(a.to_bits()), u128::from(b.to_bits())],
                    &mut h,
                );
            }
        }
        for op in [
            FpUnary::Neg,
            FpUnary::Abs,
            FpUnary::RoundZero,
            FpUnary::RoundDown,
        ] {
            let (mut f, b, p, _) = start(&[Type::F64]);
            let v = f.push(b, Op::FpUnary(op, p[0])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            compare(&f, &[u128::from((-2.7f64).to_bits())], &mut h);
        }
        for ty in [Type::I32, Type::I64] {
            let (mut f, b, p, _) = start(&[ty]);
            let v = f.push(b, Op::FromInt(p[0])).unwrap();
            finish(&mut f, b, v);
            for n in [
                0,
                17,
                0xffff_ffff,
                1 << 53,
                (1 << 53) + 1,
                i64::MAX as u128,
                i64::MIN as u64 as u128,
            ] {
                compare(
                    &f,
                    &[if ty == Type::I32 {
                        n & u128::from(u32::MAX)
                    } else {
                        n
                    }],
                    &mut h,
                );
            }
        }
        for wide in [false, true] {
            let (mut f, b, p, _) = start(&[Type::F64]);
            let v = f.push(b, Op::ToInt { value: p[0], wide }).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for x in [12.7, -8.9, f64::INFINITY, f64::NAN, 1e100] {
                compare(&f, &[u128::from(x.to_bits())], &mut h);
            }
        }
        for op in [
            VecOp::And,
            VecOp::AndNot,
            VecOp::Or,
            VecOp::Xor,
            VecOp::SubI32,
            VecOp::ShlI64,
            VecOp::ShrI64,
            VecOp::LessF64,
        ] {
            let (mut f, b, p, _) = start(&[Type::V128, Type::V128]);
            let v = f.push(b, Op::Vec(op, p[0], p[1])).unwrap();
            f.push(b, Op::Validate);
            finish(&mut f, b, v);
            for count in [0, 1, 31, 63, 64, 256, u64::MAX] {
                compare(
                    &f,
                    &[0x3ff0_0000_0000_0000_ffff_0000_1234_5678, u128::from(count)],
                    &mut h,
                );
            }
        }
        for from in [Type::I32, Type::I64, Type::F64, Type::V128] {
            for to in [Type::I32, Type::I64, Type::F64, Type::V128] {
                let (mut f, b, p, _) = start(&[from]);
                let v = f.push(b, Op::Bits { to, value: p[0] }).unwrap();
                finish(&mut f, b, v);
                let bits = match from {
                    Type::I32 => 123,
                    Type::V128 => 0x4020_0000_0000_0000_4030_0000_0000_0000,
                    _ => 0x4030_0000_0000_0000,
                };
                compare(&f, &[bits], &mut h);
            }
            let (mut f, b, p, _) = start(&[Type::I32, from, from]);
            let v = f
                .push(
                    b,
                    Op::Select {
                        cond: p[0],
                        a: p[1],
                        b: p[2],
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for cond in [0, 1] {
                compare(&f, &[cond, 123, 456], &mut h);
            }
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_flags_and_conditions_match_reference() {
        use iced_x86::ConditionCode as CC;
        let mut h = harness();
        for width in [1, 2, 4] {
            for kind in [
                FlagKind::Add,
                FlagKind::AddCarry,
                FlagKind::Sub,
                FlagKind::SubBorrow,
                FlagKind::Logic,
                FlagKind::Increment,
                FlagKind::Decrement,
                FlagKind::Negate,
                FlagKind::Shl,
                FlagKind::Shr,
                FlagKind::Sar,
                FlagKind::MulSigned,
            ] {
                let (mut f, b, p, _) = start(&[Type::I32, Type::I32, Type::Flags]);
                let v = f
                    .push(
                        b,
                        Op::Flags {
                            kind,
                            width,
                            a: p[0],
                            b: p[1],
                            prior: Some(p[2]),
                        },
                    )
                    .unwrap();
                finish(&mut f, b, v);
                for (x, y) in [
                    (0, 0),
                    (0x7f, 1),
                    (0xff, 1),
                    (0x7fff_ffff, 1),
                    (0x8000_0000, 1),
                    (0x1234_abcd, 7),
                    (0xffff_ffff, 8),
                    (0xa5a5_4321, 16),
                    (0x8000_0001, 31),
                    (0xffff_ffff, 32),
                    (0x1234_5678, 33),
                ] {
                    for prior in [0x202, 0xed7] {
                        compare(&f, &[x, y, prior], &mut h);
                    }
                }
            }
        }
        for cc in [
            CC::o,
            CC::no,
            CC::b,
            CC::ae,
            CC::e,
            CC::ne,
            CC::be,
            CC::a,
            CC::s,
            CC::ns,
            CC::p,
            CC::np,
            CC::l,
            CC::ge,
            CC::le,
            CC::g,
        ] {
            let (mut f, b, p, _) = start(&[Type::Flags]);
            let v = f
                .push(
                    b,
                    Op::Test {
                        flags: p[0],
                        cond: cc,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for flags in 0..64 {
                let bits = [0, 2, 4, 6, 7, 11]
                    .iter()
                    .enumerate()
                    .fold(0x202, |v, (i, b)| v | (flags >> i & 1) << b);
                compare(&f, &[bits], &mut h);
            }
        }
        let (mut f, b, p, _) = start(&[Type::F64, Type::F64, Type::Flags]);
        let v = f
            .push(
                b,
                Op::Flags {
                    kind: FlagKind::UnorderedCompare,
                    width: 8,
                    a: p[0],
                    b: p[1],
                    prior: Some(p[2]),
                },
            )
            .unwrap();
        f.push(b, Op::Validate);
        finish(&mut f, b, v);
        for (x, y) in [
            (1.0f64, 2.0f64),
            (2.0, 1.0),
            (0.0, -0.0),
            (f64::NAN, 1.0),
            (f64::from_bits(0x7ff0_0000_0000_0001), 0.0),
        ] {
            compare(
                &f,
                &[u128::from(x.to_bits()), u128::from(y.to_bits()), 0xed7],
                &mut h,
            );
        }
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_failures_recover_the_region_and_restore_fpcr() {
        let mut h = harness();
        let (mut f, b, p, entry) = start(&[Type::F64, Type::F64]);
        f.records[entry.0 as usize].x87.valid = 3;
        f.records[entry.0 as usize].x87.stack[0] = Some(Location::At(p[0]));
        f.records[entry.0 as usize].x87.stack[1] = Some(Location::At(p[1]));
        let v = f.push(b, Op::Fp(FpOp::Mul, p[0], p[1])).unwrap();
        f.push(b, Op::Validate);
        finish(&mut f, b, v);
        for (x, y) in [
            (1e300f64, 1e300f64),
            (1e-200, 1e-200),
            (f64::from_bits(1), 1.0),
            (f64::MIN_POSITIVE, 0.5),
        ] {
            assert_eq!(
                compare(
                    &f,
                    &[u128::from(x.to_bits()), u128::from(y.to_bits())],
                    &mut h
                )
                .record,
                entry
            );
        }
        for denormal in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I64]);
            f.records[entry.0 as usize]
                .frame
                .push((4, 8, Location::At(p[0])));
            let v = f
                .push(
                    b,
                    Op::X87Load {
                        bits: p[0],
                        denormal,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for x in [
                f64::NAN,
                f64::from_bits(0x7ff0_0000_0000_0001),
                f64::from_bits(1),
                -0.0,
                f64::INFINITY,
                3.5,
            ] {
                compare(&f, &[u128::from(x.to_bits())], &mut h);
            }
        }
        for signed in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I64, Type::I32]);
            f.records[entry.0 as usize].regs[0] = Some(Location::At(p[1]));
            let v = f
                .push(
                    b,
                    Op::Div {
                        signed,
                        remainder: false,
                        dividend: p[0],
                        divisor: p[1],
                        proved: false,
                    },
                )
                .unwrap();
            finish(&mut f, b, v);
            for (n, d) in [(123, 0), (u64::MAX, 1), (i64::MIN as u64, u32::MAX)] {
                compare(&f, &[u128::from(n), u128::from(d)], &mut h);
            }
        }
    }

    // spec: A64-OPT-4, A64-OPT-6, A64-OPT-7, A64-MEM-10
    #[test]
    fn opt_back_memory_and_fault_records_use_the_private_alias() {
        let mut h = harness();
        for (width, ty) in [
            (1, Type::I32),
            (1, Type::I64),
            (2, Type::I32),
            (2, Type::I64),
            (4, Type::I32),
            (4, Type::I64),
            (8, Type::I64),
            (16, Type::V128),
        ] {
            let (mut f, b, p, _) = start(&[Type::I32, ty]);
            let after = f.record(record(0x1010));
            f.push(
                b,
                Op::Store {
                    width,
                    addr: p[0],
                    value: p[1],
                    after,
                },
            );
            let loaded = f
                .push(
                    b,
                    Op::Load {
                        ty,
                        width,
                        addr: p[0],
                    },
                )
                .unwrap();
            finish(&mut f, b, loaded);
            let input = 0x1234_5678_9abc_def0_ffff_eeee_dddd_cccc;
            compare(&f, &[u128::from(DATA), input], &mut h);
            assert_eq!(
                h.emu.read(WINDOW_BASE + u64::from(DATA), width as usize),
                input.to_le_bytes()[..width as usize]
            );
        }
        for store in [false, true] {
            let (mut f, b, p, entry) = start(&[Type::I32, Type::I64]);
            f.records[entry.0 as usize].regs[7] = Some(Location::At(p[0]));
            f.records[entry.0 as usize]
                .frame
                .push((-8, 8, Location::At(p[1])));
            let value = if store {
                let after = f.record(record(0x1010));
                f.push(
                    b,
                    Op::Store {
                        width: 8,
                        addr: p[0],
                        value: p[1],
                        after,
                    },
                );
                p[1]
            } else {
                f.push(
                    b,
                    Op::Load {
                        ty: Type::I64,
                        width: 8,
                        addr: p[0],
                    },
                )
                .unwrap()
            };
            finish(&mut f, b, value);
            let got = compare(&f, &[0x80000, 0xdead_beef], &mut h);
            assert_eq!(got.record, entry);
            assert_eq!(got.lowered.sites[0].1.opt_record, Some(entry.0));
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_loop_parameters_countdown_and_head_exit() {
        let (mut f, head, p, entry) = start(&[Type::I32, Type::I32]);
        f.records[entry.0 as usize].regs[0] = Some(Location::At(p[0]));
        f.records[entry.0 as usize].regs[1] = Some(Location::At(p[1]));
        f.records[entry.0 as usize].refund = 3;
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, p[0], one)).unwrap();
        let total = f.push(head, Op::Int(IntOp::Add, p[1], p[0])).unwrap();
        let body = f.block(None);
        let done = f.block(None);
        f.end(
            head,
            End::Branch {
                cond: next,
                taken: Target {
                    block: body,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        f.end(
            body,
            End::Jump(Target {
                block: head,
                args: vec![next, total],
                charge: 3,
            }),
        );
        finish(&mut f, done, total);
        let mut h = harness();
        let full = compare(&f, &[5, 0], &mut h);
        assert_eq!(full.cpu.regs[0], 15);
        assert_eq!(full.countdown, 10000 - 15);
        for recall in [1, 2] {
            let cut = h.run_opt(&f, &[5, 0], 7, recall, 0);
            assert_eq!(cut.record, entry);
            assert_eq!(cut.cpu.regs[0..2], [3, 9]);
            assert_eq!(7u64.wrapping_sub(cut.countdown), 6);
        }
        f.head_exit = Some(head);
        let head_stop = h.run_opt(&f, &[5, 0], 100, 0, 2);
        assert_eq!(head_stop.record, entry);
        assert_eq!(head_stop.cpu.regs[0..2], [3, 9]);
        assert_eq!(head_stop.countdown, 94);
        f.head_exit = None;
        let refill = h.run_opt(&f, &[5, 0], 1, 0, 0);
        assert_eq!(refill.cpu.regs[0], 15);
        assert_ne!(refill.completed, 0);
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_spills_cold_values_and_c1_recipes() {
        let mut h = harness();
        let (mut f, b, p, entry) = start(&[Type::I64; 36]);
        for (i, &value) in p.iter().take(8).enumerate() {
            f.records[entry.0 as usize].regs[i] = Some(Location::At(value));
        }
        let mut result = p[0];
        for &v in &p[1..] {
            result = f.push(b, Op::Int(IntOp::Add, result, v)).unwrap();
        }
        finish(&mut f, b, result);
        let params: Vec<_> = (1..=36).collect();
        let got = compare(&f, &params, &mut h);
        assert_eq!(got.cpu.regs[0], 666);
        assert!(got.lowered.spill_cells > 0);
        assert!(got.spills.iter().any(|&v| v > 0));
        // Check a failure while all entry values must remain recoverable.
        let (mut f, b, p, entry) = start(&[Type::I64; 36]);
        f.records[entry.0 as usize].frame = p
            .iter()
            .enumerate()
            .map(|(i, &p)| (i as i32 * 8, 8, Location::At(p)))
            .collect();
        let zero = f.push(b, Op::Constant(Type::I32, 0)).unwrap();
        let v = f
            .push(
                b,
                Op::Div {
                    signed: false,
                    remainder: false,
                    dividend: p[0],
                    divisor: zero,
                    proved: false,
                },
            )
            .unwrap();
        finish(&mut f, b, v);
        compare(&f, &params, &mut h);
        let (mut f, b, p, _) = start(&[Type::F64, Type::F64]);
        let v = f.push(b, Op::Fp(FpOp::Div, p[0], p[1])).unwrap();
        f.push(b, Op::Validate);
        let exit = finish(&mut f, b, v);
        f.records[exit.0 as usize].x87.c1 = Some(Location::Recipe(Recipe::C1 {
            op: FpOp::Div,
            a: p[0],
            b: p[1],
        }));
        f.records[exit.0 as usize].x87.top = 3;
        let got = compare(
            &f,
            &[u128::from(1f64.to_bits()), u128::from(10f64.to_bits())],
            &mut h,
        );
        assert_eq!(got.cpu.x87.status >> 9 & 1, 1);
        assert_eq!(got.registers.x[28], WINDOW_BASE);
        assert_eq!(got.registers.x[29], CONTEXT);
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_edges_refund_and_copy_parameters_at_a_nonentry_head() {
        let (mut f, entry, p, _) = start(&[Type::I32, Type::I32, Type::I32]);
        let mut r = record(0x1100);
        r.refund = 3;
        let head_record = f.record(r);
        let head = f.block(Some(Head {
            record: head_record,
        }));
        let n = f.param(head, Type::I32);
        let x = f.param(head, Type::I32);
        let y = f.param(head, Type::I32);
        f.records[head_record.0 as usize].regs[0] = Some(Location::At(n));
        f.records[head_record.0 as usize].regs[1] = Some(Location::At(x));
        f.records[head_record.0 as usize].regs[2] = Some(Location::At(y));
        f.records[head_record.0 as usize].x87.ip = Some(Location::At(x));
        f.records[head_record.0 as usize].x87.opcode = Some(Location::At(y));
        f.end(
            entry,
            End::Jump(Target {
                block: head,
                args: p.clone(),
                charge: 3,
            }),
        );
        let one = f.push(head, Op::Constant(Type::I32, 1)).unwrap();
        let next = f.push(head, Op::Int(IntOp::Sub, n, one)).unwrap();
        let flags = f
            .push(
                head,
                Op::Flags {
                    kind: FlagKind::Sub,
                    width: 4,
                    a: n,
                    b: one,
                    prior: None,
                },
            )
            .unwrap();
        let cond = f
            .push(
                head,
                Op::Test {
                    flags,
                    cond: iced_x86::ConditionCode::a,
                },
            )
            .unwrap();
        let done = f.block(None);
        f.end(
            head,
            End::Branch {
                cond,
                taken: Target {
                    block: head,
                    args: vec![next, y, x],
                    charge: 3,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: -2,
                },
            },
        );
        let result = f.push(done, Op::Int(IntOp::Add, x, y)).unwrap();
        finish(&mut f, done, result);
        let mut h = harness();
        compare(&f, &[3, 0x1234, 0x5678], &mut h);
        f.head_exit = Some(head);
        let cut = h.run_opt(&f, &[3, 0x1234, 0x5678], 100, 0, 2);
        assert_eq!(cut.record, head_record);
        assert_eq!(cut.cpu.regs[0..3], [2, 0x5678, 0x1234]);
        assert_eq!(cut.cpu.x87.ip, 0x5678);
        assert_eq!(cut.cpu.x87.opcode, 0x1234);
        assert_eq!(100 - cut.countdown, 3);
        let deferred = h.run_opt(&f, &[3, 0x1234, 0x5678], 1, 1, 0);
        assert_eq!(deferred.record, head_record);
        assert_eq!(deferred.cpu.regs[0..3], [2, 0x5678, 0x1234]);
    }

    // spec: A64-OPT-4, A64-OPT-6
    #[test]
    fn opt_back_failure_after_store_recovers_the_new_region() {
        let (mut f, b, p, _) = start(&[Type::I32, Type::I64, Type::F64, Type::F64]);
        let mut r = record(0x1010);
        r.regs[1] = Some(Location::At(p[1]));
        r.frame.push((8, 8, Location::At(p[1])));
        let after = f.record(r);
        f.push(
            b,
            Op::Store {
                width: 8,
                addr: p[0],
                value: p[1],
                after,
            },
        );
        let v = f.push(b, Op::Fp(FpOp::Mul, p[2], p[3])).unwrap();
        f.push(b, Op::Validate);
        finish(&mut f, b, v);
        let mut h = harness();
        let got = compare(
            &f,
            &[
                u128::from(DATA),
                0x1234_abcd,
                u128::from(1e300f64.to_bits()),
                u128::from(1e300f64.to_bits()),
            ],
            &mut h,
        );
        assert_eq!(got.record, after);
        assert_eq!(
            h.emu.read(WINDOW_BASE + u64::from(DATA), 8),
            0x1234_abcdu64.to_le_bytes()
        );
    }
    // spec: A64-OPT-4, A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_lazy_flag_tests_and_records_cover_every_condition() {
        use iced_x86::ConditionCode as CC;
        let conditions = [
            CC::o,
            CC::no,
            CC::b,
            CC::ae,
            CC::e,
            CC::ne,
            CC::be,
            CC::a,
            CC::s,
            CC::ns,
            CC::p,
            CC::np,
            CC::l,
            CC::ge,
            CC::le,
            CC::g,
        ];
        let kinds = [
            FlagKind::Add,
            FlagKind::AddCarry,
            FlagKind::Sub,
            FlagKind::SubBorrow,
            FlagKind::Logic,
            FlagKind::Increment,
            FlagKind::Decrement,
            FlagKind::Negate,
            FlagKind::Shl,
            FlagKind::Shr,
            FlagKind::Sar,
            FlagKind::MulSigned,
            FlagKind::UnorderedCompare,
        ];
        let mut h = harness();
        for kind in kinds {
            for width in [1, 2, 4] {
                for cc in conditions {
                    let floating = kind == FlagKind::UnorderedCompare;
                    let ty = if floating { Type::F64 } else { Type::I32 };
                    let (mut f, b, p, _) = start(&[ty, ty, Type::Flags]);
                    let flags = f
                        .push(
                            b,
                            Op::Flags {
                                kind,
                                width,
                                a: p[0],
                                b: p[1],
                                prior: Some(p[2]),
                            },
                        )
                        .unwrap();
                    let test = f.push(b, Op::Test { flags, cond: cc }).unwrap();
                    if floating {
                        f.push(b, Op::Validate);
                    }
                    let exit = finish(&mut f, b, test);
                    f.records[exit.0 as usize].eflags =
                        Some(Location::Recipe(Recipe::Eflags(flags)));
                    let inputs: Vec<(u128, u128)> = if floating {
                        [
                            (1.0f64, 2.0f64),
                            (2.0, 1.0),
                            (0.0, -0.0),
                            (f64::NAN, 1.0),
                            (f64::from_bits(0x7ff0_0000_0000_0001), 0.0),
                            (f64::from_bits(1), 0.0),
                        ]
                        .map(|(a, b)| (u128::from(a.to_bits()), u128::from(b.to_bits())))
                        .to_vec()
                    } else {
                        vec![
                            (0, 0),
                            (0x7f, 0),
                            (2, 1),
                            (0xff, 0),
                            (0xff, 1),
                            (0x7fff, 1),
                            (0x7fff_ffff, 1),
                            (0x8000_0000, 1),
                            (0xffff_ffff, 1),
                            (0xa5a5_4321, 2),
                            (0x1234_abcd, 7),
                            (0xffff_ffff, 8),
                            (0x8001, 9),
                            (0xa5a5_4321, 16),
                            (0x8000_0001, 17),
                            (0x8000_0001, 31),
                            (0xffff_ffff, 32),
                            (0x1234_5678, 33),
                        ]
                    };
                    for (x, y) in inputs {
                        for prior in [0x202, 0xed7] {
                            let params = [x, y, prior];
                            let run = eval::run(&f, &params, &mut memory(&[], &[]), 10000);
                            let got = h.run_opt(&f, &params, 10000, 0, 0);
                            let expected = match run.outcome {
                                eval::Outcome::Exit(r) | eval::Outcome::Fail(r) => r,
                                _ => unreachable!(),
                            };
                            assert_eq!(
                                got.record, expected,
                                "{kind:?}/{width}/{cc:?}, {params:x?}"
                            );
                            if expected == exit {
                                assert_eq!(
                                    got.cpu.regs[0], run.values[test.0 as usize] as u32,
                                    "{kind:?}/{width}/{cc:?}, {params:x?}"
                                );
                            }
                            compare_record(&f, &run.values, &got);
                        }
                    }
                }
            }
        }
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_lazy_flags_read_only_the_prior_bits_the_operation_keeps() {
        use iced_x86::ConditionCode as CC;
        let mut h = harness();
        for kind in [
            FlagKind::Increment,
            FlagKind::Decrement,
            FlagKind::AddCarry,
            FlagKind::SubBorrow,
            FlagKind::Shl,
            FlagKind::Shr,
            FlagKind::Sar,
            FlagKind::MulSigned,
        ] {
            for cc in [
                CC::o,
                CC::no,
                CC::b,
                CC::ae,
                CC::e,
                CC::ne,
                CC::be,
                CC::a,
                CC::s,
                CC::ns,
                CC::p,
                CC::np,
                CC::l,
                CC::ge,
                CC::le,
                CC::g,
            ] {
                let (mut f, b, p, _) = start(&[Type::I32, Type::I32, Type::Flags]);
                let prior = f
                    .push(
                        b,
                        Op::Flags {
                            kind: FlagKind::Sub,
                            width: 4,
                            a: p[1],
                            b: p[0],
                            prior: Some(p[2]),
                        },
                    )
                    .unwrap();
                let flags = f
                    .push(
                        b,
                        Op::Flags {
                            kind,
                            width: 1,
                            a: p[0],
                            b: p[1],
                            prior: Some(prior),
                        },
                    )
                    .unwrap();
                let test = f.push(b, Op::Test { flags, cond: cc }).unwrap();
                let exit = finish(&mut f, b, test);
                f.records[exit.0 as usize].eflags = Some(Location::Recipe(Recipe::Eflags(flags)));
                for params in [
                    [0xff, 0, 0x202],
                    [0x80, 1, 0xed7],
                    [0xff, 9, 0x202],
                    [0x7f, 31, 0xed7],
                ] {
                    compare(&f, &params, &mut h);
                }
            }
        }
    }

    fn body(lowered: &back::Lowered, block: usize) -> &[u32] {
        let start = lowered.marks[block + 1].offset as usize / 4;
        let end = lowered.marks[block + 2].offset as usize / 4;
        &lowered.words[start..end]
    }

    // spec: A64-OPT-4, A64-OPT-7
    #[test]
    fn opt_back_emitted_word_counts_keep_common_operations_lean() {
        let stubs = runtime::stubs();
        let (mut f, b, p, _) = start(&[Type::I32]);
        let k = f.push(b, Op::Constant(Type::I32, 7)).unwrap();
        let v = f.push(b, Op::Int(IntOp::Add, p[0], k)).unwrap();
        finish(&mut f, b, v);
        let lower = back::lower(&f, 0, &stubs);
        assert_eq!(body(&lower, 0).len(), 2, "one ADD plus exit branch");
        assert_eq!(
            body(&lower, 0)[0],
            add_imm(W, Reg::new(0), Reg::new(0), 7, false)
        );

        let (mut f, b, p, _) = start(&[Type::I32]);
        let k = f.push(b, Op::Constant(Type::I32, 5)).unwrap();
        let flags = f
            .push(
                b,
                Op::Flags {
                    kind: FlagKind::Sub,
                    width: 4,
                    a: p[0],
                    b: k,
                    prior: None,
                },
            )
            .unwrap();
        // The producer need not be adjacent to the Test.
        f.push(b, Op::Constant(Type::I32, 123));
        let test = f
            .push(
                b,
                Op::Test {
                    flags,
                    cond: iced_x86::ConditionCode::a,
                },
            )
            .unwrap();
        let fall = f.block(None);
        let taken = f.block(None);
        f.end(
            b,
            End::Branch {
                cond: test,
                taken: Target {
                    block: taken,
                    args: Vec::new(),
                    charge: 0,
                },
                fall: Target {
                    block: fall,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        for block in [fall, taken] {
            let exit = f.record(record(0x2000));
            f.end(block, End::Exit(exit));
        }
        let lower = back::lower(&f, 0, &stubs);
        assert_eq!(body(&lower, 0).len(), 2, "CMP plus conditional branch");
        assert_eq!(body(&lower, 0)[0], cmp_imm(W, Reg::new(0), 5, false));
        assert_eq!(body(&lower, 0)[1] & 0xff00_001f, b_cond(Cond::Hi, 0));

        let (mut f, b, _, entry) = start(&[]);
        f.records[entry.0 as usize].refund = 3;
        f.push(b, Op::Validate);
        let exit = f.record(record(0x2000));
        f.end(b, End::Exit(exit));
        let lower = back::lower(&f, 0, &stubs);
        let start = lower.marks[1].offset as usize / 4;
        assert_eq!(lower.words[start - 2], sub_imm(X, X9, X9, 3, false));
        assert_eq!(lower.words[start - 1] & 0xfff8_001f, tbz(true, X9, 63, 0));
        assert_eq!(
            body(&lower, 0).len(),
            4,
            "three-word Validate plus exit branch"
        );
        assert_eq!(
            body(&lower, 0)[..2],
            [mrs_fpsr(X16), and_imm(W, X16, X16, 0xffff_ff8f)]
        );
        assert_eq!(body(&lower, 0)[2] & 0xff00_001f, cbz(W, true, X16, 0));
    }

    // spec: A64-OPT-4, A64-OPT-7
    #[test]
    fn opt_back_counted_floating_loop_has_no_backedge_or_arithmetic_moves() {
        let (mut f, entry, p, _) = start(&[Type::F64, Type::F64, Type::I32]);
        let head_record = f.record(record(0x1100));
        let head = f.block(Some(Head {
            record: head_record,
        }));
        let sum = f.param(head, Type::F64);
        let count = f.param(head, Type::I32);
        f.end(
            entry,
            End::Jump(Target {
                block: head,
                args: vec![p[0], p[2]],
                charge: 0,
            }),
        );
        let one = f.push(entry, Op::Constant(Type::I32, 1)).unwrap();
        let total = f.push(head, Op::Fp(FpOp::Add, sum, p[1])).unwrap();
        f.push(head, Op::Validate);
        let next = f.push(head, Op::Int(IntOp::Sub, count, one)).unwrap();
        let done = f.block(None);
        f.end(
            head,
            End::Branch {
                cond: next,
                taken: Target {
                    block: head,
                    args: vec![total, next],
                    charge: 3,
                },
                fall: Target {
                    block: done,
                    args: Vec::new(),
                    charge: 0,
                },
            },
        );
        finish(&mut f, done, total);
        let lower = back::lower(&f, 0, &runtime::stubs());
        let head_code = body(&lower, 1);
        assert_eq!(
            head_code[0],
            dfloat(Float::Fadd, VReg::new(0), VReg::new(0), VReg::new(1))
        );
        assert_eq!(head_code.len(), 7, "FADD, Validate, SUB, CMP, branch");
        // After the done block's exit branch the taken edge contains only its
        // two-word charge and branch; no register or memory copies.
        let edge = lower.marks[3].offset as usize / 4 + 1;
        assert_eq!(lower.words[edge], sub_imm(X, X9, X9, 3, false));
        assert_eq!(lower.words[edge + 1] & 0xfff8_001f, tbz(true, X9, 63, 0));
        assert_eq!(lower.words[edge + 2] & 0xfc00_0000, crate::encode::b(0));
        let mut h = harness();
        compare(
            &f,
            &[u128::from(1f64.to_bits()), u128::from(0.5f64.to_bits()), 4],
            &mut h,
        );
    }

    // spec: A64-OPT-6, A64-OPT-7
    #[test]
    fn opt_back_integer_immediates_and_allocated_constants_match_reference() {
        let mut h = harness();
        for ty in [Type::I32, Type::I64] {
            for op in [
                IntOp::Add,
                IntOp::Sub,
                IntOp::And,
                IntOp::Or,
                IntOp::Xor,
                IntOp::Shl,
                IntOp::Shr,
                IntOp::Sar,
                IntOp::Mul,
            ] {
                for bits in [0, 7, 0x1000, 0xffff0000, 0x12345678, 0x123456789abcdef0] {
                    let bits = if ty == Type::I32 {
                        bits & 0xffffffff
                    } else {
                        bits
                    };
                    let (mut f, b, p, _) = start(&[ty]);
                    let k = f.push(b, Op::Constant(ty, bits)).unwrap();
                    let v = f.push(b, Op::Int(op, p[0], k)).unwrap();
                    finish(&mut f, b, v);
                    compare(&f, &[0x98765432], &mut h);
                }
            }
        }
    }
}
