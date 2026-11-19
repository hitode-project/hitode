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

// spec: A64-OPT-2, A64-OPT-9
#[test]
fn resident_reads_esp_after_a_push_with_a_known_stack_pointer() {
    let image = Image::loop_body(|a, head, _| {
        a.mov(esp, FRAME).unwrap();
        a.push(edx).unwrap();
        a.mov(eax, esp).unwrap();
        a.pop(edx).unwrap();
        latch(a, head);
    });
    let state = initial(8);
    let mut trial = Trial::new(&image, &state, "known ESP after push", &[], |_| {});
    trial.install();
    assert_eq!(trial.compare(128).0, RunOutcome::ReachedStop);
    assert_eq!(trial.live.state.regs[0], FRAME - 4);
}

// spec: A64-OPT-2, A64-OPT-3
#[test]
fn resident_reloads_a_saved_frame_pointer_after_a_control_word_store() {
    let image = Image::loop_body(|a, head, _| {
        a.push(esp).unwrap();
        a.fnstcw(word_ptr(esp)).unwrap();
        a.pop(ebp).unwrap();
        a.mov(eax, dword_ptr(ebp)).unwrap();
        a.mov(ebp, esp).unwrap();
        latch(a, head);
    });
    let mut state = initial(8);
    state.regs[5] = FRAME;
    state.x87.status |= 0x20;
    let restored = (FRAME & 0xffff_0000) | u32::from(state.x87.control);
    let mut trial = Trial::new(&image, &state, "modified saved frame pointer", &[], |m| {
        m.map(restored & !0x3fff, HOST_PAGE, READ | WRITE).unwrap();
        m.write(restored, &42u32.to_le_bytes()).unwrap();
        m.write(FRAME, &17u32.to_le_bytes()).unwrap();
    });
    assert!(trial.try_install_at(state));
    assert_eq!(trial.compare(128).0, RunOutcome::ReachedStop);
    assert_eq!(trial.live.state.regs[0], 42);
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
        self.try_install_at(CpuState::default())
    }

    fn try_install_at(&mut self, mut at: CpuState) -> bool {
        let native = self.live.native.as_mut().unwrap();
        let memory = &mut self.live.memory;
        memory.attach().unwrap();
        // Register the stop before installing the form. New stops can cut units.
        native
            .run(&mut self.live.state, memory, self.stop, 0, null_mut())
            .unwrap();
        at.eip = self.head;
        native.force_resident_at(memory, at)
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
    for baseline_only in [false, true] {
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
            trial
                .live
                .native
                .as_mut()
                .unwrap()
                .set_baseline_only(baseline_only);
            if !baseline_only {
                trial.install();
            }

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
}

// spec: A64-OPT-9 A64-OPT-13
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

// spec: A64-OPT-9, A64-FLAG-2, A64-FLAG-4
#[test]
fn seeded_integer_memory_forms_and_flag_consumers() {
    for seed in SEEDS {
        let mut rng = Rng(seed);
        let image = Image::assemble(
            |a, head, _| {
                a.mov(eax, dword_ptr(0x10).fs()).unwrap();
                a.mov(ebx, dword_ptr(0x14).fs()).unwrap();
                a.bt(dword_ptr(esi), 31).unwrap();
                a.cmovb(eax, ebx).unwrap();
                a.cmovb(ebx, dword_ptr(esi + 4)).unwrap();
                a.setb(ah).unwrap();
                a.movsx(dx, byte_ptr(esi + 1)).unwrap();
                a.movzx(ebp, word_ptr(esi + 2)).unwrap();
                a.bswap(eax).unwrap();
                a.shl(ah, 9).unwrap();
                a.sar(dx, 16).unwrap();
                a.setb(al).unwrap();
                a.push(dword_ptr(esi)).unwrap();
                a.pop(edx).unwrap();
                a.mul(ebx).unwrap();
                a.seto(byte_ptr(edi)).unwrap();
                a.setb(byte_ptr(edi + 1)).unwrap();
                a.imul(dword_ptr(esi + 4)).unwrap();
                a.seto(byte_ptr(edi + 2)).unwrap();
                a.setb(byte_ptr(edi + 3)).unwrap();
                a.adc(byte_ptr(esi + 1), ah).unwrap();
                a.sbb(word_ptr(esi + 2), dx).unwrap();
                a.adc(dword_ptr(esi + 4), eax).unwrap();
                a.sbb(bx, word_ptr(esi + 4)).unwrap();
                a.seto(byte_ptr(edi + 4)).unwrap();
                a.setb(byte_ptr(edi + 5)).unwrap();
                a.sete(byte_ptr(edi + 6)).unwrap();
                a.setp(byte_ptr(edi + 7)).unwrap();
                a.mov(dword_ptr(edi + 8), eax).unwrap();
                a.mov(dword_ptr(edi + 12), edx).unwrap();
                a.add(esi, 8).unwrap();
                a.add(edi, 16).unwrap();
                latch(a, head);
            },
            observe_registers,
        );
        let mut state = initial(8);
        state.fs_base = DATA + 0x800;
        let mut words: Vec<u8> = (0..64).map(|_| rng.next() as u8).collect();
        words[3] |= 0x80;
        let mut trial = Trial::new(
            &image,
            &state,
            format!("integer memory seed {seed:#x}"),
            &[],
            |m| {
                m.write(DATA, &words).unwrap();
                m.write(state.fs_base + 0x10, &0xfedc_ba89u32.to_le_bytes())
                    .unwrap();
                m.write(state.fs_base + 0x14, &0x8000_0001u32.to_le_bytes())
                    .unwrap();
            },
        );
        trial.install();
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        assert!(
            trial
                .live
                .native
                .as_ref()
                .unwrap()
                .resident_arrivals(image.head)
                .unwrap()
                > 0
        );
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-9, A64-FLAG-2
#[test]
fn seeded_rotates_keep_flags_for_zero_and_mask_cl() {
    for seed in SEEDS {
        let input = Rng(seed).next() as u32;
        let image = Image::assemble(
            |a, head, _| {
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.mov(ebx, eax).unwrap();
                a.cmp(edx, 1).unwrap();
                a.rol(eax, cl).unwrap();
                a.setb(byte_ptr(edi)).unwrap();
                a.seto(byte_ptr(edi + 1)).unwrap();
                a.sets(byte_ptr(edi + 2)).unwrap();
                a.ror(ebx, cl).unwrap();
                a.setb(byte_ptr(edi + 3)).unwrap();
                a.seto(byte_ptr(edi + 4)).unwrap();
                a.rol(eax, 1).unwrap();
                a.ror(ebx, 31).unwrap();
                a.rol(eax, 32).unwrap();
                a.ror(ebx, 33).unwrap();
                a.setb(byte_ptr(edi + 5)).unwrap();
                a.seto(byte_ptr(edi + 6)).unwrap();
                a.mov(dword_ptr(edi + 8), eax).unwrap();
                a.mov(dword_ptr(edi + 12), ebx).unwrap();
                a.add(edi, 16).unwrap();
                a.sub(ecx, 1).unwrap();
                a.jae(head).unwrap();
            },
            observe_registers,
        );
        let mut state = initial(33);
        state.regs[2] = 0x8000_0000;
        let mut trial = Trial::new(&image, &state, format!("rotate seed {seed:#x}"), &[], |m| {
            m.write(DATA, &input.to_le_bytes()).unwrap();
        });
        trial.install();
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-9, A64-MEM-5
#[test]
fn a_false_cmov_still_faults_on_its_memory_source() {
    let image = Image::loop_body(|a, head, _| {
        a.cmp(ecx, ecx).unwrap();
        a.cmovne(eax, dword_ptr(esi)).unwrap();
        a.add(esi, 4).unwrap();
        latch(a, head);
    });
    let mut state = initial(4);
    let boundary = SPECIAL + HOST_PAGE as u32;
    state.regs[6] = boundary - 4;
    let mut trial = Trial::new(&image, &state, "false cmov", &[], |m| {
        m.map(SPECIAL, HOST_PAGE, READ).unwrap();
        m.map(boundary, HOST_PAGE, 0).unwrap();
    });
    trial.install();
    let (outcome, retired) = trial.compare(128);
    assert!(matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary));
    assert_eq!(retired, 7);
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

// spec: A64-OPT-4, A64-OPT-9, A64F-FP-5
#[test]
fn sse_invalid_results_resume_when_mxcsr_invalid_is_already_set() {
    let image = Image::loop_body(|a, head, _| {
        a.comisd(xmm0, xmm1).unwrap();
        a.setp(al).unwrap();
        a.ucomisd(xmm1, xmm0).unwrap();
        a.setp(bl).unwrap();
        a.movaps(xmm2, xmm0).unwrap();
        a.cmppd(xmm2, xmm1, 1).unwrap();
        a.cvttsd2si(edx, xmm0).unwrap();
        latch(a, head);
    });
    let mut state = initial(4);
    state.mxcsr |= 0x21;
    state.xmm[0][..8].copy_from_slice(&0x7ff8_0000_0000_1234u64.to_le_bytes());
    state.xmm[1][..8].copy_from_slice(&0x7ff0_0000_0000_5678u64.to_le_bytes());
    state.xmm[1][8..].copy_from_slice(&1f64.to_le_bytes());
    let mut trial = Trial::new(&image, &state, "SSE masked invalid", &[], |_| {});
    assert!(trial.try_install_at(state));
    assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    let native = trial.live.native.as_ref().unwrap();
    assert!(native.resident_arrivals(image.head).unwrap() > 0);
    assert_eq!(native.diagnostics.resident_cold_exits, 0);
}

// spec: A64-OPT-9, A64F-FP-5, A64-MEM-7
#[test]
fn seeded_scalar_sse_memory_forms_cross_a_page() {
    for seed in SEEDS {
        let mut rng = Rng(seed);
        let boundary = SPECIAL + HOST_PAGE as u32;
        let image = Image::assemble(
            |a, head, _| {
                a.movsd_2(xmm0, qword_ptr(esi)).unwrap();
                a.movss(xmm1, dword_ptr(esi + 10)).unwrap();
                a.movq(xmm2, qword_ptr(esi)).unwrap();
                a.movd(xmm3, dword_ptr(esi + 10)).unwrap();
                a.cvtsi2ss(xmm4, dword_ptr(esi + 16)).unwrap();
                a.cvtsi2sd(xmm5, dword_ptr(esi + 16)).unwrap();
                a.mulss(xmm1, dword_ptr(esi + 10)).unwrap();
                a.addss(xmm1, dword_ptr(esi + 10)).unwrap();
                a.subss(xmm1, dword_ptr(esi + 10)).unwrap();
                a.divss(xmm1, dword_ptr(esi + 10)).unwrap();
                a.mulsd(xmm0, qword_ptr(esi)).unwrap();
                a.addsd(xmm0, qword_ptr(esi)).unwrap();
                a.subsd(xmm0, qword_ptr(esi)).unwrap();
                a.divsd(xmm0, qword_ptr(esi)).unwrap();
                a.movsd_2(qword_ptr(edi), xmm0).unwrap();
                a.movss(dword_ptr(edi + 8), xmm1).unwrap();
                a.movq(qword_ptr(edi + 12), xmm2).unwrap();
                a.movd(dword_ptr(edi + 20), xmm3).unwrap();
                a.add(edi, 24).unwrap();
                latch(a, head);
            },
            observe_registers,
        );
        let mut state = initial(4);
        state.regs[6] = boundary - 4;
        state.mxcsr |= 0x20;
        state.xmm[0] = [0x55; 16];
        state.xmm[1] = [0xaa; 16];
        state.xmm[4] = [0x55; 16];
        state.xmm[5] = [0x55; 16];
        let double = 1.25 + rng.below(32) as f64;
        let single = 1.75 + rng.below(32) as f32;
        let integer = (rng.next() as u32) | (1 << 24);
        let mut trial = Trial::new(
            &image,
            &state,
            format!("scalar SSE seed {seed:#x}"),
            &[(SPECIAL, 2 * HOST_PAGE)],
            |m| {
                m.map(SPECIAL, 2 * HOST_PAGE, READ | WRITE).unwrap();
                m.write(boundary - 4, &double.to_le_bytes()).unwrap();
                m.write(boundary + 6, &single.to_le_bytes()).unwrap();
                m.write(boundary + 12, &integer.to_le_bytes()).unwrap();
            },
        );
        trial.live.native.as_mut().unwrap().checks = Checks::All;
        trial.install();
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        assert_eq!(
            trial
                .live
                .native
                .as_ref()
                .unwrap()
                .diagnostics
                .resident_cold_exits,
            0
        );
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-4, A64-OPT-9, A64F-FP-5, EXEC-CORE-9
#[test]
fn scalar_single_special_values_and_modes_match_reference() {
    let image = Image::loop_body(|a, head, _| {
        a.movaps(xmm1, xmm0).unwrap();
        a.movaps(xmm2, xmm0).unwrap();
        a.movaps(xmm3, xmm0).unwrap();
        a.movaps(xmm4, xmm0).unwrap();
        a.addss(xmm1, dword_ptr(esi)).unwrap();
        a.subss(xmm2, dword_ptr(esi)).unwrap();
        a.mulss(xmm3, dword_ptr(esi)).unwrap();
        a.divss(xmm4, dword_ptr(esi)).unwrap();
        latch(a, head);
    });
    for modes in [0, 0x40, 0x8000, 0x8040] {
        for (a, b) in [
            (0x8000_0000u32, 0x3f80_0000u32), // signed zero
            (0, 0),                           // zero divided by zero
            (0x0080_0000, 0x3f00_0000),       // subnormal product
            (0x0000_0001, 0x4000_0000),       // subnormal operand, zero quotient
            (0x0080_0000, 0x0080_0001),       // subnormal difference from normal operands
            (0x7f7f_ffff, 0x4000_0000),       // overflow
            (0x3f80_0000, 0),                 // divide by zero
            (0x7f80_0000, 0xff80_0000),       // infinities, including invalid arithmetic
            (0x7fc0_1234, 0xffc0_5678),       // first quiet NaN's payload
            (0x7fc0_1234, 0xff80_5678),       // quiet and signaling NaNs
            (0xff80_5678, 0x3f80_0000),       // quieting a signaling NaN
        ] {
            let mut state = initial(4);
            state.mxcsr |= if modes == 0 { 0x3f } else { modes };
            state.xmm[0] = [0x55; 16];
            state.xmm[0][..4].copy_from_slice(&a.to_le_bytes());
            let mut trial = Trial::new(
                &image,
                &state,
                format!("scalar SSE MXCSR={:#x} {a:#x}, {b:#x}", state.mxcsr),
                &[],
                |m| m.write(DATA, &b.to_le_bytes()).unwrap(),
            );
            assert_eq!(trial.try_install_at(state), modes == 0);
            assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
            let native = trial.live.native.as_ref().unwrap();
            if modes == 0 {
                assert!(native.resident_arrivals(image.head).unwrap() > 0);
            }
            assert_eq!(native.diagnostics.resident_cold_exits, 0);
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

// spec: A64-OPT-5, A64-OPT-9
#[test]
fn a_read_modify_write_crossing_plain_pages_stays_in_the_form() {
    let image = Image::loop_body(|a, head, _| {
        a.add(dword_ptr(esi), eax).unwrap();
        a.add(esi, 5).unwrap();
        latch(a, head);
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    let mut state = initial(8);
    state.regs[0] = 0x1234_5678;
    state.regs[6] = boundary - 7;
    for checks in [Checks::Adaptive, Checks::All] {
        let mut trial = Trial::new(
            &image,
            &state,
            "plain cross-page RMW",
            &[(SPECIAL, 2 * HOST_PAGE)],
            |m| {
                m.map(SPECIAL, 2 * HOST_PAGE, READ | WRITE).unwrap();
                m.write(boundary - 7, &[0x81; 40]).unwrap();
            },
        );
        trial.live.native.as_mut().unwrap().checks = checks;
        trial.install();
        assert_eq!(trial.compare(128), (RunOutcome::ReachedStop, 33));
        let diagnostics = &trial.live.native.as_ref().unwrap().diagnostics;
        assert_eq!(diagnostics.resident_cold_exits, 0);
        assert_eq!(diagnostics.resident_page_crossings, 1);
    }
}

// spec: A64-OPT-5, A64-OPT-9, A64-MEM-5
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
    let diagnostics = &trial.live.native.as_ref().unwrap().diagnostics;
    assert_eq!(
        diagnostics.resident_cold_exits_by_reason[&crate::runtime::ResidentCold::PageCross],
        1
    );
    assert_eq!(diagnostics.resident_page_crossings, 0);
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

fn vector_image(width: u8) -> Image {
    Image::with_prelude(
        |a, stop| {
            a.test(ecx, ecx).unwrap();
            a.je(stop).unwrap();
        },
        |a, head, _| {
            match width {
                1 => a.movzx(eax, byte_ptr(esi)),
                2 => a.movzx(eax, word_ptr(esi)),
                4 => a.mov(eax, dword_ptr(esi)),
                _ => unreachable!(),
            }
            .unwrap();
            a.xor(eax, 13).unwrap();
            a.and(
                eax,
                match width {
                    1 => 255,
                    2 => 65535,
                    _ => -1,
                },
            )
            .unwrap();
            a.shr(eax, 1).unwrap();
            a.not(eax).unwrap();
            a.imul_3(eax, eax, 3).unwrap();
            a.add(eax, edx).unwrap();
            a.sub(eax, 7).unwrap();
            match width {
                1 => a.mov(byte_ptr(edi), al),
                2 => a.mov(word_ptr(edi), ax),
                4 => a.mov(dword_ptr(edi), eax),
                _ => unreachable!(),
            }
            .unwrap();
            a.add(esi, i32::from(width)).unwrap();
            a.add(edi, i32::from(width)).unwrap();
            latch(a, head);
        },
        observe_registers,
    )
}

fn vector_install(trial: &mut Trial<'_>, at: CpuState, lanes: u8) {
    let compiled = crate::jit_map::collect_vectorized(|| assert!(trial.try_install_at(at)));
    assert!(
        compiled.contains(&(trial.head, lanes)),
        "{}: expected {lanes} vector lanes, got {compiled:?}",
        trial.context
    );
}

fn vector_data(seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    (0..4096).map(|_| rng.next() as u8).collect()
}

// spec: A64-OPT-6, A64-FLAG-3
#[test]
fn resident_head_load_fault_keeps_arrival_flags() {
    let image = vector_image(1);
    let boundary = SPECIAL + HOST_PAGE as u32;
    for readable in [1, 49] {
        let mut cpu = initial(79);
        cpu.regs[6] = boundary - readable;
        let mut trial = Trial::new(
            &image,
            &cpu,
            format!("head fault after {readable} iterations"),
            &[(SPECIAL, HOST_PAGE)],
            |m| {
                m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
                m.write(cpu.regs[6], &[0x35; 49][..readable as usize])
                    .unwrap();
                m.map(boundary, HOST_PAGE, 0).unwrap();
            },
        );
        vector_install(&mut trial, cpu.clone(), 16);
        let (outcome, retired) = trial.compare(4096);
        assert!(matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary));
        assert_eq!(retired, 2 + 13 * u64::from(readable));
        assert_eq!(trial.live.state.eip, image.head);
        assert_eq!(trial.live.state.eflags, 0x6);
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
}

// spec: A64-OPT-6, A64-FLAG-3
#[test]
fn resident_head_recovers_flags_from_either_latch() {
    let image = Image::loop_body(|a, head, stop| {
        let mut compare = a.create_label();
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.inc(esi).unwrap();
        a.dec(ebx).unwrap();
        a.jne(compare).unwrap();
        latch(a, head);
        a.jmp(stop).unwrap();
        a.set_label(&mut compare).unwrap();
        a.lea(ecx, dword_ptr(ecx - 1)).unwrap();
        a.cmp(ecx, 100).unwrap();
        a.jb(head).unwrap();
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    for (path, flags, retired_at_fault) in [(3, 0x3, 21), (0, 0x87, 22)] {
        let mut cpu = initial(7);
        cpu.regs[3] = path;
        cpu.regs[6] = boundary - 3;
        let mut trial = Trial::new(&image, &cpu, format!("latch {path}"), &[], |m| {
            m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
            m.map(boundary, HOST_PAGE, 0).unwrap();
        });
        trial.install();
        let (outcome, retired) = trial.compare(128);
        assert!(matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary));
        assert_eq!(retired, retired_at_fault);
        assert_eq!(trial.live.state.eip, image.head);
        assert_eq!(trial.live.state.eflags, flags);
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
}

// spec: A64-OPT-10
#[test]
fn vector_seeded_elementwise_widths_and_exact_tails() {
    for width in [1, 2, 4] {
        let lanes = 16 / width;
        let image = vector_image(width);
        let short = [0, 1, lanes - 1, lanes, lanes + 1, 3 * lanes + 7];
        for count in short
            .map(u32::from)
            .into_iter()
            .chain((width == 1).then_some(5007))
        {
            let mut cpu = initial(count);
            cpu.regs[2] = 0x8765_4321;
            cpu.regs[7] = DATA + 0x2000;
            let data = vector_data(0xcafe + u64::from(count));
            let mut trial = Trial::new(
                &image,
                &cpu,
                format!("width {width}, trip count {count}"),
                &[],
                |m| {
                    m.write(DATA, &data).unwrap();
                    m.write(DATA + 0x1000, &data).unwrap();
                },
            );
            vector_install(&mut trial, cpu, lanes);
            assert_eq!(trial.compare(70_000).0, RunOutcome::ReachedStop);
            trial.compare_epilogue();
        }
    }
}

// spec: A64-OPT-10
#[test]
fn vector_region_hoists_guards_over_plain_pages() {
    let image = vector_image(1);
    let mut cpu = initial(4096);
    cpu.regs[6] = DATA + 23;
    cpu.regs[7] = DATA + 0x2000 + 61;
    let data = vector_data(829);
    let mut trial = Trial::new(&image, &cpu, "strip-mined byte loop", &[], |m| {
        m.write(DATA, &data).unwrap();
        m.write(DATA + 0x1000, &data[..23]).unwrap();
    });
    vector_install(&mut trial, cpu, 16);
    assert_eq!(
        trial.compare(60_000),
        (RunOutcome::ReachedStop, 2 + 13 * 4096)
    );
    let entries = trial.live.native.as_ref().unwrap().vector_regions();
    assert!(
        (1..=3).contains(&entries),
        "region entry evaluated {entries} times"
    );
}

fn interleave_image(channels: usize, unpack: bool) -> Image {
    Image::loop_body(|a, head, _| interleave_body(a, head, channels, unpack))
}

fn interleave_body(a: &mut CodeAssembler, head: CodeLabel, channels: usize, unpack: bool) {
    interleave_bytes(a, channels, unpack);
    a.add(edi, channels as i32).unwrap();
    a.cmp(edi, ecx).unwrap();
    a.jb(head).unwrap();
}

fn interleave_bytes(a: &mut CodeAssembler, channels: usize, unpack: bool) {
    let planes = [esi, ebx, edx, ebp];
    for (i, &plane) in planes[..channels].iter().enumerate() {
        let interleaved = byte_ptr(edi + i as i32);
        let planar = byte_ptr(plane);
        let (src, dst) = if unpack {
            (interleaved, planar)
        } else {
            (planar, interleaved)
        };
        a.mov(al, src).unwrap();
        a.mov(dst, al).unwrap();
        a.inc(plane).unwrap();
    }
}

// spec: A64-OPT-6, A64-OPT-10
#[test]
fn vector_pack_recovers_latch_flags_after_a_strip() {
    let image = Image::loop_body(|a, outer, _| {
        let mut head = a.create_label();
        // The pack's entry recipe uses EAX, which the pack overwrites.
        a.cmp(eax, 0).unwrap();
        a.set_label(&mut head).unwrap();
        interleave_bytes(a, 4, false);
        a.cmp(edi, ecx).unwrap();
        a.lea(edi, dword_ptr(edi + 4)).unwrap();
        a.jb(head).unwrap();
        a.add(ecx, 321).unwrap();
        a.jmp(outer).unwrap();
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    // The first outer iteration takes one scalar trip before entering the form.
    let mut cpu = initial(OUTPUT - 1);
    cpu.regs[6] = boundary - 50;
    cpu.regs[3] = DATA;
    cpu.regs[2] = DATA + 0x400;
    cpu.regs[5] = DATA + 0x800;
    let data = vector_data(295);
    let mut trial = Trial::new(&image, &cpu, "pack latch flags", &[], |m| {
        m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
        m.map(boundary, HOST_PAGE, 0).unwrap();
        m.write(cpu.regs[6], &data[..50]).unwrap();
        m.write(DATA, &data).unwrap();
    });
    let compiled = crate::jit_map::collect_vectorized(|| assert!(trial.try_install_at(cpu)));
    let &[(head, 16)] = compiled.as_slice() else {
        panic!("expected a 16-lane pack kernel, got {compiled:?}");
    };
    let (outcome, retired) = trial.compare(4096);
    assert!(matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary));
    assert_eq!(retired, 1 + 18 + 1 + 15 * 49);
    assert_eq!(trial.live.state.eip, head);
    assert_eq!(trial.live.state.eflags, 0x87);
    assert_eq!(trial.live.native.as_ref().unwrap().vector_regions(), 3);
}

// spec: A64-OPT-10
#[test]
fn vector_interleaves_and_deinterleaves_two_three_and_four_streams() {
    for channels in [2, 3, 4] {
        for unpack in [false, true] {
            let image = interleave_image(channels, unpack);
            let mut cpu = initial(OUTPUT + 59 * channels as u32);
            for (i, r) in [6, 3, 2, 5].into_iter().enumerate() {
                cpu.regs[r] = DATA + 0x400 * i as u32;
            }
            cpu.regs[0] = 0x1234_5600;
            let data = vector_data(0x941b);
            let mut trial = Trial::new(
                &image,
                &cpu,
                format!("{channels} planes, unpack {unpack}"),
                &[],
                |m| {
                    m.write(DATA, &data).unwrap();
                    m.write(OUTPUT, &data[..256]).unwrap();
                },
            );
            vector_install(&mut trial, cpu, 16);
            assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        }
    }
}

// spec: A64-OPT-10
#[test]
fn vector_region_limits_the_union_of_aliasing_streams() {
    let image = vector_image(1);
    let data = vector_data(123);
    for (gap, entries) in [(3, 5), (49, 3)] {
        let mut cpu = initial(79);
        cpu.regs[7] = DATA + gap;
        let mut trial = Trial::new(&image, &cpu, format!("alias gap {gap}"), &[], |m| {
            m.write(DATA, &data).unwrap();
        });
        vector_install(&mut trial, cpu, 16);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        assert_eq!(
            trial.live.native.as_ref().unwrap().vector_regions(),
            entries
        );
    }
}

// spec: A64-OPT-10
#[test]
fn vector_region_stops_at_a_protected_page_on_each_stream() {
    let image = Image::loop_body(|a, head, _| {
        a.test(ecx, ecx).unwrap();
        interleave_body(a, head, 4, false);
    });
    let boundary = SPECIAL + HOST_PAGE as u32;
    let data = vector_data(295);
    let planes = [6, 3, 2, 5];
    for (index, stream) in planes.into_iter().chain([7]).enumerate() {
        let mut cpu = initial(0);
        for (i, r) in planes.into_iter().enumerate() {
            cpu.regs[r] = DATA + 0x300 * i as u32;
        }
        let width = if stream == 7 { 4 } else { 1 };
        cpu.regs[stream] = boundary - 49 * width;
        cpu.regs[1] = cpu.regs[7] + 4 * 79;
        let mut trial = Trial::new(
            &image,
            &cpu,
            format!("protected stream in register {stream}"),
            &[(SPECIAL, HOST_PAGE)],
            |m| {
                m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
                m.map(boundary, HOST_PAGE, if stream == 7 { READ } else { 0 })
                    .unwrap();
                m.write(DATA, &data).unwrap();
                m.write(cpu.regs[stream], &data[..49 * width as usize])
                    .unwrap();
            },
        );
        vector_install(&mut trial, cpu.clone(), 16);
        let (outcome, retired) = trial.compare(4096);
        if stream == 7 {
            assert!(
                matches!(outcome, RunOutcome::WriteFault { address, .. } if address == boundary)
            );
            assert_eq!(retired, 3 + 16 * 49);
        } else {
            assert!(
                matches!(outcome, RunOutcome::ReadFault { address, .. } if address == boundary)
            );
            assert_eq!(retired, 2 + 16 * 49 + 3 * index as u64);
        }
        let entries = trial.live.native.as_ref().unwrap().vector_regions();
        assert!(
            (2..=3).contains(&entries),
            "region entry evaluated {entries} times"
        );
    }
}

// spec: A64-OPT-10
#[test]
fn vector_region_walks_backwards_with_an_unsigned_trip_bound() {
    let image = Image::loop_body(|a, head, _| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.xor(eax, 123).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.sub(esi, 1).unwrap();
        a.sub(edi, 1).unwrap();
        a.cmp(esi, ecx).unwrap();
        a.ja(head).unwrap();
    });
    let mut cpu = initial(DATA + 23);
    cpu.regs[6] = DATA + 0x1000 + 23;
    cpu.regs[7] = DATA + 0x3000 + 17;
    let data = vector_data(918);
    let mut trial = Trial::new(&image, &cpu, "backwards unsigned bound", &[], |m| {
        m.write(DATA + 24, &data).unwrap();
    });
    vector_install(&mut trial, cpu, 16);
    assert_eq!(
        trial.compare(40_000),
        (RunOutcome::ReachedStop, 1 + 7 * 4096)
    );
    assert_eq!(trial.live.native.as_ref().unwrap().vector_regions(), 2);
}

// spec: A64-OPT-10
#[test]
fn vector_region_stops_at_watched_and_checked_pages() {
    let image = vector_image(1);
    let boundary = SPECIAL + HOST_PAGE as u32;
    let data = vector_data(473);
    for watched in [true, false] {
        let count = if watched {
            49 + PAGE_SIZE as u32 + 127
        } else {
            71
        };
        let mut cpu = initial(count);
        cpu.regs[6] = boundary - 49;
        let mut trial = Trial::new(&image, &cpu, format!("watched page {watched}"), &[], |m| {
            m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
            if watched {
                m.map(boundary, HOST_PAGE, READ | WRITE).unwrap();
            } else {
                m.map_shared_audio(boundary, PAGE_SIZE).unwrap();
            }
            m.write(cpu.regs[6], &data[..(count as usize).min(data.len())])
                .unwrap();
            if watched {
                m.watch_code(boundary, 1);
                m.write(boundary + PAGE_SIZE as u32, &data[..127]).unwrap();
            }
        });
        vector_install(&mut trial, cpu.clone(), 16);
        assert_eq!(trial.compare(60_000).0, RunOutcome::ReachedStop);
        let entries = trial.live.native.as_ref().unwrap().vector_regions();
        if watched {
            // The watched page runs scalar. The plain pages on either side
            // use batches, including when the scalar strip leaves that page.
            let watched_strips = (PAGE_SIZE / 16) as u64;
            let all_strips = u64::from(count.div_ceil(16));
            assert!(
                (watched_strips..all_strips).contains(&entries),
                "region entry evaluated {entries} times"
            );
        } else {
            assert!(entries >= 3, "region entry evaluated {entries} times");
        }
    }
}

// spec: A64-OPT-6, A64-OPT-10
#[test]
fn vector_region_charges_countdown_at_and_after_batch_boundaries() {
    let image = vector_image(1);
    let data = vector_data(456);
    for iterations in [33, 34] {
        let fuel = 2 + 13 * iterations;
        let cpu = initial(71);
        let mut trial = Trial::new(&image, &cpu, format!("countdown {fuel}"), &[], |m| {
            m.write(DATA, &data).unwrap();
        });
        vector_install(&mut trial, cpu, 16);
        assert_eq!(trial.compare(fuel), (RunOutcome::Budget, fuel));
        assert_eq!(trial.live.state.eip, image.head);
        trial.same_state();
        assert_eq!(trial.live.native.as_ref().unwrap().vector_regions(), 2);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
        trial.compare_epilogue();
    }
}

// spec: A64-OPT-10
#[test]
fn vector_clamps_via_cmov_or_a_pure_branch() {
    for branch in [false, true] {
        let image = Image::loop_body(|a, head, _| {
            a.movsx(eax, word_ptr(esi)).unwrap();
            a.add(eax, 129).unwrap();
            a.cmp(eax, edx).unwrap();
            if branch {
                let mut join = a.create_label();
                a.jle(join).unwrap();
                a.mov(eax, edx).unwrap();
                a.set_label(&mut join).unwrap();
            } else {
                a.cmovg(eax, edx).unwrap();
            }
            a.mov(word_ptr(edi), ax).unwrap();
            a.add(esi, 2).unwrap();
            a.add(edi, 2).unwrap();
            latch(a, head);
        });
        let mut cpu = initial(79);
        cpu.regs[2] = 1333;
        let data = vector_data(789);
        let mut trial = Trial::new(&image, &cpu, format!("branch clamp {branch}"), &[], |m| {
            m.write(DATA, &data).unwrap();
        });
        vector_install(&mut trial, cpu, 4);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    }
}

// spec: A64-OPT-10
#[test]
fn vector_unrolls_independent_packed_integer_iterations() {
    for xmm in [false, true] {
        let width = if xmm { 16 } else { 8 };
        let image = Image::loop_body(|a, head, _| {
            if xmm {
                a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
                a.paddusb(xmm0, xmm1).unwrap();
                a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
            } else {
                a.movq(mm0, qword_ptr(esi)).unwrap();
                a.paddusb(mm0, mm1).unwrap();
                a.movq(qword_ptr(edi), mm0).unwrap();
            }
            a.add(esi, width).unwrap();
            a.add(edi, width).unwrap();
            latch(a, head);
        });
        let mut cpu = initial(49);
        cpu.xmm[1] = [122; 16];
        cpu.mmx[1] = 0x4161_8151_6171_8191;
        let data = vector_data(1239);
        let mut trial = Trial::new(&image, &cpu, format!("packed width {width}"), &[], |m| {
            m.write(DATA, &data).unwrap();
        });
        vector_install(&mut trial, cpu, 2);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    }
}

// spec: A64-OPT-10
#[test]
fn vector_signed_bound_in_a_frame_cell() {
    let image = Image::loop_body(|a, head, _| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.add(eax, 71).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.inc(esi).unwrap();
        a.inc(edi).unwrap();
        a.inc(ecx).unwrap();
        a.cmp(ecx, dword_ptr(esp + 4)).unwrap();
        a.jl(head).unwrap();
    });
    let cpu = initial((-100i32) as u32);
    let data = vector_data(1277);
    let mut trial = Trial::new(&image, &cpu, "signed frame bound", &[], |m| {
        m.write(DATA, &data).unwrap();
        m.write(FRAME + 4, &(-29i32).to_le_bytes()).unwrap();
    });
    vector_install(&mut trial, cpu, 16);
    assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
}

// spec: A64-OPT-10
#[test]
fn vector_sign_latch_guards_an_invariant_load() {
    let image = Image::loop_body(|a, head, _| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.or(eax, dword_ptr(ebx)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.sub(ecx, 2).unwrap();
        a.jns(head).unwrap();
    });
    for alias in [false, true] {
        let mut cpu = initial(63);
        cpu.regs[3] = if alias { OUTPUT } else { RESULTS };
        let data = vector_data(498);
        let mut trial = Trial::new(&image, &cpu, format!("invariant alias {alias}"), &[], |m| {
            m.write(DATA, &data).unwrap();
            m.write(OUTPUT, &0x8543_2100u32.to_le_bytes()).unwrap();
            m.write(RESULTS, &0xff00_0000u32.to_le_bytes()).unwrap();
        });
        vector_install(&mut trial, cpu, 4);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    }
}

// spec: A64-OPT-10
#[test]
fn vector_interleaves_packed_integer_streams_with_a_sign_latch() {
    let image = Image::loop_body(|a, head, _| {
        for (offset, reg) in [(0, mm0), (8, mm1), (16, mm2), (24, mm3)] {
            a.movq(reg, qword_ptr(esi + offset)).unwrap();
            a.por(reg, mm7).unwrap();
        }
        for (offset, reg) in [(0, mm0), (8, mm1), (16, mm2), (24, mm3)] {
            a.movntq(qword_ptr(edi + offset), reg).unwrap();
        }
        a.add(esi, 32).unwrap();
        a.add(edi, 32).unwrap();
        a.sub(ecx, 8).unwrap();
        a.jns(head).unwrap();
    });
    for in_place in [false, true] {
        let mut cpu = initial(191);
        if in_place {
            cpu.regs[7] = cpu.regs[6];
        }
        cpu.x87.regs[7] = adv_cpu_core::X87Register::from_parts(0xff00_0000_ff00_0000, 0xffff);
        cpu.refresh_mmx();
        let data = vector_data(592);
        let mut trial = Trial::new(
            &image,
            &cpu,
            format!("interleave in place {in_place}"),
            &[],
            |m| {
                m.write(DATA, &data).unwrap();
            },
        );
        vector_install(&mut trial, cpu, 2);
        assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    }
}

// spec: A64-OPT-9, A64-OPT-10
#[test]
fn vector_enters_resident_from_a_deferred_mmx_arrival() {
    let image = Image::with_prelude(
        |a, _| a.movq(mm7, qword_ptr(ebx)).unwrap(),
        |a, head, _| {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.por(mm0, mm7).unwrap();
            a.movq(qword_ptr(edi), mm0).unwrap();
            a.add(esi, 8).unwrap();
            a.add(edi, 8).unwrap();
            latch(a, head);
        },
        |_| {},
    );
    let mut cpu = initial(128);
    cpu.regs[3] = RESULTS;
    cpu.x87.set_top(3);
    let mut trial = Trial::new(&image, &cpu, "deferred MMX", &[], |m| {
        m.write(DATA, &vector_data(172)).unwrap();
        m.write(RESULTS, &0xff00_0000_ff00_0000u64.to_le_bytes())
            .unwrap();
    });
    vector_install(&mut trial, cpu, 2);
    assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    assert!(trial.live.native.as_ref().unwrap().vector_regions() > 0);
}

// spec: A64-OPT-10
#[test]
fn vector_declines_a_loop_carried_reduction() {
    let image = Image::loop_body(|a, head, _| {
        a.movzx(eax, byte_ptr(esi)).unwrap();
        a.add(ebx, eax).unwrap();
        a.mov(byte_ptr(edi), bl).unwrap();
        a.inc(esi).unwrap();
        a.inc(edi).unwrap();
        latch(a, head);
    });
    let cpu = initial(61);
    let data = vector_data(1833);
    let mut trial = Trial::new(&image, &cpu, "loop-carried reduction", &[], |m| {
        m.write(DATA, &data).unwrap();
    });
    let compiled = crate::jit_map::collect_vectorized(|| assert!(trial.try_install_at(cpu)));
    assert!(compiled.is_empty());
    assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
}

// spec: A64-OPT-10, A64-MEM-4
#[test]
fn vector_read_falls_back_on_unreadable_host_page_neighbor() {
    let image = vector_image(1);
    let data = vector_data(1913);
    let mut cpu = initial(64);
    cpu.regs[6] = SPECIAL;
    let mut trial = Trial::new(
        &image,
        &cpu,
        "mixed host page vector read",
        &[(SPECIAL, PAGE_SIZE)],
        |m| {
            m.map(SPECIAL, HOST_PAGE, READ | WRITE).unwrap();
            m.write(SPECIAL, &data[..64]).unwrap();
            if HOST_PAGE > PAGE_SIZE {
                m.protect(SPECIAL + PAGE_SIZE as u32, PAGE_SIZE, 0).unwrap();
            }
        },
    );
    vector_install(&mut trial, cpu, 16);
    assert_eq!(trial.compare(4096).0, RunOutcome::ReachedStop);
    trial.compare_epilogue();
}
