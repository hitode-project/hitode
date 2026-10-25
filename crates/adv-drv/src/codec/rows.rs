//! PNG row admission on hot A64 translation preparation. All library and
//! register differences stop here; the execution layer receives a byte plan.
use adv_abi::native::{
    Budget, Computation, Dependency, DependencyKind, ImageSnapshot, analyze, verify,
};
use adv_cpu::native_region::NativeRegion;

pub(super) fn admit(_pc: u32, code: &[u8]) -> Vec<NativeRegion> {
    let image = ImageSnapshot::new(vec![Dependency {
        offset: 0,
        bytes: code.into(),
        kind: DependencyKind::Code,
    }]);
    let mut budget = Budget { instructions: 256 };
    let mut candidates = analyze::discover(&image, &mut budget);
    candidates.sort_by_key(|c| c.contract.computation != Computation::PngUp);
    let mut regions = Vec::new();
    for candidate in candidates {
        if candidate.entry != 0 || candidate.end as usize != code.len() {
            continue;
        }
        let kernel = match candidate.contract.computation {
            Computation::PngUp => up,
            Computation::PngSub => sub,
            Computation::PngAverage => average,
            Computation::PngPaeth => paeth,
            _ => continue,
        };
        if let Ok(binding) = verify::verify(&image, candidate, &mut budget) {
            // SAFETY: these kernels implement their checked contracts. The
            // bridge establishes the corresponding alias and memory predicates.
            regions.extend(unsafe { binding.lower(kernel) });
        }
    }
    regions
}

unsafe extern "C" fn average(source: *const u8, destination: *mut u8, length: u32, bpp: u32) {
    #[cfg(all(test, target_arch = "aarch64"))]
    tests::BATCHES.with(|n| n.set(n.get() + 1));
    // SAFETY: admitted mappings fix bpp=destination-left. The backward read
    // span proves positive distance/no underflow; all spans include the tail.
    // Prefix borrows stop at the output, and previous/output are disjoint.
    let (row, previous, prefix) = unsafe {
        (
            std::slice::from_raw_parts_mut(destination, length as usize),
            std::slice::from_raw_parts(source, length as usize),
            std::slice::from_raw_parts(destination.sub(bpp as usize), bpp.min(length) as usize),
        )
    };
    let _ = adv_device::codec_kernel::png_average_segment(row, previous, prefix);
}

unsafe extern "C" fn paeth(source: *const u8, destination: *mut u8, length: u32, bpp: u32) {
    #[cfg(all(test, target_arch = "aarch64"))]
    tests::BATCHES.with(|n| n.set(n.get() + 1));
    // SAFETY: read spans cover both prefixes and the following scalar tail.
    // Checked bounds prevent pointer subtraction from wrapping; backward
    // distance and disjoint previous spans separate all mutable borrows.
    let (row, previous, prefix, previous_prefix) = unsafe {
        (
            std::slice::from_raw_parts_mut(destination, length as usize),
            std::slice::from_raw_parts(source, length as usize),
            std::slice::from_raw_parts(destination.sub(bpp as usize), bpp.min(length) as usize),
            std::slice::from_raw_parts(source.sub(bpp as usize), bpp.min(length) as usize),
        )
    };
    let _ = adv_device::codec_kernel::png_paeth_segment(row, previous, prefix, previous_prefix);
}

unsafe extern "C" fn sub(source: *const u8, destination: *mut u8, length: u32, _scalar: u32) {
    #[cfg(all(test, target_arch = "aarch64"))]
    tests::BATCHES.with(|n| n.set(n.get() + 1));
    let prefix_len = (destination as usize - source as usize).min(length as usize);
    // SAFETY: the bridge proves a positive backward distance and readable
    // source / writable output spans. Limiting the prefix makes the Rust
    // borrows disjoint even for overlapping recurrence history.
    let (row, prefix) = unsafe {
        (
            std::slice::from_raw_parts_mut(destination, length as usize),
            std::slice::from_raw_parts(source, prefix_len),
        )
    };
    let _ = adv_device::codec_kernel::png_sub_segment(row, prefix);
}

unsafe extern "C" fn up(source: *const u8, destination: *mut u8, length: u32, _scalar: u32) {
    #[cfg(all(test, target_arch = "aarch64"))]
    tests::BATCHES.with(|n| n.set(n.get() + 1));
    // SAFETY: the specialized bridge proves disjoint readable/writable spans
    // for this whole batch on the sole execution owner. No view escapes.
    let (row, previous) = unsafe {
        (
            std::slice::from_raw_parts_mut(destination, length as usize),
            std::slice::from_raw_parts(source, length as usize),
        )
    };
    let _ = adv_device::codec_kernel::png_up(row, previous);
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;
    use adv_cpu::trap::ExecutionTrap;
    use adv_cpu::{CpuState, Executor, Memory};
    use std::cell::Cell;

    thread_local! { pub(super) static BATCHES: Cell<u64> = const { Cell::new(0) }; }
    const CODE: u32 = 0x10000;
    const SOURCE: u32 = 0x20000;
    const OUTPUT: u32 = 0x24000;
    // Authored indexed byte sum, matching the libpng scalar recurrence.
    const BODY: &[u8] = &[
        0x0f, 0xb6, 0x1c, 0x31, 0x00, 0x1c, 0x32, 0x46, 0x39, 0xf0, 0x75, 0xf4,
    ];
    const AVERAGE: &[u8] = &[
        0x0f, 0xb6, 0x1c, 0x30, 0x0f, 0xb6, 0x14, 0x37, 0x01, 0xda, 0xd1, 0xea, 0x00, 0x14, 0x31,
        0x46, 0x39, 0xf5, 0x75, 0xec,
    ];
    const FRAME_BODY: &[u8] = &[
        0x89, 0x74, 0x24, 0x04, 0x0f, 0xb6, 0x1c, 0x31, 0x89, 0x5c, 0x24, 0x08, 0x8b, 0x3c, 0x24,
        0x39, 0xff, 0x0f, 0x42, 0x5c, 0x24, 0x08, 0x00, 0x1c, 0x32, 0x8b, 0x44, 0x24, 0x0c, 0x46,
        0x39, 0xf0, 0x75, 0xde,
    ];

    struct Admission;
    impl ExecutionTrap for Admission {
        fn native_region(&self, pc: u32, code: &[u8]) -> Vec<NativeRegion> {
            admit(pc, code)
        }
        fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
            unreachable!()
        }
    }

    fn executor(count: u32, output: u32) -> Executor {
        let mut e = Executor::new(CODE, 0x20000).unwrap();
        e.protect(CODE, 0x4000, 7).unwrap();
        e.protect(SOURCE, 0x10000, 3).unwrap();
        e.write(CODE, BODY).unwrap();
        let bytes: Vec<_> = (0..0x10000).map(|i| (i * 73 + i / 13) as u8).collect();
        e.write(SOURCE, &bytes).unwrap();
        e.state.regs = [
            count, SOURCE, output, 0xdeadbeef, 0x2fff0, 0x2ffe0, 0, 0xabcdef01,
        ];
        e.state.eip = CODE;
        e.state.eflags = 0x2ad7;
        e.state.xmm = [[0xa5; 16]; 8];
        e
    }

    fn compare(a: &mut Executor, b: &mut Executor, stop: u32, budget: u64) {
        assert_eq!(
            a.run_with_traps(stop, budget, &mut Admission, false)
                .unwrap(),
            b.run_with_traps(stop, budget, &mut Admission, true)
                .unwrap()
        );
        assert_eq!(
            a.state.capture_architecture(),
            b.state.capture_architecture()
        );
        assert_eq!(
            a.memory().read(SOURCE, 0x10000).unwrap(),
            b.memory().read(SOURCE, 0x10000).unwrap()
        );
    }

    // EXEC-NATIVE-3, A64-TR-8: whole preparation/admission/kernel/bridge chain.
    #[test]
    fn admitted_rows_preserve_full_state_and_exact_budget_cuts() {
        for output in [OUTPUT, OUTPUT + 0xf00, SOURCE + 1] {
            for count in [1, 128, 129, 1024, 4096] {
                for budget in [1, 640, 645, 646, 1024, 100_000] {
                    BATCHES.set(0);
                    let (mut a, mut b) = (executor(count, output), executor(count, output));
                    compare(&mut a, &mut b, CODE + BODY.len() as u32, budget);
                    compare(&mut a, &mut b, CODE + BODY.len() as u32, 100_000);
                    if output == OUTPUT && count >= 1024 {
                        assert!(BATCHES.get() > 0);
                    }
                    if output == SOURCE + 1 && count >= 1024 {
                        assert!(BATCHES.get() > 0);
                    }
                }
            }
        }
    }

    // spec: A64-TR-8, EXEC-NATIVE-3
    #[test]
    fn average_segments_preserve_prefix_history_and_decline_invalid_extra_reads() {
        for bpp in [1, 3, 8, 1024] {
            for case in 0..4 {
                for budget in [8 * 129 - 1, 8 * 129, 4096, 100_000] {
                    let (mut a, mut b) = (executor(2048, OUTPUT), executor(2048, OUTPUT));
                    for e in [&mut a, &mut b] {
                        e.write(CODE, AVERAGE).unwrap();
                        e.state.regs = [
                            0x28000,
                            OUTPUT,
                            0xdeadbeef,
                            0xfedcba98,
                            0x2fff0,
                            2048,
                            0,
                            OUTPUT - bpp,
                        ];
                        match case {
                            1 => e.protect(OUTPUT - 0x1000, 0x1000, 0).unwrap(),
                            2 => e.state.regs[0] = OUTPUT + 1,
                            3 => e.protect(OUTPUT, 0x1000, 1).unwrap(),
                            _ => {}
                        }
                    }
                    BATCHES.set(0);
                    // Compare the readable pages individually in the prefix-fault case.
                    let stop = CODE + AVERAGE.len() as u32;
                    assert_eq!(
                        a.run_with_traps(stop, budget, &mut Admission, false)
                            .unwrap(),
                        b.run_with_traps(stop, budget, &mut Admission, true)
                            .unwrap()
                    );
                    assert_eq!(
                        a.state.capture_architecture(),
                        b.state.capture_architecture()
                    );
                    assert_eq!(a.state.fuel, b.state.fuel);
                    for page in [SOURCE, OUTPUT, OUTPUT + 0x1000, 0x28000] {
                        assert_eq!(
                            a.memory().read(page, 4096).unwrap(),
                            b.memory().read(page, 4096).unwrap()
                        );
                    }
                    if case != 0 {
                        assert_eq!(BATCHES.get(), 0);
                    }
                    if case == 0 && budget >= 100_000 {
                        assert!(BATCHES.get() > 0);
                    }
                }
            }
        }
    }

    // spec: EXEC-NATIVE-4, A64-TR-8
    #[test]
    fn fixed_frame_effects_guards_and_scalar_tail_match_guest_execution() {
        for case in 0..6 {
            let (mut a, mut b) = (executor(2048, OUTPUT), executor(2048, OUTPUT));
            for e in [&mut a, &mut b] {
                e.write(CODE, FRAME_BODY).unwrap();
                let sp = e.state.regs[4];
                e.write(sp, &e.state.regs[7].to_le_bytes()).unwrap();
                e.write(sp + 12, &2048u32.to_le_bytes()).unwrap();
            }
            let stop = CODE + FRAME_BODY.len() as u32;
            compare(&mut a, &mut b, stop, 100_000);
            assert!(BATCHES.get() > 0);
            for e in [&mut a, &mut b] {
                e.state.eip = CODE;
                e.state.regs[6] = 0;
                if case == 2 {
                    e.state.regs[4] = OUTPUT;
                }
                if case == 3 {
                    e.state.regs[4] = SOURCE + 0x100;
                }
                let sp = e.state.regs[4];
                e.write(sp, &e.state.regs[7].to_le_bytes()).unwrap();
                e.write(sp + 12, &if case == 1 { 64u32 } else { 2048 }.to_le_bytes())
                    .unwrap();
                if case == 4 {
                    e.protect(sp & !4095, 4096, 1).unwrap();
                }
                if case == 5 {
                    e.memory_mut().watch_code(sp + 8, 4);
                }
            }
            BATCHES.set(0);
            compare(&mut a, &mut b, stop, 100_000);
            assert_eq!(a.state.fuel, b.state.fuel);
            if case == 0 {
                assert!(BATCHES.get() > 0);
            }
            if matches!(case, 1 | 4 | 5) {
                assert_eq!(BATCHES.get(), 0, "case {case}");
            }
        }
    }

    // spec: EXEC-NATIVE-4
    #[test]
    fn entry_carry_cannot_escape_into_the_final_scratch_slot() {
        let code = [
            0x0f, 0x42, 0xc3, 0x89, 0x44, 0x24, 0x04, 0x8b, 0x04, 0x24, 0x0f, 0xb6, 0x16, 0x00,
            0x17, 0x46, 0x47, 0x49, 0x75, 0xec,
        ];
        let (mut a, mut b) = (executor(1024, OUTPUT), executor(1024, OUTPUT));
        for e in [&mut a, &mut b] {
            e.write(CODE, &code).unwrap();
            e.write(SOURCE, &[255; 1024]).unwrap();
            e.write(OUTPUT, &[1; 1024]).unwrap();
            e.state.regs = [1, 1024, 0, 2, 0x2fff0, 0x2ffe0, SOURCE, OUTPUT];
            e.state.eflags = 0x202;
            e.write(0x2fff0, &1u32.to_le_bytes()).unwrap();
        }
        BATCHES.set(0);
        compare(&mut a, &mut b, CODE + code.len() as u32, 100_000);
        assert_eq!(a.memory().read(0x2fff4, 4).unwrap(), 2u32.to_le_bytes());
        assert_eq!(BATCHES.get(), 0);
    }

    // A64-TR-8: translation invalidation also revokes the specialized bridge.
    #[test]
    fn patches_unmaps_and_internal_stops_cannot_reuse_a_stale_bridge() {
        let (mut a, mut b) = (executor(1024, OUTPUT), executor(1024, OUTPUT));
        compare(&mut a, &mut b, CODE + BODY.len() as u32, 100_000);
        assert!(BATCHES.get() > 0);
        // Change ADD to SUB. The same kernel would now be observably wrong.
        for e in [&mut a, &mut b] {
            e.write(CODE + 4, &[0x28]).unwrap();
            e.state.eip = CODE;
            e.state.regs[6] = 0;
        }
        BATCHES.set(0);
        compare(&mut a, &mut b, CODE + BODY.len() as u32, 100_000);
        assert_eq!(BATCHES.get(), 0);
        for e in [&mut a, &mut b] {
            e.unmap(CODE, 0x4000).unwrap();
            e.map(CODE, 0x4000, 7).unwrap();
            e.write(CODE, BODY).unwrap();
            e.state.eip = CODE;
            e.state.regs[6] = 0;
        }
        compare(&mut a, &mut b, CODE + 7, 100_000);
        assert_eq!(BATCHES.get(), 0);
    }

    // spec: A64-TR-8, EXEC-NATIVE-1
    #[test]
    fn wine_installation_wires_machine_admission_and_guest_mode_keeps_guest_execution() {
        use adv_cpu::backend::{Guest, Prot};
        use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
        for mode in [super::super::Mode::Native, super::super::Mode::Guest] {
            let cpu = adv_cpu::Cpu::new(PARK_ADDR, true).unwrap();
            let mut machine =
                Machine::new(cpu, adv_abi::Registry::default(), Clock::virtual_time()).unwrap();
            super::super::install(&mut machine.ext, mode);
            machine.cpu.map(CODE, 0x4000, Prot::RWX).unwrap();
            machine.cpu.map(SOURCE, 0x10000, Prot::RW).unwrap();
            machine.cpu.write(SOURCE, &[0x55; 0x10000]).unwrap();
            let mut code = Vec::new();
            for (op, value) in [(0xb8, 4096u32), (0xb9, SOURCE), (0xba, OUTPUT), (0xbe, 0)] {
                code.push(op);
                code.extend(value.to_le_bytes());
            }
            code.extend(BODY);
            code.push(0xc3);
            machine.cpu.write(CODE, &code).unwrap();
            machine.spawn("row", CODE, &[], 0x10000).unwrap();
            BATCHES.set(0);
            assert!(matches!(machine.run(), RunResult::Exited(4096)));
            let mut output = [0; 4096];
            machine.cpu.read(OUTPUT, &mut output).unwrap();
            assert_eq!(output, [0xaa; 4096]);
            assert_eq!(BATCHES.get() > 0, mode == super::super::Mode::Native);
        }
    }

    // A64-TR-8: public upstream bodies, not authored recognition templates.
    #[test]
    #[ignore = "run python3 tools/codec/png_reference.py first"]
    fn upstream_png_rows_compiler_configurations_execute_through_the_same_rule() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/codec-reference/png");
        for name in [
            "up",
            "sub",
            "avg",
            "paeth_multibyte_pixel",
            "paeth_1byte_pixel",
        ] {
            for optimization in ["O1", "O2", "Os"] {
                let bytes =
                    std::fs::read(directory.join(format!("{name}-{optimization}.bin"))).unwrap();
                for (count, bpp) in [
                    (0, 1),
                    (1, 1),
                    (3, 3),
                    (129, 1),
                    (129, 4),
                    (4096, 1),
                    (4096, 3),
                    (8192, 1),
                    (8192, 8),
                ] {
                    if matches!(name, "avg" | "paeth_multibyte_pixel" | "paeth_1byte_pixel")
                        && count < bpp
                    {
                        continue;
                    }
                    if name == "paeth_1byte_pixel" && bpp != 1 {
                        continue;
                    }
                    let (mut a, mut b) = (executor(count, OUTPUT), executor(count, OUTPUT));
                    let stop = CODE + 0x2000;
                    for e in [&mut a, &mut b] {
                        e.write(CODE, &bytes).unwrap();
                        let info = SOURCE + 0x3000;
                        let row_info = [count, count, (bpp * 8) << 24 | bpp << 16 | 0x0802u32];
                        for (n, value) in row_info.into_iter().enumerate() {
                            e.write(info + n as u32 * 4, &value.to_le_bytes()).unwrap();
                        }
                        let stack = e.state.regs[4] - 32;
                        e.state.regs[4] = stack;
                        for (n, value) in [stop, info, OUTPUT, SOURCE].into_iter().enumerate() {
                            e.write(stack + n as u32 * 4, &value.to_le_bytes()).unwrap();
                        }
                    }
                    BATCHES.set(0);
                    compare(&mut a, &mut b, stop, 1_000_000);
                    if count >= 4096 {
                        if name == "paeth_1byte_pixel"
                            || (name == "paeth_multibyte_pixel" && optimization == "Os")
                        {
                            assert_eq!(
                                BATCHES.get(),
                                0,
                                "top-tested Paeth remains guest execution"
                            );
                            continue;
                        }
                        let image = ImageSnapshot::new(vec![Dependency {
                            offset: 0,
                            bytes: bytes.clone().into(),
                            kind: DependencyKind::Code,
                        }]);
                        let mut budget = Budget {
                            instructions: 10_000,
                        };
                        let reports: Vec<_> = analyze::discover(&image, &mut budget)
                            .into_iter()
                            .map(|c| {
                                (
                                    c.contract.computation,
                                    verify::verify(&image, c, &mut budget)
                                        .map(|b| b.frame().is_some()),
                                )
                            })
                            .collect();
                        assert!(BATCHES.get() > 0, "{name} {optimization}: {reports:?}");
                    }
                }
            }
        }
    }
}
