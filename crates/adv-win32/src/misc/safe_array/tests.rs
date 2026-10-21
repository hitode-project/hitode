use super::*;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const ARRAY: u32 = 0x410000;
const OUT: u32 = 0x410100;
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
fn data_access_counts_locks_and_publishes_the_data_pointer() {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(ARRAY, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    // One dimension of four 4-byte elements whose data lives at 0x410200.
    let descriptor: [u32; 6] = [0x0080_0001, 4, 0, 0x410200, 4, 0];
    for (index, word) in descriptor.into_iter().enumerate() {
        m.cpu.write_u32(ARRAY + index as u32 * 4, word).unwrap();
    }
    let a = GuestPtr::new(ARRAY);
    let out = GuestPtr::<u32>::new(OUT);
    assert_eq!(access(&mut env(&mut m), a, out), S_OK);
    assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0x410200);
    assert_eq!(lock(&mut env(&mut m), a), S_OK);
    assert_eq!(m.cpu.read_u32(ARRAY + LOCKS).unwrap(), 2);
    assert_eq!(unlock(&mut env(&mut m), a), S_OK);
    assert_eq!(unaccess(&mut env(&mut m), a), S_OK);
    assert_eq!(unaccess(&mut env(&mut m), a), E_UNEXPECTED);
    assert_eq!(m.cpu.read_u32(ARRAY + LOCKS).unwrap(), 0);
    m.cpu.write_u32(ARRAY + LOCKS, MAX_LOCKS).unwrap();
    m.cpu.write_u32(OUT, 0x1234).unwrap();
    assert_eq!(access(&mut env(&mut m), a, out), E_UNEXPECTED);
    assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0);
    let null = GuestPtr::new(0);
    assert_eq!(access(&mut env(&mut m), null, out), E_INVALIDARG);
    assert_eq!(access(&mut env(&mut m), a, GuestPtr::new(0)), E_INVALIDARG);
    assert_eq!(
        lock(&mut env(&mut m), GuestPtr::new(READ_ONLY)),
        E_INVALIDARG
    );
}
