//! Explicit local-only comparison on the existing private 24-region manifest.
//! Input: ADV_GIR_CORPUS. Output: anonymous JSON in ADV_GIR_OUTPUT; raw code
//! remains private in that same explicitly selected directory.
use super::*;
#[test]
#[ignore = "requires a private region manifest and output directory"]
fn compare_formal_lowering() {
    let file =
        std::path::PathBuf::from(std::env::var_os("ADV_GIR_CORPUS").expect("private corpus"));
    let out = std::path::PathBuf::from(std::env::var_os("ADV_GIR_OUTPUT").expect("private output"));
    std::fs::create_dir_all(&out).unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut result = vec![];
    for (n, row) in rows.as_array().unwrap().iter().enumerate() {
        let origin = row["origin"].as_u64().unwrap() as u32;
        let start = row["start"].as_u64().unwrap() as u32;
        let bytes =
            std::fs::read(file.parent().unwrap().join(row["file"].as_str().unwrap())).unwrap();
        let decoded: BTreeMap<_, _> = row["pcs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let pc = p.as_u64().unwrap() as u32;
                let i = Decoder::with_ip(
                    32,
                    &bytes[(pc - origin) as usize..],
                    u64::from(pc),
                    DecoderOptions::NONE,
                )
                .decode();
                (pc, i)
            })
            .collect();
        let base = origin & !4095;
        let len = (((origin - base) as usize + bytes.len() + 65535) & !4095).max(65536);
        let lifted = lift::lift(&decoded, start).unwrap();
        let formal = lower_cl::compile(&lifted.region, base, len, true).unwrap();
        let old_graph = Graph::from_instructions(decoded.values().copied(), start).unwrap();
        let old = lower::compile(&old_graph, Route::Proof, base, len, &BTreeSet::new(), true);
        let mut memory = crate::Memory::new(base, len).unwrap();
        memory
            .protect(base, len, crate::READ | crate::WRITE)
            .unwrap();
        memory.write(origin, &bytes).unwrap();
        memory
            .protect(
                base,
                (((origin - base) as usize + bytes.len() + 4095) & !4095).max(4096),
                crate::READ | crate::EXEC,
            )
            .unwrap();
        let end = decoded.values().map(Instruction::next_ip32).max().unwrap();
        let mut baseline_region = crate::Region::new(start, (end - start) as usize);
        baseline_region.reachable = true;
        baseline_region.entries = decoded.keys().copied().filter(|pc| *pc >= start).collect();
        baseline_region.frontier = decoded
            .values()
            .flat_map(|i| [i.next_ip32(), i.near_branch32()])
            .filter(|pc| !decoded.contains_key(pc))
            .collect();
        let directory = out.join(format!("baseline-{n}"));
        std::fs::create_dir_all(&directory).unwrap();
        unsafe {
            std::env::set_var("ADV_GIR_NATIVE", &directory);
            std::env::set_var("ADV_JIT_DUMP_DIR", &directory);
        }
        let baseline = crate::compile::compile(&baseline_region, &mut memory, false).unwrap();
        assert_eq!(baseline.instruction_count, decoded.len());
        let arm_bytes: usize = std::fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "bin")
                    && p.file_name().unwrap().to_string_lossy().contains("aarch64")
            })
            .map(|p| std::fs::metadata(p).unwrap().len() as usize)
            .sum();
        result.push(serde_json::json!({"case":n,"guest":decoded.len(),"baseline":{"x64":baseline.code_size,"arm64":arm_bytes},"r1":{"x64":old.bytes,"arm64":old.cross.as_ref().unwrap().0,"edges":old.recovery_uses / 9,"blocks":1},"g12":{"x64":formal.machine.len(),"arm64":formal.cross.as_ref().unwrap().0.len(),"edges":formal.cold_edges,"blocks":formal.cold_blocks,"flags_materialized":formal.flags_materialized}}));
    }
    unsafe {
        std::env::remove_var("ADV_GIR_NATIVE");
        std::env::remove_var("ADV_JIT_DUMP_DIR");
    }
    std::fs::write(
        out.join("comparison.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    eprintln!(
        "gir_g12_measure regions={} guest={}",
        result.len(),
        result
            .iter()
            .map(|r| r["guest"].as_u64().unwrap())
            .sum::<u64>()
    );
}
