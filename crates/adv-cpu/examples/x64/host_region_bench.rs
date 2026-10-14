//! Headless region benchmark: cargo run -p adv-cpu --release --example host_region_bench.
//! Inputs: optional ADV_GIR_LIFT_CORPUS (engine subdirectories of private .tsv/.bin
//! captures), ADV_REGION_BENCH_SAMPLES/LOOPS/REPEATS/PRIVATE_LIMIT. Output: anonymous
//! CSV statistics, OS capabilities and current CPU; never code, PCs or file names.
//! Compilation and one full-state differential warmup are outside measurement.
//! ADV_REGION_BENCH_COMPILE=1 measures Fast/Speed synchronous compilation with
//! 1/2/4 host compiler workers (not the background service's scheduling policy).
//! COMPILE_REPEATS (1-31, default 9) bounds jobs per case per worker count.
//! ADV_REGION_BENCH_TIER0_ONLY=1 and ADV_REGION_BENCH_CATEGORY filter sampling
//! workloads.
//! Short synthetic comparisons can use ADV_REGION_BENCH_SAMPLES=3,
//! ADV_REGION_BENCH_LOOPS=1000 and ADV_REGION_BENCH_REPEATS=1.
use adv_cpu::{
    ArchitecturalState, CompileStrategy, CpuState, EXEC, Executor, HostFeatures, READ, Region,
    RunOutcome, WRITE,
};
use iced_x86::{
    Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, Mnemonic as M,
    code_asm::*,
};
use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f000;
const STOP: u32 = 0x30000;
const BUDGET: u64 = 100_000_000;

#[derive(Clone)]
struct Case {
    category: String,
    ordinal: usize,
    bytes: Vec<u8>,
    stop: u32,
    repeats: usize,
    memory_movs: u64,
}

struct Ready {
    case: Case,
    executors: Vec<(u8, Executor)>,
    initial: CpuState,
    units: u64,
    expected: ArchitecturalState,
    data: Vec<u8>,
}

fn option(name: &str, default: usize, max: usize) -> usize {
    let n = std::env::var(name).map_or(default, |s| s.parse().expect("positive integer option"));
    assert!(n > 0 && n <= max, "option outside bounded range");
    n
}

fn data_seed(category: &str) -> Vec<u8> {
    let mut data = vec![0; 4096];
    data[0..8].copy_from_slice(&1.0f64.to_le_bytes());
    data[8..16].copy_from_slice(&0.25f64.to_le_bytes());
    if category == "synthetic_memory_scalar" {
        for start in [512, 1024, 1536] {
            for (index, byte) in data[start..start + 256].iter_mut().enumerate() {
                *byte = (index * 37 + 13) as u8;
            }
        }
    }
    data
}

fn seed() -> CpuState {
    let mut state = CpuState {
        eip: CODE,
        eflags: 0x202,
        regs: [3, 5, 7, 11, STACK, 13, DATA, 17],
        ..CpuState::default()
    };
    for lane in 0..4 {
        state.xmm[0][lane * 4..lane * 4 + 4].copy_from_slice(&1.0f32.to_le_bytes());
        state.xmm[1][lane * 4..lane * 4 + 4].copy_from_slice(&0.25f32.to_le_bytes());
    }
    state
}

fn synthetic(loops: u32, repeats: usize) -> Vec<Case> {
    let mut cases = Vec::new();
    for category in [
        "alu_flags",
        "cmp_jcc",
        "sse_scalar",
        "sse_packed",
        "memory",
        "memory_scalar",
        "x87",
    ] {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut top = a.create_label();
        a.mov(ecx, loops).unwrap();
        a.set_label(&mut top).unwrap();
        for _ in 0..4 {
            match category {
                "alu_flags" => {
                    a.add(eax, 3).unwrap();
                    a.adc(edx, eax).unwrap();
                    a.xor(ebx, edx).unwrap();
                    a.sbb(eax, 1).unwrap();
                }
                "cmp_jcc" => {
                    let mut next = a.create_label();
                    a.cmp(eax, edx).unwrap();
                    a.jbe(next).unwrap();
                    a.add(eax, 1).unwrap();
                    a.set_label(&mut next).unwrap();
                    a.xor(edx, 1).unwrap();
                }
                "sse_scalar" => {
                    a.addss(xmm0, xmm1).unwrap();
                    a.subss(xmm0, xmm1).unwrap();
                }
                "sse_packed" => {
                    a.addps(xmm0, xmm1).unwrap();
                    a.subps(xmm0, xmm1).unwrap();
                }
                "memory" => {
                    a.mov(eax, dword_ptr(esi + 64)).unwrap();
                    a.add(eax, 1).unwrap();
                    a.mov(dword_ptr(esi + 64), eax).unwrap();
                    a.xor(edx, eax).unwrap();
                }
                "memory_scalar" => {
                    // Separate source/destination ranges and a changing base
                    // retain all six scalar accesses through slot forwarding.
                    a.movzx(eax, byte_ptr(esi + 512)).unwrap();
                    a.mov(byte_ptr(esi + 2048), al).unwrap();
                    a.movzx(eax, word_ptr(esi + 1024)).unwrap();
                    a.mov(word_ptr(esi + 2560), ax).unwrap();
                    a.mov(eax, dword_ptr(esi + 1536)).unwrap();
                    a.add(eax, 1).unwrap();
                    a.mov(dword_ptr(esi + 3072), eax).unwrap();
                    a.add(esi, 16).unwrap();
                    a.and(esi, 255).unwrap();
                    a.or(esi, DATA).unwrap();
                }
                "x87" => {
                    a.fld(qword_ptr(esi)).unwrap();
                    a.fadd(qword_ptr(esi + 8)).unwrap();
                    a.fstp(qword_ptr(esi + 16)).unwrap();
                }
                _ => unreachable!(),
            }
        }
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        cases.push(Case {
            category: format!("synthetic_{category}"),
            ordinal: 1,
            bytes: a.assemble(u64::from(CODE)).unwrap(),
            stop: STOP,
            repeats,
            memory_movs: match category {
                "memory" => 8 * u64::from(loops),
                "memory_scalar" => 24 * u64::from(loops),
                _ => 0,
            },
        });
    }
    // Authored short fragments expose entry/validation costs without requiring
    // private captures. They are labelled separately from real engine slices.
    for instructions in [8, 16, 32] {
        let mut a = CodeAssembler::new(32).unwrap();
        for _ in 0..instructions / 4 {
            a.add(eax, 3).unwrap();
            a.adc(edx, eax).unwrap();
            a.xor(ebx, edx).unwrap();
            a.sbb(eax, 1).unwrap();
        }
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        cases.push(Case {
            category: format!("synthetic_short_{instructions}"),
            ordinal: 1,
            stop: CODE + bytes.len() as u32,
            bytes,
            repeats: 1000,
            memory_movs: 0,
        });
    }
    cases
}

fn percentiles(mut us: Vec<f64>) -> (f64, f64, f64) {
    assert!(!us.is_empty());
    us.sort_by(f64::total_cmp);
    (
        us[(us.len() - 1) / 2],
        us[(us.len() * 95).div_ceil(100) - 1],
        *us.last().unwrap(),
    )
}

fn benchmark_compilation(cases: &[Case]) {
    let repeats = option("ADV_REGION_BENCH_COMPILE_REPEATS", 9, 31);
    println!(
        "# compile topology=synchronous_host_workers setup_excluded=true publication_in_throughput=true repeats={repeats}"
    );
    println!("kind,category,strategy,threads,regions,p50_us,p95_us,max_us,regions_per_second");
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for threads in [1, 2, 4] {
            let barrier = std::sync::Barrier::new(threads);
            let results = std::thread::scope(|scope| {
                let workers: Vec<_> = (0..threads)
                    .map(|worker| {
                        let barrier = &barrier;
                        scope.spawn(move || {
                            let jobs: Vec<_> = (0..cases.len() * repeats)
                                .filter(|n| n % threads == worker)
                                .map(|n| {
                                    let index = n % cases.len();
                                    let executor = setup(&cases[index], 0).unwrap();
                                    (index, executor)
                                })
                                .collect();
                            barrier.wait();
                            let start = Instant::now();
                            let mut timings = Vec::new();
                            // Setup and guest execution are outside this window.
                            // Stats time includes capture, lowering and publication
                            // preparation, but excludes the owner's cache insertion.
                            let mut finished = Vec::new();
                            for (index, mut executor) in jobs {
                                let mut region = Region::new(CODE, cases[index].bytes.len());
                                region.compile_strategy = strategy;
                                executor.add_region(region).unwrap();
                                timings
                                    .push((index, executor.stats.compile_time.as_secs_f64() * 1e6));
                                finished.push(executor); // Keep retirement outside throughput.
                            }
                            (start.elapsed().as_secs_f64(), timings)
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|w| w.join().unwrap())
                    .collect::<Vec<_>>()
            });
            let elapsed = results.iter().map(|(time, _)| *time).fold(0.0, f64::max);
            let mut categories = BTreeMap::<&str, Vec<f64>>::new();
            for (_, timings) in results {
                for (index, us) in timings {
                    categories
                        .entry(&cases[index].category)
                        .or_default()
                        .push(us);
                }
            }
            let all: Vec<_> = categories.values().flatten().copied().collect();
            let count = all.len();
            let (p50, p95, max) = percentiles(all);
            println!(
                "compile_total,all,{strategy:?},{threads},{count},{p50:.3},{p95:.3},{max:.3},{:.3}",
                count as f64 / elapsed
            );
            for (category, times) in categories {
                let count = times.len();
                let (p50, p95, max) = percentiles(times);
                println!(
                    "compile,{category},{strategy:?},{threads},{count},{p50:.3},{p95:.3},{max:.3},"
                );
            }
        }
    }
}

fn replayable(i: &Instruction, info: &mut InstructionInfoFactory) -> bool {
    !i.is_invalid()
        && i.flow_control() == FlowControl::Next
        && info.info(i).used_memory().is_empty()
        && matches!(
            i.mnemonic(),
            M::Mov
                | M::Movzx
                | M::Movsx
                | M::Lea
                | M::Add
                | M::Adc
                | M::Sub
                | M::Sbb
                | M::And
                | M::Or
                | M::Xor
                | M::Not
                | M::Neg
                | M::Inc
                | M::Dec
                | M::Cmp
                | M::Test
                | M::Imul
                | M::Shl
                | M::Shr
                | M::Sar
                | M::Rol
                | M::Ror
                | M::Bswap
                | M::Xchg
                | M::Addss
                | M::Subss
                | M::Mulss
                | M::Divss
                | M::Addps
                | M::Subps
                | M::Mulps
                | M::Divps
                | M::Addsd
                | M::Subsd
                | M::Mulsd
                | M::Divsd
                | M::Addpd
                | M::Subpd
                | M::Mulpd
                | M::Divpd
                | M::Movaps
                | M::Movups
                | M::Movdqa
                | M::Movdqu
                | M::Pxor
                | M::Pand
                | M::Por
                | M::Paddd
                | M::Psubd
                | M::Xorps
                | M::Xorpd
                | M::Andps
                | M::Andpd
        )
}

/// Captures lack runtime state. Select the first bounded contiguous register-only
/// slice with at least four instructions. Startup captures often alternate small
/// register runs with memory accesses. Never infer missing memory or branches.
fn private_slice(manifest: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let mut lines = manifest.lines();
    let header: Vec<u32> = lines
        .next()?
        .split('\t')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if header.len() != 4 || bytes.len() > 1024 * 1024 {
        return None;
    }
    let pcs: Vec<u32> = lines
        .next()?
        .split('\t')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if pcs.len() != header[3] as usize || pcs.windows(2).any(|p| p[0] >= p[1]) {
        return None;
    }
    let mut info = InstructionInfoFactory::new();
    let mut start = 0;
    let mut end = 0;
    let mut count = 0;
    for pc in pcs {
        let offset = pc.checked_sub(header[1])? as usize;
        let tail = bytes.get(offset..)?;
        let i = Decoder::with_ip(32, tail, u64::from(pc), DecoderOptions::NONE).decode();
        if replayable(&i, &mut info) {
            if count != 0 && offset != end && count >= 4 {
                return Some(bytes.get(start..end)?.to_vec());
            }
            if count == 0 || offset != end {
                start = offset;
                count = 0;
            }
            count += 1;
            end = offset + i.len();
            if count == 32 {
                return Some(bytes.get(start..end)?.to_vec());
            }
        } else {
            if count >= 4 {
                return Some(bytes.get(start..end)?.to_vec());
            }
            count = 0;
        }
    }
    (count >= 4).then(|| bytes[start..end].to_vec())
}

fn load_private(root: &Path, limit: usize, repeats: usize) -> Vec<Case> {
    // Only known engine labels may leave this loader. Directory/file names stay private.
    let mut cases = Vec::new();
    let mut directories: Vec<_> = std::fs::read_dir(root)
        .expect("private corpus directory")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    directories.sort_by_key(|e| e.file_name());
    for category in [
        "advhd", "bgi", "cat", "kirikiri", "majiro", "pjadv", "softpal",
    ] {
        let mut candidates = 0;
        let mut selected = 0;
        let mut seen = BTreeSet::new();
        for dir in &directories {
            let name = dir.file_name();
            let name = name.to_string_lossy();
            if name != category && !name.starts_with(&format!("{category}-")) {
                continue;
            }
            let mut manifests: Vec<_> = std::fs::read_dir(dir.path())
                .expect("capture category")
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
                .collect();
            manifests.sort();
            for path in manifests {
                if selected == limit {
                    break;
                }
                candidates += 1;
                let Ok(manifest) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(bytes) = std::fs::read(path.with_extension("bin")) else {
                    continue;
                };
                let Some(bytes) = private_slice(&manifest, &bytes) else {
                    continue;
                };
                if !seen.insert(bytes.clone()) {
                    continue;
                }
                selected += 1;
                // Optional local staging contains private captures, never report data.
                // Reuse the exact selected inputs on another host without transferring
                // thousands of files or relying on private original file names.
                if let Some(export) = std::env::var_os("ADV_REGION_BENCH_EXPORT_CORPUS") {
                    let export = std::path::PathBuf::from(export).join(category);
                    assert!(
                        std::fs::create_dir_all(&export).is_ok(),
                        "corpus staging failed"
                    );
                    assert!(
                        std::fs::copy(&path, export.join(format!("{selected:03}.tsv"))).is_ok(),
                        "corpus staging failed"
                    );
                    assert!(
                        std::fs::copy(
                            path.with_extension("bin"),
                            export.join(format!("{selected:03}.bin"))
                        )
                        .is_ok(),
                        "corpus staging failed"
                    );
                }
                cases.push(Case {
                    category: category.into(),
                    ordinal: selected,
                    stop: CODE + bytes.len() as u32,
                    bytes,
                    repeats,
                    memory_movs: 0,
                });
            }
            if selected == limit {
                break;
            }
        }
        println!("# corpus category={category} examined={candidates} selected={selected}");
    }
    cases
}

fn setup(case: &Case, tier: u8) -> Result<Executor, String> {
    let mut m = Executor::new(CODE, 0x21000)?;
    m.disable_row_batching();
    m.set_tier_verification(None);
    m.protect(CODE, 0x21000, READ | WRITE)?;
    m.write(CODE, &case.bytes)?;
    m.write(DATA, &data_seed(&case.category))?;
    m.write(STACK, &STOP.to_le_bytes())?;
    m.protect(CODE, 4096, READ | EXEC)?;
    m.protect(STOP, 4096, READ | EXEC)?;
    m.enable_auto_jit(false);
    m.set_async_compilation(false);
    m.set_tiered_compilation(false);
    if tier >= 2 {
        m.add_region(Region::new(CODE, case.bytes.len()))?;
    }
    Ok(m)
}

fn run(m: &mut Executor, initial: &CpuState, stop: u32) -> Result<(), String> {
    m.state = initial.clone();
    if m.run(stop, BUDGET)? != RunOutcome::ReachedStop {
        return Err("incomplete".into());
    }
    black_box(&m.state);
    Ok(())
}

fn prepare(case: Case, tiers: &[u8]) -> Result<Ready, &'static str> {
    let initial = seed();
    let mut oracle = setup(&case, 0).map_err(|_| "setup")?;
    run(&mut oracle, &initial, case.stop).map_err(|_| "oracle_execution")?;
    let units = oracle.stats.interpreter_instructions;
    if units == 0 {
        return Err("empty");
    }
    let expected = oracle.state.capture_architecture();
    let data = oracle
        .memory()
        .read(DATA, 4096)
        .map_err(|_| "data")?
        .to_vec();
    let mut executors = Vec::new();
    for &tier in tiers {
        let mut m = setup(&case, tier).map_err(|_| "setup_or_compile")?;
        run(&mut m, &initial, case.stop).map_err(|_| "execution")?;
        if m.state.capture_architecture() != expected
            || m.memory().read(DATA, 4096).map_err(|_| "data")?.as_ref() != data
        {
            return Err("semantic_mismatch");
        }
        executors.push((tier, m));
    }
    Ok(Ready {
        case,
        executors,
        initial,
        units,
        expected,
        data,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn cpu() -> i32 {
    // SAFETY: sched_getcpu has no pointer arguments or side effects.
    unsafe { libc::sched_getcpu() }
}
#[cfg(windows)]
fn cpu() -> i32 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessorNumber() -> u32;
    }
    // SAFETY: reads the current processor number without pointer arguments.
    unsafe { GetCurrentProcessorNumber() as i32 }
}
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn cpu() -> i32 {
    -1
}

pub fn main() {
    let samples = option("ADV_REGION_BENCH_SAMPLES", 7, 31);
    let loops = option("ADV_REGION_BENCH_LOOPS", 10_000, 100_000) as u32;
    let repeats = option("ADV_REGION_BENCH_REPEATS", 4, 1000);
    let private_limit = option("ADV_REGION_BENCH_PRIVATE_LIMIT", 3, 16);
    println!(
        "# host_features detected={} enabled={} cpu={} samples={samples} loops={loops} repeats={repeats} private_limit={private_limit}",
        HostFeatures::detected(),
        HostFeatures::get(),
        cpu()
    );
    let mut tiers: Vec<u8> = vec![0, 2];
    if std::env::var("ADV_REGION_BENCH_TIER0_ONLY").as_deref() == Ok("1") {
        tiers = vec![0];
    }
    let mut cases = synthetic(loops, repeats);
    if let Some(root) = std::env::var_os("ADV_GIR_LIFT_CORPUS") {
        cases.extend(load_private(Path::new(&root), private_limit, 1000));
    }
    let filtered = if let Ok(category) = std::env::var("ADV_REGION_BENCH_CATEGORY") {
        cases.retain(|case| case.category == category);
        true
    } else {
        false
    };
    assert!(!cases.is_empty(), "empty benchmark selection");
    if std::env::var("ADV_REGION_BENCH_COMPILE").as_deref() == Ok("1") {
        benchmark_compilation(&cases);
        return;
    }
    println!(
        "kind,category,ordinal,tier,guest_instructions_per_run,repeats,median_ns_per_guest,range_ns_per_guest,min_ns_per_guest,max_ns_per_guest,fallback_percent,cpu_start,cpu_end"
    );
    let mut aggregates = BTreeMap::<(String, u8), (Vec<f64>, u64)>::new();
    let mut accepted = 0;
    let mut skipped = 0;
    for case in cases {
        let label = (case.category.clone(), case.ordinal);
        let mut ready = match prepare(case, &tiers) {
            Ok(ready) => ready,
            Err(reason) => {
                assert!(
                    !label.0.starts_with("synthetic_"),
                    "synthetic validation failed: {reason}"
                );
                println!(
                    "# skipped category={} ordinal={} reason={reason}",
                    label.0, label.1
                );
                skipped += 1;
                continue;
            }
        };
        accepted += 1;
        let mut timings = vec![vec![0.; samples]; tiers.len()];
        let mut fallback = vec![0; tiers.len()];
        let native_before: Vec<_> = ready
            .executors
            .iter()
            .map(|(_, m)| m.stats.native_entries)
            .collect();
        let cpu_start = cpu();
        // Rotate the tier order to distribute gradual thermal/frequency drift.
        for sample in 0..samples {
            for offset in 0..tiers.len() {
                let index = (sample + offset) % tiers.len();
                let (_, m) = &mut ready.executors[index];
                let before = m.stats.interpreter_instructions;
                assert!(
                    m.write(DATA, &data_seed(&ready.case.category)).is_ok(),
                    "data reset failed"
                );
                // CPU reset and dispatch are included. The original memory workload
                // needs a four-byte data reset per run; other data is immutable
                // or overwritten with the same result on every invocation.
                let start = Instant::now();
                for _ in 0..ready.case.repeats {
                    if ready.case.category == "synthetic_memory" {
                        assert!(m.write(DATA + 64, &[0; 4]).is_ok(), "data reset failed");
                    }
                    assert!(
                        run(m, &ready.initial, ready.case.stop).is_ok(),
                        "validated execution failed"
                    );
                }
                timings[index][sample] = start.elapsed().as_secs_f64() * 1e9;
                fallback[index] += m.stats.interpreter_instructions - before;
                assert!(
                    m.state.capture_architecture() == ready.expected,
                    "anonymous state mismatch"
                );
                assert!(
                    m.memory().read(DATA, 4096).unwrap().as_ref() == ready.data,
                    "anonymous memory mismatch"
                );
            }
        }
        let cpu_end = cpu();
        let units = ready.units * ready.case.repeats as u64;
        for (index, &tier) in tiers.iter().enumerate() {
            let entry = aggregates
                .entry((ready.case.category.clone(), tier))
                .or_insert((vec![0.; samples], 0));
            for (total, elapsed) in entry.0.iter_mut().zip(&timings[index]) {
                *total += elapsed;
            }
            entry.1 += units;
            let mut ns: Vec<_> = timings[index].iter().map(|t| t / units as f64).collect();
            ns.sort_by(f64::total_cmp);
            println!(
                "region,{},{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{cpu_start},{cpu_end}",
                ready.case.category,
                ready.case.ordinal,
                tier,
                ready.units,
                ready.case.repeats,
                ns[samples / 2],
                ns[samples - 1] - ns[0],
                ns[0],
                ns[samples - 1],
                fallback[index] as f64 * 100. / (units * samples as u64) as f64
            );
            let m = &ready.executors[index].1;
            println!(
                "# execution category={} ordinal={} tier={tier} native_code_bytes={} native_entries_measured={} memory_movs_per_run={} measured_runs={}",
                ready.case.category,
                ready.case.ordinal,
                m.stats.native_code_bytes,
                m.stats.native_entries - native_before[index],
                ready.case.memory_movs,
                samples * ready.case.repeats
            );
        }
    }
    for ((category, tier), (mut timings, units)) in aggregates {
        timings.iter_mut().for_each(|t| *t /= units as f64);
        timings.sort_by(f64::total_cmp);
        println!(
            "summary,{category},0,{tier},{units},1,{:.3},{:.3},{:.3},{:.3},,,",
            timings[samples / 2],
            timings[samples - 1] - timings[0],
            timings[0],
            timings[samples - 1]
        );
    }
    println!(
        "# completed accepted={accepted} skipped={skipped} cpu={}",
        cpu()
    );
    assert!(
        accepted >= if filtered { 1 } else { 6 },
        "synthetic baseline incomplete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compilation_percentiles_use_nearest_rank_and_keep_maximum() {
        assert_eq!(percentiles(vec![10., 1., 2., 3.]), (2., 10., 10.));
        assert_eq!(percentiles(vec![7.]), (7., 7., 7.));
    }

    #[test]
    fn authored_short_cases_have_exact_instruction_counts_and_stop_boundaries() {
        for case in synthetic(1, 1)
            .into_iter()
            .filter(|c| c.category.starts_with("synthetic_short_"))
        {
            let expected: usize = case.category.rsplit('_').next().unwrap().parse().unwrap();
            assert_eq!(
                Decoder::new(32, &case.bytes, DecoderOptions::NONE)
                    .into_iter()
                    .count(),
                expected
            );
            assert_eq!(case.stop, CODE + case.bytes.len() as u32);
            assert_eq!(case.memory_movs, 0);
        }
    }

    #[test]
    fn private_loader_rejects_missing_or_nonmonotonic_manifest() {
        assert!(private_slice("invalid", &[]).is_none());
        assert!(private_slice("1\t1\t1\t2\n2\t1\n", &[]).is_none());
    }

    #[test]
    fn implicit_memory_and_nondeterministic_instructions_are_excluded() {
        let mut info = InstructionInfoFactory::new();
        for build in [
            (|a: &mut CodeAssembler| a.push(eax)) as fn(&mut CodeAssembler) -> _,
            |a| a.rdtsc(),
            |a| a.mov(eax, dword_ptr(esi)),
        ] {
            let mut a = CodeAssembler::new(32).unwrap();
            build(&mut a).unwrap();
            let bytes = a.assemble(u64::from(CODE)).unwrap();
            let i = Decoder::new(32, &bytes, DecoderOptions::NONE).decode();
            assert!(!replayable(&i, &mut info));
        }
    }

    #[test]
    fn anonymous_slice_is_bounded_and_relocated_without_address_output() {
        let mut a = CodeAssembler::new(32).unwrap();
        for _ in 0..40 {
            a.add(eax, edx).unwrap();
        }
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        let pcs = Decoder::with_ip(32, &bytes, u64::from(CODE), 0)
            .into_iter()
            .map(|i| i.ip().to_string())
            .collect::<Vec<_>>();
        let manifest = format!("{CODE}\t{CODE}\t{}\t40\n{}\n", bytes.len(), pcs.join("\t"));
        let slice = private_slice(&manifest, &bytes).unwrap();
        assert_eq!(Decoder::new(32, &slice, 0).into_iter().count(), 32);
    }

    #[test]
    fn short_register_runs_need_four_instructions() {
        for count in [3, 4] {
            let mut a = CodeAssembler::new(32).unwrap();
            for _ in 0..count {
                a.add(eax, edx).unwrap();
            }
            let bytes = a.assemble(u64::from(CODE)).unwrap();
            let pcs = Decoder::with_ip(32, &bytes, u64::from(CODE), 0)
                .into_iter()
                .map(|i| i.ip().to_string())
                .collect::<Vec<_>>();
            let manifest = format!(
                "{CODE}\t{CODE}\t{}\t{count}\n{}\n",
                bytes.len(),
                pcs.join("\t")
            );
            assert_eq!(private_slice(&manifest, &bytes).is_some(), count == 4);
        }
    }
}
