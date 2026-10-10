//! Private captured regions, selected offline; no samples are compiled into tests.
use super::*;
use crate::{
    CpuState, EXEC, Memory, READ, WRITE,
    interpreter::{Interpreter, Step},
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn load(value: &serde_json::Value, root: &Path) -> (Graph, Vec<u8>, u32) {
    let origin = value["origin"].as_u64().unwrap() as u32;
    let start = value["start"].as_u64().unwrap() as u32;
    let bytes = std::fs::read(root.join(value["file"].as_str().unwrap())).unwrap();
    let decoded = value["pcs"].as_array().unwrap().iter().map(|p| {
        let pc = p.as_u64().unwrap() as u32;
        Decoder::with_ip(
            32,
            &bytes[(pc - origin) as usize..],
            u64::from(pc),
            DecoderOptions::NONE,
        )
        .decode()
    });
    (
        Graph::from_instructions(decoded, start).unwrap(),
        bytes,
        origin,
    )
}

fn memory(bytes: &[u8], origin: u32) -> Memory {
    let base = origin & !4095;
    let len = ((origin - base) as usize + bytes.len() + 65535) & !4095;
    let mut memory = Memory::new(base, len.max(65536)).unwrap();
    memory.protect(base, memory.len, READ | WRITE).unwrap();
    memory.write(origin, bytes).unwrap();
    memory
        .protect(
            base,
            (((origin - base) as usize + bytes.len() + 4095) & !4095).max(4096),
            READ | EXEC,
        )
        .unwrap();
    memory
}

#[test]
#[ignore = "private local capture selection and code-generation measurements"]
fn select_and_measure() {
    let path = PathBuf::from(std::env::var_os("ADV_GIR_CANDIDATES").expect("local candidates"));
    let out = PathBuf::from(std::env::var_os("ADV_GIR_OUTPUT").expect("local output"));
    std::fs::create_dir_all(&out).unwrap();
    let candidates: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut accepted = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut rejected = BTreeMap::<String, usize>::new();
    for row in candidates.as_array().unwrap() {
        let engine = row["engine"].as_str().unwrap().to_string();
        if counts.get(&engine).copied().unwrap_or(0) >= 3 {
            continue;
        }
        let bytes =
            std::fs::read(path.parent().unwrap().join(row["file"].as_str().unwrap())).unwrap();
        let origin = row["origin"].as_u64().unwrap() as u32;
        let start = row["start"].as_u64().unwrap() as u32;
        let decoded = row["pcs"].as_array().unwrap().iter().map(|p| {
            let pc = p.as_u64().unwrap() as u32;
            Decoder::with_ip(
                32,
                &bytes[(pc - origin) as usize..],
                u64::from(pc),
                DecoderOptions::NONE,
            )
            .decode()
        });
        let Ok(graph) = Graph::from_instructions(decoded, start) else {
            *rejected.entry(engine).or_default() += 1;
            continue;
        };
        // Include nontrivial hot integer regions, not standalone dispatch RETs.
        if graph.instructions.len() < 4 {
            continue;
        }
        let ordinal = accepted.len();
        let mut saved = row.clone();
        saved["file"] = format!("case-{ordinal}.bin").into();
        std::fs::write(out.join(saved["file"].as_str().unwrap()), &bytes).unwrap();
        let mut memory = memory(&bytes, origin);
        let potential: BTreeSet<_> = graph
            .instructions
            .values()
            .filter(|i| graph.invariant_address(i, &Frame::default()).is_some())
            .map(Instruction::ip32)
            .collect();
        let mut metrics = Vec::new();
        for (name, route, proof) in [
            ("A", Route::Backend, BTreeSet::new()),
            ("B_checked", Route::Proof, BTreeSet::new()),
            ("B_proven", Route::Proof, potential),
        ] {
            let code = lower::compile(&graph, route, memory.base, memory.len, &proof, true);
            let key = format!("case-{ordinal}-{name}");
            std::fs::write(out.join(format!("{key}.bin")), &code.machine).unwrap();
            std::fs::write(out.join(format!("{key}.vcode")), &code.vcode).unwrap();
            std::fs::write(
                out.join(format!("{key}.blocks")),
                code.blocks
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
            if let Some((_, vcode, bytes, blocks)) = &code.cross {
                std::fs::write(out.join(format!("{key}.arm64.vcode")), vcode).unwrap();
                std::fs::write(out.join(format!("{key}.arm64.bin")), bytes).unwrap();
                std::fs::write(
                    out.join(format!("{key}.arm64.blocks")),
                    blocks
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .unwrap();
            }
            metrics.push(serde_json::json!({"route":name,"bytes":code.bytes,"arm64_bytes":code.cross.as_ref().map(|c|c.0),"us":code.compile_us,"ir":code.ir_count,"guards":code.exits,"recovery_uses":code.recovery_uses,"explicit_spills":code.spill_stores,"proven_sites":proof.len()}));
        }
        let end = graph
            .instructions
            .values()
            .map(Instruction::next_ip32)
            .max()
            .unwrap();
        let mut region = crate::Region::new(start, (end - start) as usize);
        region.reachable = true;
        region.entries = graph
            .instructions
            .keys()
            .copied()
            .filter(|pc| *pc >= start)
            .collect();
        region.frontier = graph
            .instructions
            .values()
            .flat_map(|i| [i.next_ip32(), i.near_branch32()])
            .filter(|pc| !graph.instructions.contains_key(pc))
            .collect();
        let directory = out.join(format!("baseline-{ordinal}"));
        std::fs::create_dir_all(&directory).unwrap();
        // This ignored diagnostic is run alone with one test thread.
        unsafe {
            std::env::set_var("ADV_GIR_NATIVE", &directory);
            std::env::set_var("ADV_JIT_DUMP_DIR", &directory);
        }
        let began = std::time::Instant::now();
        let baseline = crate::compile::compile(&region, &mut memory).unwrap();
        assert_eq!(
            baseline.instruction_count,
            graph.instructions.len(),
            "baseline must cover exactly the captured instruction set"
        );
        metrics.push(serde_json::json!({"route":"baseline","bytes":baseline.code_size,"guest":baseline.instruction_count,"us":began.elapsed().as_micros()}));
        saved["metrics"] = metrics.into();
        saved["guest"] = graph.instructions.len().into();
        accepted.push(saved);
        *counts.entry(engine).or_default() += 1;
    }
    std::fs::write(
        out.join("gir-corpus.json"),
        serde_json::to_vec_pretty(&accepted).unwrap(),
    )
    .unwrap();
    println!(
        "gir-corpus-selection: {}",
        serde_json::json!({"accepted":counts,"rejected_forms":rejected})
    );
}

#[test]
fn captured_regions_match_interpreter() {
    let file = std::env::var_os("ADV_GIR_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gir-corpus.json"));
    if !file.exists() {
        return;
    }
    let rows: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    if std::env::var_os("ADV_GIR_FAULT_CHILD").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gir::corpus::captured_regions_match_interpreter",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("ADV_GIR_FAULT_CHILD", "1")
            .env("ADV_GIR_CORPUS", file.canonicalize().unwrap())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "captured-region child failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        print!("{}", String::from_utf8_lossy(&result.stdout));
        return;
    }
    let mut units = 0u64;
    let mut checks = 0;
    for row in rows.as_array().unwrap() {
        let (graph, bytes, origin) = load(row, file.parent().unwrap());
        for route in [Route::Proof, Route::Backend] {
            for budget in [0, 1, 2, 4, 8, 16, 32, 64] {
                let mut memory = memory(&bytes, origin);
                let mut reference = self::memory(&bytes, origin);
                let mut initial = CpuState {
                    eip: graph.start,
                    eflags: 0x202,
                    ..CpuState::default()
                };
                initial.regs = [memory.base + 0x8000; 8];
                initial.regs[1] = 3;
                initial.regs[4] = memory.base + 0xc000;
                let mut frame = Frame::from_cpu(&initial);
                let proof = memory::prove(&graph, &frame, &memory);
                let code = lower::compile(&graph, route, memory.base, memory.len, &proof, false);
                let stop = AtomicBool::new(false);
                let status = if route == Route::Backend {
                    let window = memory::Window::new(&memory);
                    let (status, captured) = unsafe {
                        backend::run(&code, &window, || {
                            code.call(
                                &mut frame,
                                window.pointer(),
                                memory.perms.as_ptr(),
                                &stop,
                                budget,
                            )
                        })
                    };
                    if let Some(captured) = captured {
                        frame = captured;
                    }
                    window.copy_back(&mut memory);
                    status
                } else {
                    unsafe {
                        code.call(
                            &mut frame,
                            memory.ptr.as_ptr(),
                            memory.perms.as_ptr(),
                            &stop,
                            budget,
                        )
                    }
                };
                assert!(matches!(status, DONE | CHECK | BUDGET | HARDWARE));
                let mut state = initial;
                let mut interpreter = Interpreter::new();
                for _ in 0..frame.committed {
                    assert_eq!(interpreter.step(&mut state, &mut reference), Step::Executed);
                }
                assert_eq!(frame.gpr, state.regs);
                assert_eq!(frame.flags, state.eflags);
                assert_eq!(frame.pc, state.eip);
                assert_eq!(
                    memory.read(memory.base, memory.len).unwrap(),
                    reference.read(reference.base, reference.len).unwrap()
                );
                units += u64::from(frame.committed);
                checks += 1;
            }
        }
    }
    println!(
        "gir-corpus-execution: regions={} checks={checks} committed={units}",
        rows.as_array().unwrap().len()
    );
}

/// Compare native compilation with cross compilation and file output disabled.
/// Input is the private accepted manifest; output contains only aggregate times.
#[test]
#[ignore = "local native compilation timing; run alone with ADV_GIR_CORPUS"]
fn measure_native_compilation() {
    for name in [
        "ADV_GIR_NATIVE",
        "ADV_GIR_CAPTURE",
        "ADV_JIT_DUMP_DIR",
        "ADV_JIT_IR_COUNTS",
    ] {
        // This ignored diagnostic requires an isolated single-test process.
        unsafe { std::env::remove_var(name) };
    }
    let file = PathBuf::from(std::env::var_os("ADV_GIR_CORPUS").expect("local corpus"));
    let rows: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut totals = [0u128; 4];
    for row in rows.as_array().unwrap() {
        let (graph, bytes, origin) = load(row, file.parent().unwrap());
        let mut memory = memory(&bytes, origin);
        let proof: BTreeSet<_> = graph
            .instructions
            .values()
            .filter(|i| graph.invariant_address(i, &Frame::default()).is_some())
            .map(Instruction::ip32)
            .collect();
        let end = graph
            .instructions
            .values()
            .map(Instruction::next_ip32)
            .max()
            .unwrap();
        let mut region = crate::Region::new(graph.start, (end - graph.start) as usize);
        region.reachable = true;
        region.entries = graph
            .instructions
            .keys()
            .copied()
            .filter(|pc| *pc >= graph.start)
            .collect();
        region.frontier = graph
            .instructions
            .values()
            .flat_map(|i| [i.next_ip32(), i.near_branch32()])
            .filter(|pc| !graph.instructions.contains_key(pc))
            .collect();
        let mut times = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for _ in 0..3 {
            for (n, route, proven) in [
                (0, Route::Backend, &BTreeSet::new()),
                (1, Route::Proof, &BTreeSet::new()),
                (2, Route::Proof, &proof),
            ] {
                let code = lower::compile(&graph, route, memory.base, memory.len, proven, false);
                times[n].push(code.compile_us);
            }
            let began = std::time::Instant::now();
            let code = crate::compile::compile(&region, &mut memory).unwrap();
            times[3].push(began.elapsed().as_micros());
            assert_eq!(code.instruction_count, graph.instructions.len());
        }
        for (n, mut samples) in times.into_iter().enumerate() {
            samples.sort_unstable();
            totals[n] += samples[1];
        }
    }
    println!(
        "gir-native-compilation: {}",
        serde_json::json!({
            "regions": rows.as_array().unwrap().len(), "median_us_sum": totals,
            "order": ["A", "B_checked", "B_proven", "baseline"], "repetitions": 3
        })
    );
}
