//! Authored x86 copies/comparisons against device and lazy/COW file storage.
//! Opens no window, audio device, or real guest program.
use adv_cpu::backend::{FilePage, Guest, Prot};
use adv_cpu::{CpuState, JitCpu, Region, RunOutcome};
use iced_x86::code_asm::*;
use std::time::Instant;

const CODE: u32 = 0x10000;
const STOP: u32 = CODE + 0x1000;
const SOURCE: u32 = 0x100000;
const DEST: u32 = 0x200000;
const MIRROR: u32 = 0x300000;
const STACK: u32 = 0x4ff00;
const BYTES: usize = 65536;

fn code(operation: &str) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    if operation == "rep_movsd" {
        a.rep().movsd().unwrap();
    } else if operation == "rep_cmpsb" {
        a.repe().cmpsb().unwrap();
    } else {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        let stride = if operation == "sse_loop" {
            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
            16
        } else {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            4
        };
        a.add(esi, stride).unwrap();
        a.add(edi, stride).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    }
    a.ret().unwrap();
    a.assemble(CODE as u64).unwrap()
}

fn main() {
    let samples: usize = std::env::args().nth(1).map_or(9, |n| n.parse().unwrap());
    let filter = std::env::args().nth(2);
    assert!(samples > 0);
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
        }
        assert_ne!(unsafe { SetProcessAffinityMask(GetCurrentProcess(), 1) }, 0);
    }
    println!(
        "operation,storage,bytes,median_ms,interpreter_instructions,native_entries,native_code_bytes"
    );
    for operation in ["dword_loop", "sse_loop", "rep_movsd", "rep_cmpsb"] {
        if filter.as_deref().is_some_and(|filter| filter != operation) {
            continue;
        }
        let bytes = code(operation);
        for storage in ["ordinary", "pcm", "file", "file_cow"] {
            let mut cpu = JitCpu::new(STOP, false).unwrap();
            for address in [CODE, STOP] {
                cpu.map(address, 4096, Prot::RX).unwrap();
            }
            cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
            let expected: Vec<_> = (0..BYTES).map(|i| ((i * 17 + 37) % 251) as u8).collect();
            let file = storage.starts_with("file");
            let callback = if file {
                let source: Vec<_> = expected
                    .as_chunks::<4096>()
                    .0
                    .iter()
                    .map(|p| {
                        let data = *p;
                        FilePage::lazy(move || Ok(data))
                    })
                    .collect();
                let destination: Vec<_> = (0..BYTES / 4096)
                    .map(|_| FilePage::lazy(|| Ok([0; 4096])))
                    .collect();
                cpu.map_file_pages(SOURCE, &source, Prot::READ, false)
                    .unwrap();
                cpu.map_file_pages(DEST, &destination, Prot::RW, storage == "file_cow")
                    .unwrap();
                cpu.map_file_pages(MIRROR, &destination, Prot::RW, false)
                    .unwrap();
                None
            } else {
                cpu.map(SOURCE, BYTES as u32, Prot::RW).unwrap();
                cpu.write(SOURCE, &expected).unwrap();
                if storage == "pcm" {
                    Some(cpu.map_shared_audio(DEST, BYTES as u32).unwrap())
                } else {
                    cpu.map(DEST, BYTES as u32, Prot::RW).unwrap();
                    None
                }
            };
            cpu.write(CODE, &bytes).unwrap();
            cpu.executor_mut()
                .add_region(Region::new(CODE, bytes.len()))
                .unwrap();
            if operation == "rep_cmpsb" {
                cpu.write(DEST, &expected).unwrap();
            }
            let count = BYTES
                / if operation == "rep_cmpsb" {
                    1
                } else if operation == "sse_loop" {
                    16
                } else {
                    4
                };
            let mut times = Vec::with_capacity(samples);
            let mut interpreted = 0;
            let mut entries = 0;
            for iteration in 0..samples + 3 {
                cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
                cpu.executor_mut().state = CpuState {
                    eip: CODE,
                    regs: [0, count as u32, 0, 0, STACK, 0, SOURCE, DEST],
                    ..CpuState::default()
                };
                let before = cpu.stats();
                let start = Instant::now();
                assert_eq!(
                    cpu.executor_mut().run(STOP, 1_000_000).unwrap(),
                    RunOutcome::ReachedStop
                );
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(cpu.executor().state.regs[1], 0);
                if iteration >= 3 {
                    times.push(elapsed);
                    interpreted +=
                        cpu.stats().interpreter_instructions - before.interpreter_instructions;
                    entries += cpu.stats().native_entries - before.native_entries;
                }
                let mut actual = vec![0; BYTES];
                cpu.read(DEST, &mut actual).unwrap();
                assert_eq!(actual, expected);
                if let Some(region) = &callback {
                    region.read_alias.copy_into(0, &mut actual);
                    assert_eq!(actual, expected);
                }
                if file {
                    cpu.read(MIRROR, &mut actual).unwrap();
                    assert_eq!(
                        actual,
                        if storage == "file_cow" {
                            vec![0; BYTES]
                        } else {
                            expected.clone()
                        }
                    );
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{operation},{storage},{BYTES},{:.4},{},{},{}",
                times[samples / 2],
                interpreted / samples as u64,
                entries / samples as u64,
                cpu.stats().native_code_bytes
            );
        }
    }
}
