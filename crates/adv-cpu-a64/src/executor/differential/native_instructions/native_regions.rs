//! Engine-only checks of the admitted bridge ABI and its guarded scalar tail.
use super::*;
use adv_cpu_core::native_region::{Affine, Aliasing, NativeRegion, ReadSpan, Scalar};
use adv_cpu_core::trap::ExecutionTrap;

struct Admission(Vec<u8>);
impl ExecutionTrap for Admission {
    fn native_region(&self, _pc: u32, code: &[u8]) -> Vec<NativeRegion> {
        if code != self.0 {
            return Vec::new();
        }
        let input = |r| {
            let mut a = Affine {
                registers: [0; 8],
                constant: 0,
            };
            a.registers[r] = 1;
            a
        };
        // SAFETY: this fixture is exactly load-byte/add-byte/inc/inc/dec/jne;
        // the original tail reconstructs AL, all flags and the next PC.
        vec![unsafe {
            NativeRegion::new(
                [input(3), input(7), input(1)],
                [0, -1, 0, 1, 0, 0, 0, 1],
                6,
                [
                    Some(ReadSpan {
                        address: input(3),
                        aliasing: Aliasing::Disjoint,
                    }),
                    None,
                    None,
                ],
                Some(Scalar {
                    value: Affine {
                        registers: [0; 8],
                        constant: 0xa55a,
                    },
                    upper_bounds: [None; 2],
                }),
                add_bytes,
            )
        }]
    }
    fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
        unreachable!()
    }
}

unsafe extern "C" fn add_bytes(source: *const u8, destination: *mut u8, count: u32, scalar: u32) {
    assert_eq!(scalar, 0xa55a);
    // SAFETY: the bridge checked the complete disjoint ordinary batch spans.
    unsafe {
        for i in 0..count as usize {
            destination
                .add(i)
                .write(destination.add(i).read().wrapping_add(source.add(i).read()));
        }
        // Exercise canonical flags/fuel across a fully clobbering host call.
        std::arch::asm!("mov x9, #0", "mov x10, #0", "mov x11, #0", "mov x12, #0", "cmp xzr, xzr",
            out("x9") _, out("x10") _, out("x11") _, out("x12") _, options(nostack));
    }
}

fn program() -> Program {
    let mut p = fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(al, byte_ptr(ebx)).unwrap();
        a.add(byte_ptr(edi), al).unwrap();
        a.inc(ebx).unwrap();
        a.inc(edi).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
    });
    for (i, byte) in p.data.iter_mut().enumerate() {
        *byte = (i * 59 + i / 7) as u8;
    }
    p.state.regs[1] = 2048;
    p.state.regs[3] = DATA;
    p.state.regs[7] = DATA + 0x2000;
    p.state.eflags = 0xed7;
    p
}

fn compare(a: &mut Executor, b: &mut Executor, p: &Program, budget: u64) {
    let body = p.code[..p.code.len() - 1].to_vec();
    assert_eq!(
        a.run_with_traps(p.stop, budget, &mut Admission(body.clone()), false)
            .unwrap(),
        b.run_with_traps(p.stop, budget, &mut Admission(body), true)
            .unwrap()
    );
    same(a, b);
}

// spec: A64-TR-8, A64-VER-1, A64-CTL-7, A64-REG-4
#[test]
fn native_regions_preserve_state_and_instruction_budgets_in_all_configurations() {
    let p = program();
    for config in super::pixel_loops::configs() {
        for budget in [
            1,
            6 * 128,
            6 * 129 - 1,
            6 * 129,
            6 * 129 + 1,
            6 * 1025,
            100_000,
        ] {
            let mut a = executor(&p, config, false);
            let mut b = executor(&p, config, true);
            compare(&mut a, &mut b, &p, budget);
            compare(&mut a, &mut b, &p, 100_000);
            assert_eq!(
                a.native.as_ref().unwrap().native_batches() > 0,
                config.2 == Checks::Adaptive
            );
        }
    }
}

// spec: A64-TR-8, A64-MEM-4, A64-MEM-8, A64-VER-1
#[test]
fn native_regions_keep_fault_prefixes_and_decline_nonordinary_ranges() {
    for case in 0..5 {
        let mut p = program();
        match case {
            0 => p.state.regs[3] = DATA + DATA_LEN as u32 - 300,
            1 => p.state.regs[7] = p.state.regs[3] + 1,
            _ => {}
        }
        for flagm in [false, true] {
            let config = (Features { flagm }, false, Checks::Adaptive);
            let mut a = executor(&p, config, false);
            let mut b = executor(&p, config, true);
            for e in [&mut a, &mut b] {
                match case {
                    2 => e.memory_mut().watch_code(DATA + 0x2000, 4),
                    3 => e.protect(DATA + 0x2000, 0x1000, 1).unwrap(),
                    4 => e.protect(DATA, 0x1000, 1).unwrap(),
                    _ => {}
                }
            }
            compare(&mut a, &mut b, &p, 100_000);
            if case <= 3 {
                assert_eq!(a.native.as_ref().unwrap().native_batches() > 0, case == 0);
            }
        }
    }
}
