use super::*;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;

fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

#[test]
fn generated_bytes_fill_exact_lengths_and_reject_unwritable_buffers() {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write(DATA, &[0xcc; 64]).unwrap();
    assert_eq!(generate(&mut env(&mut machine), GuestPtr::new(DATA), 13), 1);
    assert_eq!(
        generate(&mut env(&mut machine), GuestPtr::new(DATA + 32), 13),
        1
    );
    let mut bytes = [0; 64];
    machine.cpu.read(DATA, &mut bytes).unwrap();
    assert_eq!(bytes[13..32], [0xcc; 19]);
    assert_eq!(bytes[45..], [0xcc; 19]);
    assert_ne!(bytes[..13], bytes[32..45]);
    assert_eq!(generate(&mut env(&mut machine), GuestPtr::new(DATA), 0), 1);
    assert_eq!(
        generate(&mut env(&mut machine), GuestPtr::new(READ_ONLY), 4),
        0
    );
}
