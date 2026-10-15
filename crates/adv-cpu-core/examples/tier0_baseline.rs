//! Synthetic Tier 0 baseline, without JIT or game samples.
//! Run: cargo run -p adv-cpu-core --release --example tier0_baseline -- --seconds 2
//! Input: optional seconds per case (default 0), iterations per run (default 1000).
//! Output: JSON lines with retired instructions, REP units and wall throughput.
use adv_cpu_core::interpreter::{Interpreter, Step};
use adv_cpu_core::{CpuState, EXEC, Memory, READ, WRITE};
use iced_x86::code_asm::*;
use std::time::{Duration, Instant};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const DEST: u32 = 0x22000;
const FUNCTION: u32 = CODE + 0x1000;

fn program(case: &str) -> (Vec<u8>, u64) {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut start = a.create_label();
    a.set_label(&mut start).unwrap();
    let instructions = match case {
        "integer" => {
            a.add(eax, edx).unwrap();
            a.xor(eax, 0x55aa).unwrap();
            a.add(edx, 1).unwrap();
            5
        }
        "strings" => {
            a.mov(ecx, 4096).unwrap();
            a.mov(esi, DATA).unwrap();
            a.mov(edi, DEST).unwrap();
            a.rep().movsb().unwrap();
            a.mov(ecx, 4096).unwrap();
            a.mov(edi, DEST).unwrap();
            a.xor(eax, eax).unwrap();
            a.rep().stosb().unwrap();
            10
        }
        "x87" => {
            a.fld1().unwrap();
            a.fld1().unwrap();
            a.faddp(st1, st0).unwrap();
            a.fstp(dword_ptr(DEST)).unwrap();
            6
        }
        "mmx_mixed" => {
            a.movq(mm0, qword_ptr(DATA)).unwrap();
            a.paddb(mm0, qword_ptr(DATA + 8)).unwrap();
            a.pxor(mm0, mm1).unwrap();
            a.movq(qword_ptr(DEST), mm0).unwrap();
            a.emms().unwrap();
            a.add(eax, 1).unwrap();
            8
        }
        "indirect_call" => {
            a.call(dword_ptr(DATA)).unwrap();
            5 // call + authored INC/RET callee + DEC/JNZ
        }
        _ => panic!("unknown case"),
    };
    if case == "strings" {
        a.dec(edx).unwrap();
    } else {
        a.dec(ecx).unwrap();
    }
    a.jnz(start).unwrap();
    (a.assemble(CODE as u64).unwrap(), instructions)
}

fn baseline(case: &str, iterations: u32, seconds: f64) -> (u64, u64) {
    let (code, per_iteration) = program(case);
    let stop = CODE + code.len() as u32;
    let mut memory = Memory::new(CODE, 0x20000).unwrap();
    memory.protect(CODE, 0x20000, READ | WRITE).unwrap();
    memory.protect(CODE, 0x2000, READ | WRITE | EXEC).unwrap();
    memory.write(CODE, &code).unwrap();
    memory.write(FUNCTION, &[0x43, 0xc3]).unwrap(); // authored INC EBX; RET
    memory.write(DATA, &FUNCTION.to_le_bytes()).unwrap();
    let mut interpreter = Interpreter::new();
    interpreter.frame_retired = Some(0);
    let mut run = || {
        let mut state = CpuState {
            eip: CODE,
            ..CpuState::default()
        };
        state.regs[1] = iterations;
        state.regs[2] = iterations;
        state.regs[4] = DATA + 0x7000;
        let before = interpreter.frame_retired.unwrap();
        let mut units = 0u64;
        while state.eip != stop {
            let (execution, _) = interpreter.run_block(&mut state, &mut memory, 256, stop, None);
            assert_eq!(execution.step, Step::Executed, "{case}");
            assert!(execution.units > 0);
            units += u64::from(execution.units);
        }
        let retired = interpreter.frame_retired.unwrap() - before;
        assert_eq!(retired, per_iteration * u64::from(iterations), "{case}");
        match case {
            "x87" => assert_eq!(memory.read(DEST, 4).unwrap(), 2f32.to_le_bytes()),
            "mmx_mixed" => assert_eq!(state.regs[0], iterations),
            "indirect_call" => assert_eq!(state.regs[3], iterations),
            "strings" => assert!(memory.read(DEST, 4096).unwrap().iter().all(|&b| b == 0)),
            _ => {}
        }
        (retired, units)
    };
    run(); // warm decoding and allocations outside the measured interval
    let (retired, units, elapsed) = measure_loop(&mut run, seconds);
    println!(
        "{{\"case\":\"{case}\",\"tier0_instructions\":{retired},\"tier0_units\":{units},\"elapsed_seconds\":{elapsed:.6},\"tier0_instructions_per_second\":{:.3}}}",
        retired as f64 / elapsed
    );
    (retired, units)
}

// A stable collection boundary for Callgrind; decoding and setup are warmed first.
#[inline(never)]
fn measure_loop(mut run: impl FnMut() -> (u64, u64), seconds: f64) -> (u64, u64, f64) {
    let started = Instant::now();
    let mut retired = 0;
    let mut units = 0;
    loop {
        let (n, u) = run();
        retired += n;
        units += u;
        if started.elapsed() >= Duration::from_secs_f64(seconds) {
            break;
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    (retired, units, elapsed)
}

fn main() {
    let mut seconds = 0.0;
    let mut iterations = 1000;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seconds" => seconds = args.next().expect("seconds").parse::<f64>().unwrap(),
            "--iterations" => iterations = args.next().expect("iterations").parse::<u32>().unwrap(),
            _ => panic!("expected --seconds N or --iterations N"),
        }
    }
    assert!(seconds.is_finite() && seconds >= 0. && iterations > 0);
    for case in ["integer", "strings", "x87", "mmx_mixed", "indirect_call"] {
        baseline(case, iterations, seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: EXEC-CORE-5
    #[test]
    fn authored_loops_count_retired_instructions_separately_from_rep_units() {
        for case in ["integer", "strings", "x87", "mmx_mixed", "indirect_call"] {
            let (count, units) = baseline(case, 2, 0.);
            assert_eq!(count, program(case).1 * 2);
            if case == "strings" {
                assert!(units > count);
            } else {
                assert_eq!(units, count);
            }
        }
    }
}
