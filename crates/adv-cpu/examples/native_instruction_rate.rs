//! Calibrate a conditional native-time lower estimate using authored scalar loops.
//!
//! Usage: cargo run -p adv-cpu --release --example native_instruction_rate
//! Input: no sample or guest code. Output: anonymous JSON with seven instructions
//! per iteration, three measurements per loop, and the fastest observed rate.
//! This is an x64 synthetic reference, not an ARM64 bound or a native game replay.
use std::time::Instant;

#[cfg(target_arch = "x86_64")]
fn kernel(iterations: u32, dependent: bool) -> u32 {
    let mut result: u32;
    // SAFETY: authored register-only loops, no stack or memory access. All
    // clobbers and condition flags are declared; the counter is nonzero.
    unsafe {
        if dependent {
            std::arch::asm!(
                "2:", "add eax, edx", "xor eax, r8d", "rol eax, 5",
                "imul edx, edx, 1664525", "add edx, 1013904223", "dec ecx", "jnz 2b",
                inout("eax") 1u32 => result, inout("edx") 7u32 => _,
                in("r8d") 0x12345678u32, inout("ecx") iterations => _,
                options(nomem, nostack),
            );
        } else {
            std::arch::asm!(
                "2:", "add eax, 1", "add edx, 3", "add r8d, 5",
                "add r9d, 7", "add r10d, 11", "dec ecx", "jnz 2b",
                inout("eax") 1u32 => result, inout("edx") 7u32 => _,
                inout("r8d") 0u32 => _, inout("r9d") 0u32 => _, inout("r10d") 0u32 => _,
                inout("ecx") iterations => _, options(nomem, nostack),
            );
        }
    }
    result
}

#[cfg(target_arch = "x86_64")]
fn main() {
    const ITERATIONS: u32 = 40_000_000;
    let mut measurements = Vec::new();
    let mut fastest = 0u64;
    for dependent in [false, true] {
        std::hint::black_box(kernel(10_000, dependent));
        for _ in 0..3 {
            let at = Instant::now();
            std::hint::black_box(kernel(ITERATIONS, dependent));
            let ns = at.elapsed().as_nanos() as u64;
            let instructions = u64::from(ITERATIONS) * 7;
            let rate = ((instructions as u128 * 1_000_000_000) / u128::from(ns)) as u64;
            fastest = fastest.max(rate);
            measurements.push(format!("{{\"dependent\":{dependent},\"instructions\":{instructions},\"ns\":{ns},\"instructions_per_second\":{rate}}}"));
        }
    }
    println!(
        "{{\"schema\":\"adv-native-rate-v1\",\"architecture\":\"x86_64\",\"source\":\"authored_scalar_loops\",\"instructions_per_second\":{fastest},\"measurements\":[{}]}}",
        measurements.join(",")
    );
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    let _ = Instant::now();
    eprintln!("Native reference calibration requires x86_64.");
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    #[test]
    fn kernels_execute_the_declared_iteration_count() {
        for n in [1, 2, 100] {
            assert_eq!(super::kernel(n, false), 1 + n);
            let (mut a, mut d) = (1u32, 7u32);
            for _ in 0..n {
                a = (a.wrapping_add(d) ^ 0x12345678).rotate_left(5);
                d = d.wrapping_mul(1664525).wrapping_add(1013904223);
            }
            assert_eq!(super::kernel(n, true), a);
        }
    }
}
