use std::ptr::null_mut;
use std::time::{Duration, Instant};

use adv_cpu_core::memory::{EXEC, PAGE_SIZE, READ, SHARED, WRITE};

use super::{BASE, CODE, CpuState, DATA, Executor, LEN, Memory, Rng, RunOutcome, STACK};
use super::{BlockEncoderOptions, alu_imm};
use crate::native::{Checks, Run};
use crate::runtime::Exit;
use iced_x86::code_asm::*;

const HOST_PAGE: usize = 0x4000;
const SPECIAL: u32 = 0x2_0000;
const OUTPUT: u32 = DATA + 0x1000;
const RESULTS: u32 = DATA + 0x3000;
const FRAME: u32 = STACK + 0x2000;

struct Image {
    code: Vec<u8>,
    head: u32,
    stop: u32,
    end: u32,
}

impl Image {
    fn loop_body(build: impl FnOnce(&mut CodeAssembler, CodeLabel, CodeLabel)) -> Self {
        Self::assemble(build, |_| {})
    }

    fn assemble(
        build: impl FnOnce(&mut CodeAssembler, CodeLabel, CodeLabel),
        finish: impl FnOnce(&mut CodeAssembler),
    ) -> Self {
        Self::with_prelude(|a, _| a.nop().unwrap(), build, finish)
    }

    fn with_prelude(
        prelude: impl FnOnce(&mut CodeAssembler, CodeLabel),
        build: impl FnOnce(&mut CodeAssembler, CodeLabel, CodeLabel),
        finish: impl FnOnce(&mut CodeAssembler),
    ) -> Self {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        let mut stop = a.create_label();
        // Start outside the loop so the form is reached from Tier 1.
        prelude(&mut a, stop);
        a.set_label(&mut head).unwrap();
        build(&mut a, head, stop);
        a.set_label(&mut stop).unwrap();
        finish(&mut a);
        a.int3().unwrap();
        let result = a
            .assemble_options(
                u64::from(CODE),
                BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
            )
            .unwrap();
        Self {
            head: result.label_ip(&head).unwrap() as u32,
            stop: result.label_ip(&stop).unwrap() as u32,
            end: CODE + result.inner.code_buffer.len() as u32 - 1,
            code: result.inner.code_buffer,
        }
    }
}

fn latch(a: &mut CodeAssembler, head: CodeLabel) {
    a.dec(ecx).unwrap();
    a.jne(head).unwrap();
}

fn initial(count: u32) -> CpuState {
    CpuState {
        eip: CODE,
        regs: [0, count, 0, 0, FRAME, 0, DATA, OUTPUT],
        ..CpuState::default()
    }
}

struct Trial<'a> {
    live: Executor,
    reference: Executor,
    stop: u32,
    end: u32,
    head: u32,
    extra_memory: &'a [(u32, usize)],
    context: String,
}

impl<'a> Trial<'a> {
    fn new(
        image: &Image,
        state: &CpuState,
        context: impl Into<String>,
        extra_memory: &'a [(u32, usize)],
        setup: impl Fn(&mut Memory),
    ) -> Self {
        let make = |translated| {
            let mut memory = Memory::window_at_host_size(BASE, LEN, HOST_PAGE).unwrap();
            memory.map(CODE, HOST_PAGE, READ | WRITE).unwrap();
            memory.write(CODE, &image.code).unwrap();
            memory.protect(CODE, HOST_PAGE, READ | EXEC).unwrap();
            memory.map(DATA, HOST_PAGE, READ | WRITE).unwrap();
            memory.map(STACK, HOST_PAGE, READ | WRITE).unwrap();
            setup(&mut memory);
            let mut cpu = Executor::create(memory, translated).unwrap();
            cpu.state = state.clone();
            if let Some(native) = &mut cpu.native {
                native.first_visits = false;
                native.promotion = 0;
            }
            cpu
        };
        Self {
            live: make(true),
            reference: make(false),
            stop: image.stop,
            end: image.end,
            head: image.head,
            extra_memory,
            context: context.into(),
        }
    }

    fn try_install(&mut self) -> bool {
        let native = self.live.native.as_mut().unwrap();
        let memory = &mut self.live.memory;
        memory.attach().unwrap();
        // Register the stop before installing the form. New stops can cut units.
        native
            .run(&mut self.live.state, memory, self.stop, 0, null_mut())
            .unwrap();
        native.force_resident(memory, self.head)
    }

    fn install(&mut self) {
        assert!(
            self.try_install(),
            "{}: no resident form at {:#x}",
            self.context,
            self.head,
        );
    }

    fn same_state(&self) {
        let mut live = self.live.state.clone();
        let mut reference = self.reference.state.clone();
        for cpu in [&mut live, &mut reference] {
            cpu.settle_flags();
            cpu.refresh_mmx();
        }
        let context = &self.context;
        assert_eq!(
            live.regs, reference.regs,
            "{context}: GPRs at {:#x}",
            live.eip
        );
        assert_eq!(
            live.eflags, reference.eflags,
            "{context}: EFLAGS at {:#x}",
            live.eip
        );
        assert_eq!(live.x87, reference.x87, "{context}: x87 at {:#x}", live.eip);
        assert_eq!(live.xmm, reference.xmm, "{context}: XMM at {:#x}", live.eip);
        assert_eq!(
            live.mxcsr, reference.mxcsr,
            "{context}: MXCSR at {:#x}",
            live.eip
        );
        assert_eq!(live.mmx, reference.mmx, "{context}: MMX at {:#x}", live.eip);
        assert_eq!(
            live.fault_addr, reference.fault_addr,
            "{context}: fault address"
        );
    }

    fn same_position_and_memory(&self, retired: u64, terminal: bool) {
        let context = &self.context;
        assert_eq!(
            self.live.state.eip, self.reference.state.eip,
            "{context}: EIP after {retired}"
        );
        // Frame cells may be dead at a checkpoint (A64-OPT-3).
        let frame = terminal.then_some((STACK, HOST_PAGE));
        for (addr, len) in [(CODE, HOST_PAGE), (DATA, HOST_PAGE)]
            .into_iter()
            .chain(self.extra_memory.iter().copied())
            .chain(frame)
        {
            let live = self.live.memory.read_data(addr, len).unwrap();
            let reference = self.reference.memory.read_data(addr, len).unwrap();
            assert!(
                live == reference,
                "{context}: memory at {addr:#x} after {retired}"
            );
        }
    }

    fn run_native(&mut self, fuel: u64) -> Option<Run> {
        self.live.memory.attach().unwrap();
        self.live.state.settle_flags();
        self.live
            .native
            .as_mut()
            .unwrap()
            .run(
                &mut self.live.state,
                &mut self.live.memory,
                self.stop,
                fuel,
                null_mut(),
            )
            .unwrap()
    }

    /// Check every return from native code, then compare any instruction the
    /// owner must finish in Tier 0. A wrong retirement count advances the
    /// reference to a different instruction or a different loop iteration.
    /// Dead registers at checkpoints are tested by resuming from both states.
    fn compare(&mut self, fuel: u64) -> (RunOutcome, u64) {
        let mut retired = 0;
        for _ in 0..=fuel {
            let stop = self.stop;
            if self.live.state.eip == stop {
                self.same_state();
                return (RunOutcome::ReachedStop, retired);
            }
            if retired == fuel {
                return (RunOutcome::Budget, retired);
            }
            if let Some(run) = self.run_native(fuel - retired) {
                let units = u64::from(run.retired);
                assert!(
                    units <= fuel - retired,
                    "{}: native exceeded fuel",
                    self.context
                );
                let outcome = self.reference.run(stop, units).unwrap();
                assert!(
                    matches!(outcome, RunOutcome::ReachedStop | RunOutcome::Budget),
                    "{}: Tier 0 stopped before the native exit: {outcome:?}",
                    self.context,
                );
                assert_eq!(
                    self.reference.state.fuel, 0,
                    "{}: retired count at stop",
                    self.context
                );
                retired += units;
                self.same_position_and_memory(retired, self.live.state.eip == stop);
                match run.exit {
                    Exit::Dispatch | Exit::Budget => continue,
                    Exit::Interpret | Exit::Fault if retired == fuel => continue,
                    Exit::Interpret | Exit::Fault => {}
                    exit => panic!("{}: unexpected native exit {exit:?}", self.context),
                }
            }
            let outcome = self.live.run_interpreted(stop, 1).unwrap();
            let expected = self.reference.run(stop, 1).unwrap();
            assert_eq!(outcome, expected, "{}: Tier-0 completion", self.context);
            assert_eq!(
                self.live.state.fuel, self.reference.state.fuel,
                "{}: Tier-0 retired count",
                self.context
            );
            retired += 1 - self.live.state.fuel;
            self.same_position_and_memory(retired, outcome != RunOutcome::Budget);
            if outcome != RunOutcome::Budget {
                self.same_state();
                return (outcome, retired);
            }
        }
        panic!("{}: too many returns to the owner", self.context);
    }

    fn compare_epilogue(&mut self) {
        const FUEL: u64 = 64;
        for cpu in [&mut self.live, &mut self.reference] {
            assert_eq!(cpu.run(self.end, FUEL).unwrap(), RunOutcome::ReachedStop);
        }
        assert_eq!(
            self.live.state.fuel, self.reference.state.fuel,
            "{}: epilogue retired count",
            self.context
        );
        self.same_position_and_memory(FUEL - self.live.state.fuel, true);
        self.same_state();
    }
}

#[derive(Clone, Copy, Debug)]
enum InnerRole {
    ShiftCount,
    WordIndex,
}

fn nested_frame_loop(role: InnerRole, frame_offset: i32, locals: i32) -> Image {
    Image::with_prelude(
        |a, stop| {
            a.lea(ebp, esp + frame_offset).unwrap();
            a.mov(edx, dword_ptr(ebp - 8)).unwrap();
            a.xor(edi, edi).unwrap();
            a.test(edx, edx).unwrap();
            a.jle(stop).unwrap();
            a.mov(esi, dword_ptr(ebp - 12)).unwrap();
            a.mov(ecx, dword_ptr(ebp - 4)).unwrap();
        },
        |a, head, stop| {
            let mut inner = a.create_label();
            let mut reload = a.create_label();
            let mut join = a.create_label();
            a.mov(dl, byte_ptr(edi + ecx)).unwrap();
            a.xor(eax, eax).unwrap();
            a.xor(ecx, ecx).unwrap();
            a.movsx(ebx, dl).unwrap();
            a.test(dl, dl).unwrap();
            a.jle(reload).unwrap();
            a.set_label(&mut inner).unwrap();
            let word = match role {
                InnerRole::ShiftCount => dword_ptr(esi),
                InnerRole::WordIndex => dword_ptr(esi + ecx * 4),
            };
            a.mov(edx, word).unwrap();
            for local in 0..locals {
                a.xor(eax, dword_ptr(ebp + 32 + 4 * local)).unwrap();
            }
            a.add(eax, eax).unwrap();
            if matches!(role, InnerRole::ShiftCount) {
                a.shr(edx, cl).unwrap();
            }
            a.inc(ecx).unwrap();
            a.and(edx, 1).unwrap();
            a.or(eax, edx).unwrap();
            a.cmp(ecx, ebx).unwrap();
            a.jl(inner).unwrap();
            a.mov(ecx, dword_ptr(ebp - 4)).unwrap();
            a.mov(dl, byte_ptr(edi + ecx)).unwrap();
            a.jmp(join).unwrap();
            a.set_label(&mut reload).unwrap();
            a.mov(ecx, dword_ptr(ebp - 4)).unwrap();
            a.set_label(&mut join).unwrap();
            a.cmp(dword_ptr(ebp + 8), 0).unwrap();
            a.je(stop).unwrap();
            a.inc(edi).unwrap();
            a.cmp(edi, dword_ptr(ebp - 8)).unwrap();
            a.jl(head).unwrap();
        },
        observe_registers,
    )
}

fn nested_frame_trial(
    role: InnerRole,
    frame_offset: i32,
    table: &[u8],
    word: u32,
    soft_word: bool,
    context: impl Into<String>,
) -> Trial<'static> {
    // Keep more values live across the checked load than fit in registers.
    let locals = if soft_word { 18 } else { 0 };
    let image = nested_frame_loop(role, frame_offset, locals);
    let extra: &[_] = if soft_word {
        &[(SPECIAL, PAGE_SIZE)]
    } else {
        &[]
    };
    let mut trial = Trial::new(&image, &initial(0), context, extra, |m| {
        m.write(DATA, table).unwrap();
        let word_addr = if soft_word {
            soft_page(m, word);
            SPECIAL
        } else {
            let words: Vec<_> = (0..32)
                .flat_map(|i| word.rotate_left(i).to_le_bytes())
                .collect();
            m.write(DATA + 0x800, &words).unwrap();
            DATA + 0x800
        };
        let frame = FRAME + frame_offset as u32;
        for local in 0..locals as u32 {
            m.write(
                frame + 32 + 4 * local,
                &word.rotate_left(local).to_le_bytes(),
            )
            .unwrap();
        }
        for (offset, value) in [
            (-12, word_addr),
            (-8, table.len() as u32),
            (-4, DATA),
            (8, 1),
        ] {
            m.write(frame.wrapping_add_signed(offset), &value.to_le_bytes())
                .unwrap();
        }
    });
    if soft_word {
        trial.live.native.as_mut().unwrap().checks = Checks::All;
    }
    trial
}

fn enter_nested_loop(trial: &mut Trial<'_>, force: bool) {
    let initial = trial.live.state.clone();
    if force {
        assert_eq!(trial.compare(7), (RunOutcome::Budget, 7));
        assert_eq!(trial.live.state.eip, trial.head);
        trial.live.memory.attach().unwrap();
        assert!(
            trial
                .live
                .native
                .as_mut()
                .unwrap()
                .force_resident_at(&mut trial.live.memory, trial.live.state.clone(),)
        );
    } else {
        assert_eq!(trial.compare(8192).0, RunOutcome::ReachedStop);
        let native = trial.live.native.as_ref().unwrap();
        assert!(native.unit_address(trial.head).is_none());
        assert_eq!(native.resident_arrivals(trial.head), Some(0));
    }
    trial.live.state = initial.clone();
    trial.reference.state = initial;
}

// spec: A64-OPT-7, A64-OPT-8, A64-OPT-9
#[test]
fn nested_loop_reloads_the_table_base_at_the_outer_head() {
    let table: Vec<u8> = [3, 0, 1, 0xff, 5].into_iter().cycle().take(96).collect();
    for (force, soft_word) in [(true, false), (false, false), (true, true), (false, true)] {
        let mut trial = nested_frame_trial(
            InnerRole::ShiftCount,
            16,
            &table,
            0x1234_567d,
            soft_word,
            format!("nested frame loop force {force} soft word {soft_word}"),
        );
        enter_nested_loop(&mut trial, force);
        assert_eq!(trial.compare(8192).0, RunOutcome::ReachedStop);
        assert!(
            trial
                .live
                .native
                .as_ref()
                .unwrap()
                .resident_arrivals(trial.head)
                .unwrap()
                > 0
        );
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-9
#[test]
fn seeded_nested_loops_reload_registers_reused_in_inner_loops() {
    for role in [InnerRole::ShiftCount, InnerRole::WordIndex] {
        for seed in SEEDS {
            let mut rng = Rng(seed);
            let frame_offset = 4 * rng.below(9) as i32;
            let table: Vec<_> = (0..24)
                .map(|_| (rng.below(12) as u8).wrapping_sub(2))
                .collect();
            let mut trial = nested_frame_trial(
                role,
                frame_offset,
                &table,
                rng.next() as u32,
                false,
                format!("{role:?} seed {seed:#x}"),
            );
            enter_nested_loop(&mut trial, true);
            assert_eq!(trial.compare(32), (RunOutcome::Budget, 32));
            assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
            trial.compare_epilogue();
        }
    }
}

fn observe_registers(a: &mut CodeAssembler) {
    for (i, reg) in [eax, ecx, edx, ebx, esp, ebp, esi, edi]
        .into_iter()
        .enumerate()
    {
        a.mov(dword_ptr(RESULTS + 4 * i as u32), reg).unwrap();
    }
    for (i, reg) in [xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7]
        .into_iter()
        .enumerate()
    {
        a.movdqu(xmmword_ptr(RESULTS + 32 + 16 * i as u32), reg)
            .unwrap();
    }
    for (i, reg) in [mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7]
        .into_iter()
        .enumerate()
    {
        a.movq(qword_ptr(RESULTS + 160 + 8 * i as u32), reg)
            .unwrap();
    }
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Integer,
    Frame,
    CarriedFrame,
    Division,
    Sse,
    Mmx,
    Pointer,
    Indexed,
    EarlyExit,
    FrameEscape,
}

const SEEDS: [u64; 4] = [1, 0x9e37_79b9, 0x1234_5678_9abc_def0, 0xd1ff_e2e7_a640_0241];

fn generated(shape: Shape, seed: u64) {
    let mut rng = Rng(seed);
    let count = 16 + rng.below(12) as u32;
    let early_count = 2 + rng.below(4) as i32;
    let divisor = 2 + rng.below(29) as i32;
    let displacement = 4 * rng.below(8) as i32;
    let image = Image::assemble(
        |a, head, stop| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            match shape {
                Shape::Integer => {
                    for _ in 0..4 {
                        let op = rng.below(7);
                        alu_imm(a, op, eax, rng.imm());
                        if rng.below(2) == 0 {
                            a.adc(eax, rng.imm()).unwrap();
                        } else {
                            a.sbb(eax, rng.imm()).unwrap();
                        }
                    }
                }
                Shape::Frame | Shape::CarriedFrame => {
                    if matches!(shape, Shape::CarriedFrame) {
                        a.add(eax, dword_ptr(esp - 8)).unwrap();
                    }
                    a.mov(dword_ptr(esp - 8), eax).unwrap();
                    a.add(word_ptr(esp - 7), rng.below(256) as u32).unwrap();
                    a.movzx(edx, byte_ptr(esp - 6)).unwrap();
                    a.add(eax, dword_ptr(esp - 8)).unwrap();
                }
                Shape::Division => {
                    a.cdq().unwrap();
                    a.mov(ebx, divisor).unwrap();
                    a.idiv(ebx).unwrap();
                    a.add(eax, edx).unwrap();
                }
                Shape::Sse => {
                    a.cvtsi2sd(xmm1, eax).unwrap();
                    if rng.below(2) == 0 {
                        a.addsd(xmm0, xmm1).unwrap();
                    } else {
                        a.subsd(xmm0, xmm1).unwrap();
                    }
                    a.cvttsd2si(eax, xmm0).unwrap();
                }
                Shape::Mmx => {
                    a.movd(mm1, eax).unwrap();
                    if rng.below(2) == 0 {
                        a.paddd(mm0, mm1).unwrap();
                    } else {
                        a.psubd(mm0, mm1).unwrap();
                    }
                    a.pxor(mm2, mm0).unwrap();
                    a.movd(eax, mm2).unwrap();
                }
                Shape::Pointer => {
                    a.mov(eax, dword_ptr(eax)).unwrap();
                    a.add(eax, ecx).unwrap();
                }
                Shape::Indexed => {
                    a.mov(dword_ptr(ebx + eax * 4 + displacement), ecx).unwrap();
                }
                Shape::EarlyExit => {
                    a.cmp(ecx, early_count).unwrap();
                    a.je(stop).unwrap();
                }
                Shape::FrameEscape => {
                    a.mov(dword_ptr(esp - 8), eax).unwrap();
                    a.lea(edx, esp - 8).unwrap();
                    a.mov(dword_ptr(edi + 0x400), edx).unwrap();
                    a.mov(edx, dword_ptr(edi + 0x400)).unwrap();
                    a.mov(dword_ptr(edx), ecx).unwrap();
                    a.add(eax, dword_ptr(esp - 8)).unwrap();
                }
            }
            a.mov(dword_ptr(edi), eax).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            latch(a, head);
        },
        observe_registers,
    );

    let mut state = initial(count);
    state.regs[3] = DATA + 0x2000;
    state.eflags |= rng.next() as u32 & 0x8d5;
    state.xmm[0][8..].copy_from_slice(&rng.next().to_le_bytes());
    state.xmm[0][..8].copy_from_slice(&3.5f64.to_le_bytes());
    // The existing scalar forms require sticky precision on entry.
    state.mxcsr |= 0x20;
    let words: Vec<u8> = (0..count)
        .flat_map(|_| {
            let word = match shape {
                Shape::Sse => rng.below(31) as u32,
                Shape::Pointer => DATA + 0x800 + 4 * rng.below(64) as u32,
                Shape::Indexed => rng.below(128) as u32,
                _ => rng.next() as u32,
            };
            word.to_le_bytes()
        })
        .collect();
    let pointed: Vec<u8> = (0..256).map(|_| rng.next() as u8).collect();
    for fuel in [32, 4096] {
        let mut trial = Trial::new(
            &image,
            &state,
            format!("{shape:?} seed {seed:#x} fuel {fuel}"),
            &[],
            |m| {
                m.write(DATA, &words).unwrap();
                m.write(DATA + 0x800, &pointed).unwrap();
            },
        );
        trial.install();
        let (outcome, retired) = trial.compare(fuel);
        if fuel == 32 {
            assert_eq!((outcome, retired), (RunOutcome::Budget, fuel));
            assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        } else {
            assert_eq!(outcome, RunOutcome::ReachedStop);
        }
        assert_eq!(
            trial.live.state.regs[1],
            if matches!(shape, Shape::EarlyExit) {
                early_count as u32
            } else {
                0
            }
        );
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-9
#[test]
fn seeded_loops_match_at_the_stop_and_at_every_owner_exit() {
    for shape in [
        Shape::Integer,
        Shape::Frame,
        Shape::CarriedFrame,
        Shape::Division,
        Shape::Sse,
        Shape::Mmx,
    ] {
        for seed in SEEDS {
            generated(shape, seed);
        }
    }
}

// spec: A64-OPT-9
#[test]
fn seeded_loops_with_pointer_reads_indexed_stores_early_exits_and_stack_escapes() {
    for shape in [
        Shape::Pointer,
        Shape::Indexed,
        Shape::EarlyExit,
        Shape::FrameEscape,
    ] {
        for seed in SEEDS {
            generated(shape, seed);
        }
    }
}

// spec: A64-OPT-9, A64-FLAG-3
#[test]
fn a_loop_carried_carry_stays_in_tier1() {
    let image = Image::loop_body(|a, head, _| {
        a.adc(eax, edx).unwrap();
        latch(a, head);
    });
    let mut state = initial(7);
    state.regs[0] = u32::MAX - 1;
    state.regs[2] = 1;
    state.eflags |= 1;
    let mut trial = Trial::new(&image, &state, "loop-carried carry", &[], |_| {});
    assert!(!trial.try_install());
    assert_eq!(trial.compare(8), (RunOutcome::Budget, 8));
    trial.same_state();
    assert_eq!(trial.compare(32), (RunOutcome::ReachedStop, 14));
}

// spec: A64-OPT-6, A64-OPT-9, A64-MEM-5
#[test]
fn a_load_from_a_protected_page_reports_exact_fault_state() {
    let image = Image::loop_body(|a, head, _| {
        a.add(ebx, ecx).unwrap();
        a.mov(dword_ptr(edi), ebx).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(esi, 4).unwrap();
        latch(a, head);
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    let mut state = initial(8);
    state.regs[6] = boundary - 8;
    let mut trial = Trial::new(
        &image,
        &state,
        "protected load",
        &[(SPECIAL, HOST_PAGE)],
        |m| {
            m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
            m.write(boundary - 8, &0x1234_5678u32.to_le_bytes())
                .unwrap();
            m.write(boundary - 4, &0xfedc_ba98u32.to_le_bytes())
                .unwrap();
            m.map(boundary, HOST_PAGE, 0).unwrap();
        },
    );
    trial.install();
    let run = trial.run_native(128).unwrap();
    assert_eq!((run.exit, run.retired), (Exit::Dispatch, 15));
    assert_eq!(
        trial.reference.run(image.stop, 15).unwrap(),
        RunOutcome::Budget
    );
    trial.same_position_and_memory(15, false);
    trial.same_state();
    let (outcome, retired) = trial.compare(128 - 15);
    assert!(matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary));
    assert_eq!(retired, 0);
    assert!(
        trial
            .live
            .native
            .as_ref()
            .unwrap()
            .diagnostics
            .resident_cold_exits
            > 0
    );
}

// spec: A64-OPT-9, A64-MEM-7
#[test]
fn a_store_away_from_code_lines_on_a_watched_page_keeps_code_live() {
    let image = Image::loop_body(|a, head, _| {
        a.mov(dword_ptr(edi), ecx).unwrap();
        a.add(edi, 4).unwrap();
        latch(a, head);
    });
    let mut state = initial(6);
    state.regs[7] = CODE + 0x200;
    let mut trial = Trial::new(&image, &state, "watched page data", &[], |m| {
        m.protect(CODE, HOST_PAGE, READ | WRITE | EXEC).unwrap();
    });
    trial.live.native.as_mut().unwrap().checks = Checks::All;
    trial.install();
    let before = trial
        .live
        .native
        .as_ref()
        .unwrap()
        .diagnostics
        .dropped_units;
    assert_eq!(trial.compare(128), (RunOutcome::ReachedStop, 25));
    assert_eq!(
        trial
            .live
            .native
            .as_ref()
            .unwrap()
            .diagnostics
            .dropped_units,
        before
    );
}

// spec: A64-OPT-9, A64-MEM-5, A64-MEM-7
#[test]
fn a_store_on_a_watched_code_line_invalidates_the_old_instruction() {
    // The immediate of the first instruction at the head.
    let immediate = CODE + 2;
    let image = Image::loop_body(|a, head, _| {
        a.mov(ebx, 0x1234_5678).unwrap();
        a.mov(byte_ptr(edi), cl).unwrap();
        a.mov(edi, immediate).unwrap();
        latch(a, head);
    });
    let mut trial = Trial::new(&image, &initial(4), "watched code line", &[], |m| {
        m.protect(CODE, HOST_PAGE, READ | WRITE | EXEC).unwrap();
    });
    trial.install();
    assert_eq!(trial.compare(128), (RunOutcome::ReachedStop, 21));
    assert_eq!(trial.live.state.regs[3], 0x1234_5602);
    let diagnostics = &trial.live.native.as_ref().unwrap().diagnostics;
    assert!(diagnostics.resident_cold_exits > 0);
    assert!(diagnostics.dropped_units > 0);
}

fn soft_page(memory: &mut Memory, value: u32) {
    memory.map_shared_audio(SPECIAL, PAGE_SIZE).unwrap();
    memory.write(SPECIAL, &value.to_le_bytes()).unwrap();
    let page = memory.pages(SPECIAL, PAGE_SIZE).unwrap().start;
    assert_ne!(memory.native_permissions()[page] & SHARED, 0);
}

// spec: A64-OPT-9, A64-MEM-7, A64-MEM-11
#[test]
fn a_shared_soft_page_completes_loads_and_stores() {
    let image = Image::loop_body(|a, head, _| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, ecx).unwrap();
        a.mov(dword_ptr(esi + 64), eax).unwrap();
        a.adc(ebx, eax).unwrap();
        a.mov(esi, SPECIAL).unwrap();
        latch(a, head);
    });
    let mut trial = Trial::new(
        &image,
        &initial(24),
        "SHARED soft page",
        &[(SPECIAL, PAGE_SIZE)],
        |m| {
            soft_page(m, u32::MAX - 10);
        },
    );
    trial.install();
    assert_eq!(trial.compare(512), (RunOutcome::ReachedStop, 169));
    assert!(trial.live.native.as_ref().unwrap().diagnostics.accesses > 0);
}

// spec: A64-OPT-9, A64-MEM-11
#[test]
fn a_loop_polling_linear_shared_memory_observes_a_device_write() {
    const COUNT: u32 = 1 << 22;
    const VALUE: u8 = 7;
    let image = Image::loop_body(|a, head, stop| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.test(eax, eax).unwrap();
        a.jne(stop).unwrap();
        // Publish progress only after sampling the polled byte. This store
        // is locally disjoint from the load and does not prohibit hoisting.
        a.mov(byte_ptr(esi + 64), cl).unwrap();
        latch(a, head);
    });
    let mut state = initial(COUNT);
    state.regs[6] = SPECIAL;
    let mut trial = Trial::new(
        &image,
        &state,
        "LINEAR polling",
        &[(SPECIAL, HOST_PAGE)],
        |_| {},
    );
    let storage = trial
        .live
        .memory
        .map_shared_audio(SPECIAL, HOST_PAGE)
        .unwrap();
    trial
        .reference
        .memory
        .map_shared_audio(SPECIAL, HOST_PAGE)
        .unwrap();
    let page = trial.live.memory.pages(SPECIAL, HOST_PAGE).unwrap().start;
    assert_eq!(
        trial.live.memory.native_permissions()[page] & (READ | SHARED),
        READ
    );
    trial.install();

    let deadline = Instant::now() + Duration::from_secs(3);
    trial.live.set_deadline(Some(deadline));
    let fuel = 6 * u64::from(COUNT) + 1;
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            // The first iteration publishes zero. Wait for a later sample.
            while storage.load_u8(64) == 0 {
                assert!(Instant::now() < deadline, "guest never published a poll");
                std::thread::yield_now();
            }
            storage.store_u8(0, VALUE);
        });
        let outcome = trial.live.run(image.stop, fuel).unwrap();
        writer.join().unwrap();
        assert_eq!(outcome, RunOutcome::ReachedStop);
        assert!(
            Instant::now() < deadline,
            "guest did not observe the write in time"
        );
    });
    assert_eq!(trial.live.state.regs[0], u32::from(VALUE));
    let missed = COUNT - trial.live.state.regs[1];
    assert!((2..COUNT).contains(&missed));

    // Replay the write at the sample that saw it. The number of polls made
    // before the writer ran depends on host scheduling.
    let prefix = 1 + 6 * u64::from(missed);
    assert_eq!(
        trial.reference.run(image.stop, prefix).unwrap(),
        RunOutcome::Budget
    );
    assert_eq!(trial.reference.state.eip, image.head);
    trial.reference.memory.write(SPECIAL, &[VALUE]).unwrap();
    assert_eq!(
        trial.reference.run(image.stop, 3).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(trial.reference.state.fuel, 0);
    assert_eq!(fuel - trial.live.state.fuel, prefix + 3);
    trial.same_position_and_memory(prefix + 3, true);
    trial.same_state();
}

// spec: A64-OPT-9, A64-MEM-5
#[test]
fn a_read_modify_write_crossing_into_read_only_memory_has_precise_state() {
    let image = Image::loop_body(|a, head, _| {
        a.add(dword_ptr(esi), eax).unwrap();
        a.add(esi, 4).unwrap();
        latch(a, head);
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    let mut state = initial(8);
    state.regs[0] = 0x1234_5678;
    state.regs[6] = boundary - 6;
    let mut trial = Trial::new(
        &image,
        &state,
        "cross-page RMW",
        &[(SPECIAL, 2 * HOST_PAGE)],
        |m| {
            m.map(SPECIAL, 2 * HOST_PAGE, READ | WRITE).unwrap();
            m.write(boundary - 6, &[0x81; 8]).unwrap();
            m.protect(boundary, HOST_PAGE, READ).unwrap();
        },
    );
    trial.install();
    let (outcome, retired) = trial.compare(128);
    assert!(matches!(outcome, RunOutcome::WriteFault { address, .. } if address == boundary));
    assert_eq!(retired, 5);
}

// spec: A64-OPT-9, A64-MEM-7, A64-MEM-11
#[test]
fn a_hoistable_load_rechecks_the_page_class_between_entries() {
    let image = Image::loop_body(|a, head, _| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(ebx, eax).unwrap();
        latch(a, head);
    });
    let mut state = initial(2);
    state.regs[6] = SPECIAL;
    let mut trial = Trial::new(
        &image,
        &state,
        "hoisted load",
        &[(SPECIAL, PAGE_SIZE)],
        |m| {
            m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
            m.write(SPECIAL, &3u32.to_le_bytes()).unwrap();
        },
    );
    trial.install();
    assert_eq!(trial.compare(128), (RunOutcome::ReachedStop, 9));
    assert_eq!(trial.live.state.regs[3], 6);
    let before = trial
        .live
        .native
        .as_ref()
        .unwrap()
        .diagnostics
        .resident_cold_exits;

    for cpu in [&mut trial.live, &mut trial.reference] {
        cpu.memory.unmap(SPECIAL, HOST_PAGE).unwrap();
        soft_page(&mut cpu.memory, 9);
        cpu.state = state.clone();
    }
    assert_eq!(trial.compare(128), (RunOutcome::ReachedStop, 9));
    assert_eq!(trial.live.state.regs[3], 18);
    assert_eq!(
        trial
            .live
            .native
            .as_ref()
            .unwrap()
            .diagnostics
            .resident_cold_exits,
        before,
        "the PlainPage miss should run Tier 1 without a resident fault"
    );
}
