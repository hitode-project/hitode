//! Conditional writes retain stack-register bias, including false conditions
//! and memory sources that alias the destination or fault before selection.
use super::*;

// spec: A64-REG-6, A64-FLAG-4, A64-MEM-5, A64-VER-1
#[test]
fn conditional_stack_register_writes_preserve_bias_and_eager_faults() {
    for target in [esp, ebp] {
        for take in [false, true] {
            for source in 0..5 {
                let mut p = fixture(|a| {
                    a.cmp(eax, i32::from(take)).unwrap();
                    match source {
                        0 => a.cmovb(target, ecx),
                        1 => a.cmovb(target, dword_ptr(edi)),
                        2 => a.cmovb(target, dword_ptr(target)),
                        3 => a.cmovb(target, target),
                        _ => a.cmovb(target, dword_ptr(HOLE)),
                    }
                    .unwrap();
                    a.mov(dword_ptr(target), 0x12345678u32).unwrap();
                });
                p.state.regs[0] = 0;
                p.state.regs[1] = DATA + 512;
                p.state.regs[7] = DATA + 256;
                p.data[256..260].copy_from_slice(&(DATA + 512).to_le_bytes());
                for config in super::pixel_loops::configs() {
                    let mut a = executor(&p, config, false);
                    let mut b = executor(&p, config, true);
                    if source == 2 {
                        let address = p.state.regs[if target == esp { 4 } else { 5 }];
                        for e in [&mut a, &mut b] {
                            e.write(address, &(DATA + 512).to_le_bytes()).unwrap();
                        }
                    }
                    assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                    same(&a, &b);
                    if source != 4 {
                        assert_eq!(a.native.as_ref().unwrap().unchecked_faults, 0);
                    }
                }
            }
        }
    }
}
