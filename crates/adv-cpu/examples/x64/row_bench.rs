//! Reproducible row-runtime A/B benchmark over authored guest code.
//! cargo run -p adv-cpu --release --example row_bench -- 21
//! Only warmed Executor::run calls are timed; setup, register resets and host
//! copies are excluded. Every setting executes the same evolving frame sequence.
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::{hint::black_box, time::Instant};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
const BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;
const WARMUP: usize = 5;
const CODE: u32 = 0x10000;
const STOP: u32 = 0x11000;
const STACK: u32 = 0x12f00;
const FRAME: u32 = 0x13000;
const SOURCE: u32 = 0x20000;
const DESTINATION: u32 = SOURCE + BYTES as u32;
const END: u32 = DESTINATION + BYTES as u32;

fn program(blend: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut outer = a.create_label();
    let mut inner = a.create_label();
    a.set_label(&mut outer).unwrap();
    a.mov(ecx, dword_ptr(ebp)).unwrap();
    a.set_label(&mut inner).unwrap();
    if blend {
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(mm1, dword_ptr(edi)).unwrap();
        a.paddusb(mm0, mm1).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
    } else {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
    }
    a.add(esi, 4).unwrap();
    a.add(edi, 4).unwrap();
    a.dec(ecx).unwrap();
    a.jne(inner).unwrap();
    a.add(esi, dword_ptr(ebp + 4)).unwrap();
    a.add(edi, dword_ptr(ebp + 8)).unwrap();
    a.dec(ebx).unwrap();
    a.jne(outer).unwrap();
    if blend {
        a.emms().unwrap();
    }
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(
    code: &[u8],
    source: &[u8],
    destination: &[u8],
    workers: usize,
    threshold: Option<u64>,
) -> Executor {
    let mut m = Executor::new(CODE, (END - CODE) as usize).unwrap();
    m.protect(CODE, (END - CODE) as usize, READ | WRITE)
        .unwrap();
    m.write(CODE, code).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let parameters: Vec<_> = [WIDTH, 0, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    m.write(FRAME, &parameters).unwrap();
    m.write(SOURCE, source).unwrap();
    m.write(DESTINATION, destination).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.protect(FRAME, 4096, READ).unwrap();
    m.protect(SOURCE, BYTES, READ).unwrap();
    m.enable_auto_jit(false);
    m.set_row_workers(workers).unwrap();
    if let Some(threshold) = threshold {
        m.set_row_threshold(threshold);
    }
    m.add_region(Region::new(CODE, code.len())).unwrap();
    m
}

fn frame(m: &mut Executor) -> f64 {
    // Reset only architectural inputs. Guest output persists across frames,
    // including warmup, identically for every worker configuration.
    m.state = CpuState {
        eip: CODE,
        regs: [0, 0, 0, HEIGHT, STACK, FRAME, SOURCE, DESTINATION],
        ..CpuState::default()
    };
    let start = Instant::now();
    let result = m.run(STOP, 50_000_000);
    let milliseconds = start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(result.unwrap(), RunOutcome::ReachedStop);
    black_box(&m.state);
    milliseconds
}

pub fn main() {
    let samples = std::env::args().nth(1).map_or(21, |value| {
        value.parse::<usize>().expect("positive sample count")
    });
    assert!(samples > 0);
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let default_workers = cores.saturating_sub(2).min(3);
    println!(
        "# arch={},logical_cores={cores},warmup_frames={WARMUP}",
        std::env::consts::ARCH
    );
    println!(
        "workload,setting,width,height,samples,workers,threshold,median_ms,p95_ms,startup_ms,compile_ms,batches,rows,rejected,output_hash"
    );
    let source: Vec<_> = (0..BYTES)
        .map(|index| ((index * 7 + index / 97) & 3) as u8)
        .collect();
    let destination: Vec<_> = (0..BYTES)
        .map(|index| ((index * 13 + index / 19) & 31) as u8)
        .collect();
    for (blend, name) in [(false, "copy"), (true, "mmx_blend")] {
        let code = program(blend);
        // Compile and warm every setting before timing. Alternate the order
        // each frame so host load and frequency changes do not favor a setting.
        let settings = [
            ("serial", 0, Some(u64::MAX)),
            ("inline", 0, Some(0)),
            ("workers1", 1, Some(0)),
            ("workers2", 2, Some(0)),
            ("workers3", 3, Some(0)),
            ("default", default_workers, None),
        ];
        let mut machines: Vec<_> = settings
            .iter()
            .map(|&(_, workers, threshold)| {
                machine(&code, &source, &destination, workers, threshold)
            })
            .collect();
        let startups: Vec<_> = machines.iter_mut().map(frame).collect();
        for _ in 1..WARMUP {
            for m in &mut machines {
                frame(m);
            }
        }
        let before: Vec<_> = machines.iter().map(Executor::row_parallel_stats).collect();
        let mut timings = vec![Vec::with_capacity(samples); settings.len()];
        for round in 0..samples {
            for position in 0..settings.len() {
                let index = if round % 2 == 0 {
                    position
                } else {
                    settings.len() - 1 - position
                };
                timings[index].push(frame(&mut machines[index]));
            }
        }
        let reference = machines[0]
            .memory()
            .read(DESTINATION, BYTES)
            .unwrap()
            .to_vec();
        for (index, (setting, _, threshold)) in settings.into_iter().enumerate() {
            let m = &machines[index];
            let after = m.row_parallel_stats();
            let timings = &mut timings[index];
            timings.sort_by(f64::total_cmp);
            let output = m.memory().read(DESTINATION, BYTES).unwrap();
            assert_eq!(
                output, reference,
                "{name}/{setting}: output differs from serial"
            );
            let hash = output
                .iter()
                .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
                });
            let threshold_label = threshold.map_or_else(|| "default".into(), |n| n.to_string());
            println!(
                "{name},{setting},{WIDTH},{HEIGHT},{samples},{},{threshold_label},{:.4},{:.4},{:.4},{:.4},{},{},{},{hash:016x}",
                after.workers,
                timings[samples / 2],
                timings[(samples * 95).div_ceil(100) - 1],
                startups[index],
                m.stats.compile_time.as_secs_f64() * 1000.0,
                after.batches - before[index].batches,
                after.rows - before[index].rows,
                after.rejected - before[index].rejected
            );
            if threshold == Some(0) {
                assert!(after.batches > before[index].batches);
            }
        }
    }
}
