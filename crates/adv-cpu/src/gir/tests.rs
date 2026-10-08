//! Deterministic, self-authored instruction fixtures and strict prefix oracles.
use super::*;
use crate::{
    CpuState, EXEC, Memory, READ, WRITE,
    interpreter::{Interpreter, Step},
};
use iced_x86::code_asm::*;
use std::sync::atomic::AtomicBool;

const BASE: u32 = 0x10000;
const LENGTH: usize = 0x10000;
const DATA: u32 = BASE + 0x4000;
const STACK: u32 = BASE + 0xc000;

fn fixture() -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut other = a.create_label();
    let mut join = a.create_label();
    let mut callee = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, dword_ptr(esi)).unwrap();
    a.add(eax, ecx).unwrap();
    a.test(ecx, 1).unwrap();
    a.je(other).unwrap();
    a.add(eax, 7).unwrap();
    a.jmp(join).unwrap();
    a.set_label(&mut other).unwrap();
    a.sub(eax, 5).unwrap();
    a.set_label(&mut join).unwrap();
    a.call(callee).unwrap();
    a.mov(dword_ptr(esi + 4), eax).unwrap();
    a.dec(ecx).unwrap();
    a.jne(top).unwrap();
    a.jmp(u64::from(BASE + 0x1000)).unwrap();
    a.set_label(&mut callee).unwrap();
    a.push(ebx).unwrap();
    a.mov(ebx, dword_ptr(esi + 8)).unwrap();
    a.add(eax, ebx).unwrap();
    a.pop(ebx).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(BASE)).unwrap()
}

fn memory(bytes: &[u8]) -> Memory {
    let mut memory = Memory::new(BASE, LENGTH).unwrap();
    memory.protect(BASE, LENGTH, READ | WRITE).unwrap();
    memory.write(BASE, bytes).unwrap();
    memory.write(DATA, &17u32.to_le_bytes()).unwrap();
    memory.write(DATA + 8, &31u32.to_le_bytes()).unwrap();
    memory.protect(BASE, 4096, READ | EXEC).unwrap();
    memory
}

fn initial() -> CpuState {
    CpuState {
        regs: [
            0x7fff_fff0,
            4,
            0xa5a5,
            0x1234,
            STACK,
            STACK,
            DATA,
            DATA + 0x1000,
        ],
        eip: BASE,
        eflags: 0x202,
        ..CpuState::default()
    }
}

fn run(
    code: &lower::Code,
    route: Route,
    memory: &mut Memory,
    frame: &mut Frame,
    budget: u32,
    stop: bool,
) -> u32 {
    let stop = AtomicBool::new(stop);
    let lease = memory::Lease::new(memory);
    if route == Route::Proof
        && !code
            .proven
            .is_subset(&memory::prove(&code.graph, frame, lease.memory))
    {
        return CHECK;
    }
    if route == Route::Backend {
        let window = memory::Window::new(lease.memory);
        let (result, recovered) = unsafe {
            backend::run(code, &window, || {
                code.call(
                    frame,
                    window.pointer(),
                    lease.memory.perms.as_ptr(),
                    &stop,
                    budget,
                )
            })
        };
        if let Some(snapshot) = recovered {
            *frame = snapshot;
        }
        window.copy_back(lease.memory);
        result
    } else {
        unsafe {
            code.call(
                frame,
                lease.memory.ptr.as_ptr(),
                lease.memory.perms.as_ptr(),
                &stop,
                budget,
            )
        }
    }
}

fn compare_prefixes(route: Route) {
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let initial = initial();
    let mut reference = memory(&bytes);
    let proven = memory::prove(&graph, &Frame::from_cpu(&initial), &reference);
    assert_eq!(
        proven.len(),
        3,
        "two invariant data loads and one data store"
    );
    for proof in [&BTreeSet::new(), &proven] {
        let code = lower::compile(&graph, route, BASE, LENGTH, proof, false);
        let mut state = initial.clone();
        let mut interpreter = Interpreter::new();
        for budget in 0..200 {
            let mut candidate = memory(&bytes);
            let mut frame = Frame::from_cpu(&initial);
            let reason = run(&code, route, &mut candidate, &mut frame, budget, false);
            assert_eq!(frame.gpr, state.regs, "register prefix {budget}");
            assert_eq!(frame.flags, state.eflags, "flags prefix {budget}");
            assert_eq!(frame.pc, state.eip, "PC prefix {budget}");
            assert_eq!(frame.committed, budget);
            assert_eq!(
                candidate.read(DATA, 16).unwrap(),
                reference.read(DATA, 16).unwrap()
            );
            assert_eq!(
                candidate.read(STACK - 32, 32).unwrap(),
                reference.read(STACK - 32, 32).unwrap()
            );
            let mut restored = initial.clone();
            frame.restore(&mut restored);
            assert_eq!(
                (restored.regs, restored.eflags, restored.eip),
                (state.regs, state.eflags, state.eip)
            );
            if !graph.instructions.contains_key(&state.eip) {
                assert_eq!(reason, DONE);
                break;
            }
            assert_eq!(reason, BUDGET);
            assert_eq!(interpreter.step(&mut state, &mut reference), Step::Executed);
        }
        // The second variant needs an independent reference trajectory.
        reference = memory(&bytes);
    }
}

#[test]
fn proof_state_ssa_matches_every_interpreter_prefix() {
    compare_prefixes(Route::Proof);
}

#[test]
fn backend_state_ssa_and_real_faults() {
    if std::env::var_os("ADV_GIR_FAULT_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gir::tests::backend_state_ssa_and_real_faults",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ADV_GIR_FAULT_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "hardware child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    compare_prefixes(Route::Backend);
    faults(Route::Backend);
    stack_faults(Route::Backend);
    arithmetic_states(Route::Backend);
}

fn faults(route: Route) {
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let code = lower::compile(&graph, route, BASE, LENGTH, &BTreeSet::new(), false);
    // Exercise data loads, committed data stores, CALL/PUSH and POP/RET faults.
    for (data_offset, stack, protection) in [
        (0, STACK, 0),
        (0, STACK, READ),
        (4092, STACK, READ | WRITE),
        (0, STACK - 4096, READ | WRITE),
    ] {
        let mut state = initial();
        state.regs[6] += data_offset;
        state.regs[4] = stack;
        let mut reference = memory(&bytes);
        reference.protect(DATA, 4096, protection).unwrap();
        reference.protect(DATA + 4096, 4096, 0).unwrap();
        reference.protect(STACK - 8192, 4096, 0).unwrap();
        let mut candidate = memory(&bytes);
        candidate.protect(DATA, 4096, protection).unwrap();
        candidate.protect(DATA + 4096, 4096, 0).unwrap();
        candidate.protect(STACK - 8192, 4096, 0).unwrap();
        let mut frame = Frame::from_cpu(&state);
        let mut interpreter = Interpreter::new();
        let mut count = 0;
        let expected = loop {
            let step = interpreter.step(&mut state, &mut reference);
            if step != Step::Executed {
                assert!(matches!(
                    step,
                    Step::ReadFault { .. } | Step::WriteFault { .. }
                ));
                break step;
            }
            count += 1;
            assert!(count < 200);
        };
        let reason = run(&code, route, &mut candidate, &mut frame, 200, false);
        assert_eq!(
            reason,
            if route == Route::Backend {
                HARDWARE
            } else {
                CHECK
            }
        );
        assert_eq!(frame.gpr, state.regs);
        assert_eq!(frame.flags, state.eflags);
        assert_eq!(frame.pc, state.eip);
        assert_eq!(frame.committed, count);
        let address = match expected {
            Step::ReadFault { address, .. } | Step::WriteFault { address, .. } => address,
            _ => unreachable!(),
        };
        assert_eq!(frame.address, address);
        let mut resumed = initial();
        frame.restore(&mut resumed);
        assert_eq!(interpreter.step(&mut resumed, &mut candidate), expected);
        if protection & READ != 0 {
            assert_eq!(
                candidate.read(DATA, 4096).unwrap(),
                reference.read(DATA, 4096).unwrap()
            );
        }
    }
}

#[test]
fn checked_failures_preserve_exact_prefix() {
    faults(Route::Proof);
    stack_faults(Route::Proof);
}

#[test]
fn arithmetic_flags_and_preserved_registers_match_edge_values() {
    arithmetic_states(Route::Proof);
}

fn arithmetic_states(route: Route) {
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let code = lower::compile(&graph, route, BASE, LENGTH, &BTreeSet::new(), false);
    for (index, value) in [0u32, 1, 7, 0x7fff_ffff, 0x8000_0000, u32::MAX, 0xffff_ff00]
        .into_iter()
        .enumerate()
    {
        let mut state = initial();
        state.eflags = 0x202 | ((index as u32 * 0x195) & 0x8d5);
        state.regs[2] = value.rotate_left(13);
        state.regs[3] = !value;
        let mut candidate = memory(&bytes);
        let mut reference = memory(&bytes);
        for memory in [&mut candidate, &mut reference] {
            memory.write(DATA, &value.to_le_bytes()).unwrap();
            memory
                .write(DATA + 8, &value.rotate_left(5).to_le_bytes())
                .unwrap();
        }
        let mut frame = Frame::from_cpu(&state);
        let mut interpreter = Interpreter::new();
        while graph.instructions.contains_key(&state.eip) {
            assert_eq!(interpreter.step(&mut state, &mut reference), Step::Executed);
        }
        assert_eq!(
            run(&code, route, &mut candidate, &mut frame, 512, false),
            DONE
        );
        assert_eq!(frame.gpr, state.regs);
        assert_eq!(frame.flags, state.eflags);
        assert_eq!(frame.pc, state.eip);
        assert_eq!(
            candidate.read(DATA, 16).unwrap(),
            reference.read(DATA, 16).unwrap()
        );
    }
}

fn stack_faults(route: Route) {
    for operation in 0..3 {
        let mut a = CodeAssembler::new(32).unwrap();
        a.add(eax, 19).unwrap();
        a.mov(esp, STACK - 4096).unwrap();
        match operation {
            0 => a.push(eax).unwrap(),
            1 => a.pop(ebx).unwrap(),
            _ => a.ret().unwrap(),
        }
        let bytes = a.assemble(u64::from(BASE)).unwrap();
        let graph = Graph::decode(&bytes, BASE).unwrap();
        let code = lower::compile(&graph, route, BASE, LENGTH, &BTreeSet::new(), false);
        let mut reference = memory(&bytes);
        let mut candidate = memory(&bytes);
        for memory in [&mut reference, &mut candidate] {
            memory.protect(STACK - 8192, 8192, 0).unwrap();
        }
        let initial = initial();
        let mut state = initial.clone();
        let mut interpreter = Interpreter::new();
        for _ in 0..2 {
            assert_eq!(interpreter.step(&mut state, &mut reference), Step::Executed);
        }
        let expected = interpreter.step(&mut state, &mut reference);
        assert!(matches!(
            expected,
            Step::ReadFault { .. } | Step::WriteFault { .. }
        ));
        let mut frame = Frame::from_cpu(&initial);
        assert_eq!(
            run(&code, route, &mut candidate, &mut frame, 20, false),
            if route == Route::Backend {
                HARDWARE
            } else {
                CHECK
            }
        );
        assert_eq!(frame.committed, 2);
        assert_eq!(frame.gpr, state.regs);
        assert_eq!(frame.flags, state.eflags);
        let mut resumed = initial;
        let address = match expected {
            Step::ReadFault { address, .. } | Step::WriteFault { address, .. } => address,
            _ => unreachable!(),
        };
        assert_eq!(frame.address, address);
        frame.restore(&mut resumed);
        assert_eq!(interpreter.step(&mut resumed, &mut candidate), expected);
    }
}

#[test]
fn interrupt_and_failed_entry_proof_do_not_commit() {
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let mut memory = memory(&bytes);
    let state = initial();
    memory.protect(DATA, 4096, 0).unwrap();
    assert!(memory::prove(&graph, &Frame::from_cpu(&state), &memory).is_empty());
    let code = lower::compile(&graph, Route::Proof, BASE, LENGTH, &BTreeSet::new(), false);
    let mut frame = Frame::from_cpu(&state);
    assert_eq!(
        run(&code, Route::Proof, &mut memory, &mut frame, 200, true),
        INTERRUPT
    );
    assert_eq!(frame, Frame::from_cpu(&state));
}

#[test]
fn asynchronous_interrupt_restores_the_observed_loop_prefix() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.inc(eax).unwrap();
    a.jmp(top).unwrap();
    let bytes = a.assemble(u64::from(BASE)).unwrap();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let code = lower::compile(&graph, Route::Proof, BASE, LENGTH, &BTreeSet::new(), false);
    let mut memory = memory(&bytes);
    let initial = initial();
    let mut frame = Frame::from_cpu(&initial);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(1));
            stop.store(true, std::sync::atomic::Ordering::Release);
        });
        let lease = memory::Lease::new(&mut memory);
        assert_eq!(
            unsafe {
                code.call(
                    &mut frame,
                    lease.memory.ptr.as_ptr(),
                    lease.memory.perms.as_ptr(),
                    &stop,
                    u32::MAX,
                )
            },
            INTERRUPT
        );
    });
    let mut reference = self::memory(&bytes);
    let mut state = initial;
    let mut interpreter = Interpreter::new();
    for _ in 0..frame.committed {
        assert_eq!(interpreter.step(&mut state, &mut reference), Step::Executed);
    }
    assert_eq!(frame.gpr, state.regs);
    assert_eq!(frame.flags, state.eflags);
    assert_eq!(frame.pc, state.eip);
}

#[test]
fn proofs_reject_wrap_special_pages_and_revocation() {
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let mut memory = memory(&bytes);
    let mut frame = Frame::from_cpu(&initial());
    assert!(!memory::prove(&graph, &frame, &memory).is_empty());
    let code = lower::compile(
        &graph,
        Route::Proof,
        BASE,
        LENGTH,
        &memory::prove(&graph, &frame, &memory),
        false,
    );
    memory.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
    assert!(memory::prove(&graph, &frame, &memory).is_empty());
    let before = frame.clone();
    assert_eq!(
        run(&code, Route::Proof, &mut memory, &mut frame, 200, false),
        CHECK
    );
    assert_eq!(before, frame);
    frame.gpr[6] = u32::MAX - 1;
    assert!(!memory::ordinary(&memory, frame.gpr[6], 4, false));
}

#[test]
#[ignore = "local diagnostic; set ADV_GIR_OUTPUT on the scratch volume"]
fn measure_synthetic() {
    let path = std::path::PathBuf::from(
        std::env::var_os("ADV_GIR_OUTPUT").expect("explicit local output"),
    );
    std::fs::create_dir_all(&path).unwrap();
    let bytes = fixture();
    let graph = Graph::decode(&bytes, BASE).unwrap();
    let mut memory = memory(&bytes);
    let proof = memory::prove(&graph, &Frame::from_cpu(&initial()), &memory);
    for (name, route, proven) in [
        ("A", Route::Backend, &BTreeSet::new()),
        ("B_checked", Route::Proof, &BTreeSet::new()),
        ("B_proven", Route::Proof, &proof),
    ] {
        let code = lower::compile(&graph, route, BASE, LENGTH, proven, true);
        std::fs::write(path.join(format!("{name}.vcode")), &code.vcode).unwrap();
        std::fs::write(path.join(format!("{name}.bin")), &code.machine).unwrap();
        std::fs::write(
            path.join(format!("{name}.blocks")),
            code.blocks
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        if let Some((_, text, machine, blocks)) = &code.cross {
            std::fs::write(path.join(format!("{name}.arm64.vcode")), text).unwrap();
            std::fs::write(path.join(format!("{name}.arm64.bin")), machine).unwrap();
            std::fs::write(
                path.join(format!("{name}.arm64.blocks")),
                blocks
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
        }
        println!(
            "gir-metric: {}",
            serde_json::json!({"route":name,"guest":graph.instructions.len(),"bytes":code.bytes,"ir":code.ir_count,"guard_edges":code.exits,"recovery_uses":code.recovery_uses,"explicit_spills":code.spill_stores,"compile_us":code.compile_us,"arm64_bytes":code.cross.as_ref().map(|p| p.0)})
        );
    }
    let without = lower::counterfactual(&graph, BASE, LENGTH, &proof);
    std::fs::write(path.join("B_without_recovery.vcode"), &without.vcode).unwrap();
    std::fs::write(path.join("B_without_recovery.bin"), &without.machine).unwrap();
    std::fs::write(
        path.join("B_without_recovery.blocks"),
        without
            .blocks
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    if let Some((_, text, machine, blocks)) = &without.cross {
        std::fs::write(path.join("B_without_recovery.arm64.vcode"), text).unwrap();
        std::fs::write(path.join("B_without_recovery.arm64.bin"), machine).unwrap();
        std::fs::write(
            path.join("B_without_recovery.arm64.blocks"),
            blocks
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
    }
    println!(
        "gir-counterfactual: {}",
        serde_json::json!({"bytes":without.bytes,"arm64_bytes":without.cross.as_ref().map(|p|p.0)})
    );
    let started = std::time::Instant::now();
    let baseline =
        crate::compile::compile(&crate::Region::new(BASE, bytes.len()), &mut memory, false)
            .unwrap();
    println!(
        "gir-metric: {}",
        serde_json::json!({"route":"baseline","guest":baseline.instruction_count,"bytes":baseline.code_size,"compile_us":started.elapsed().as_micros()})
    );
}
