use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg},
};
use adv_machine::{Clock, Env, Machine, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const SOURCE: u32 = 0x410000;
const DEST: u32 = SOURCE + 32;
fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn failure<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(".", "fixture.exe", "target/array-rollback-tests"),
    )
    .unwrap();
    m.cpu.map(SOURCE, 4096, Prot::RW).unwrap();
    m.spawn("array rollback", PARK_ADDR, &[], 65536).unwrap();
    let stack = m.sys.current_context().stack_hi - 128;
    m.cpu.set_reg(Reg::Esp, stack);
    let array = alloc(
        &mut env(&mut m),
        12,
        &[SAFEARRAYBOUND {
            cElements: 1,
            lLbound: 0,
        }],
    );
    assert_ne!(array, 0);
    let allocation = allocation(&env(&mut m), array).unwrap();
    m.cpu.write_u32(allocation.data, 15).unwrap();
    m.cpu.write_u32(SOURCE, 0x200c).unwrap();
    m.cpu.write_u32(SOURCE + 8, array).unwrap();
    assert!(matches!(
        crate::misc::variant::copy_value(&mut env(&mut m), DEST, SOURCE),
        Outcome::Return(0x80020008)
    ));
    assert_eq!(
        m.ext.get::<Arrays>().0.len(),
        1,
        "failed element copies retire unpublished descriptor/data blocks"
    );
    assert_eq!(m.cpu.read_u32(array + LOCKS).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(allocation.data).unwrap(), 15);
    assert_eq!(m.cpu.read_u16(DEST).unwrap(), 0);
    m.cpu.write_u32(allocation.data, 0).unwrap();
    assert!(matches!(
        crate::misc::variant::clear_value(&mut env(&mut m), SOURCE),
        Outcome::Return(0)
    ));
    assert!(m.ext.get::<Arrays>().0.is_empty());
    for pointer in [allocation.base, allocation.data] {
        assert_eq!(m.ext.get_mut::<Heaps>().size(&m.cpu, 0, pointer), None);
    }
}
#[test]
// spec: SYS-AUTOMATION-1, PERS-SCOPE-4
fn failed_nested_array_copy_retires_unpublished_storage_and_releases_source_lock() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        failure(cpu);
    }
    failure(UnicornCpu::new(PARK_ADDR).unwrap());
}
