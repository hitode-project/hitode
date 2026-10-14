//! Process-lifetime OLE/Shell allocator over the shared COM task-memory owner.
use adv_abi::{GuestPtr, Registry, win32};
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_os::{
    Heaps,
    com::{Com, IUNKNOWN, task_alloc},
};

const IID_MALLOC: [u8; 16] = [2, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46];
struct Allocator;
struct State {
    com: Com,
    pointer: u32,
}

fn allocator(env: &mut Env<'_>) -> Option<u32> {
    if let Some(state) = env.ext.try_get::<State>() {
        return Some(state.pointer);
    }
    let table = env
        .ext
        .get_mut::<Heaps>()
        .alloc(env.cpu, &mut env.sys.vm, 0, 40, true)?;
    let pointer = table + 36;
    let result = (|| {
        for s in adv_abi::signature::com_signatures()
            .iter()
            .filter(|s| s.module == "IMalloc")
        {
            let entry = env
                .sys
                .thunks
                .publish(env.cpu, "ole32.dll", &format!("IMalloc::{}", s.name))
                .ok()?;
            env.cpu
                .write_u32(table + u32::from(s.com_slot?) * 4, entry)
                .ok()?;
        }
        env.cpu.write_u32(pointer, table).ok()?;
        let com = Com::new(&mut env.sys.objects)?;
        com.create(&mut env.sys.objects, pointer, IID_MALLOC, Allocator)?;
        // The native task allocator survives all Release calls. Its permanent
        // owner keeps the identity alive until the process itself is destroyed.
        env.ext.insert(State { com, pointer });
        Some(pointer)
    })();
    if result.is_none() {
        env.ext
            .get_mut::<Heaps>()
            .free(env.cpu, &mut env.sys.vm, 0, table);
    }
    result
}
fn valid(env: &Env<'_>, this: u32) -> bool {
    env.ext
        .try_get::<State>()
        .is_some_and(|s| s.com.get::<Allocator>(&env.sys.objects, this).is_some())
}
fn get(env: &mut Env<'_>, context: u32, output: u32) -> Outcome {
    if output == 0 || env.cpu.write_u32(output, 0).is_err() {
        return Outcome::Return(0x80004003);
    }
    if context != 1 {
        return Outcome::Return(0x80070057);
    }
    let Some(pointer) = allocator(env) else {
        return Outcome::Return(0x8007000e);
    };
    match env.cpu.write_u32(output, pointer) {
        Ok(()) => Outcome::Return(0),
        Err(e) => Outcome::Fail(e.to_string()),
    }
}
#[win32(ole32, name = "CoGetMalloc", level = state)]
fn co_get(env: &mut Env<'_>, context: u32, output: GuestPtr<()>) -> Outcome {
    get(env, context, output.addr())
}
#[win32(shell32, name = "SHGetMalloc", level = state)]
fn sh_get(env: &mut Env<'_>, output: GuestPtr<()>) -> Outcome {
    get(env, 1, output.addr())
}
#[win32(ole32, name = "IMalloc::QueryInterface", level = state)]
fn query(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    iid: GuestPtr<()>,
    output: GuestPtr<()>,
) -> Outcome {
    if output.is_null() || env.cpu.write_u32(output.addr(), 0).is_err() {
        return Outcome::Return(0x80004003);
    }
    if !valid(env, this.addr()) {
        return Outcome::Return(0x80070057);
    }
    let mut id = [0; 16];
    if env.cpu.read(iid.addr(), &mut id).is_err() {
        return Outcome::Return(0x80004003);
    }
    if id != IID_MALLOC && id != IUNKNOWN {
        return Outcome::Return(0x80004002);
    }
    match env.cpu.write_u32(output.addr(), this.addr()) {
        Ok(()) => Outcome::Return(0),
        Err(e) => Outcome::Fail(e.to_string()),
    }
}
#[win32(ole32, name = "IMalloc::AddRef", level = state)]
fn add_ref(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    Outcome::Return(u32::from(valid(env, this.addr())))
}
#[win32(ole32, name = "IMalloc::Release", level = state)]
fn release(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    add_ref(env, this)
}
#[win32(ole32, name = "IMalloc::Alloc", level = state)]
fn alloc(env: &mut Env<'_>, this: GuestPtr<()>, size: u32) -> Outcome {
    if !valid(env, this.addr()) {
        return Outcome::Return(0);
    }
    Outcome::Return(task_alloc(env, size))
}
#[win32(ole32, name = "IMalloc::Realloc", level = state)]
fn realloc(env: &mut Env<'_>, this: GuestPtr<()>, old: GuestPtr<()>, size: u32) -> Outcome {
    if !valid(env, this.addr()) {
        return Outcome::Return(0);
    }
    super::ole_memory::realloc_value(env, old.addr(), size)
}
#[win32(ole32, name = "IMalloc::Free", level = state)]
fn free(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> Outcome {
    if !valid(env, this.addr()) {
        return Outcome::Fail("IMalloc::Free: invalid allocator".into());
    }
    super::ole_memory::free_value(env, pointer.addr())
}
#[win32(ole32, name = "IMalloc::GetSize", level = state)]
fn get_size(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> Outcome {
    Outcome::Return(if valid(env, this.addr()) {
        env.ext
            .get_mut::<Heaps>()
            .size(env.cpu, 0, pointer.addr())
            .unwrap_or(u32::MAX)
    } else {
        u32::MAX
    })
}
#[win32(ole32, name = "IMalloc::DidAlloc", level = state)]
fn did_alloc(env: &mut Env<'_>, this: GuestPtr<()>, pointer: GuestPtr<()>) -> Outcome {
    Outcome::Return(if !valid(env, this.addr()) || pointer.is_null() {
        u32::MAX
    } else {
        u32::from(
            env.ext
                .get_mut::<Heaps>()
                .size(env.cpu, 0, pointer.addr())
                .is_some(),
        )
    })
}
#[win32(ole32, name = "IMalloc::HeapMinimize", level = state)]
fn minimize(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    if !valid(env, this.addr()) {
        return Outcome::Fail("IMalloc::HeapMinimize: invalid allocator".into());
    }
    // An optional compaction hint. The shared heap can retain reusable blocks;
    // this allocator has no separate cache and never relocates live pointers.
    Outcome::Return(0)
}
pub(super) fn register(reg: &mut Registry) {
    for api in [
        co_get_ABI,
        sh_get_ABI,
        query_ABI,
        add_ref_ABI,
        release_ABI,
        alloc_ABI,
        realloc_ABI,
        free_ABI,
        get_size_ABI,
        did_alloc_ABI,
        minimize_ABI,
    ] {
        reg.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Prot},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        }
    }
    fn returned(result: Outcome) -> u32 {
        match result {
            Outcome::Return(v) => v,
            _ => panic!("allocator must return synchronously"),
        }
    }
    fn fixture() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        register(&mut registry);
        super::super::ole_memory::register(&mut registry);
        assert!(registry.signature_conflicts().is_empty());
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut heaps = Heaps::default();
        heaps.process_heap = heaps.create(&mut m.cpu, &mut m.sys.vm).unwrap();
        m.ext.insert(heaps);
        m.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        m
    }
    use adv_os::com::TaskMemory;
    #[test]
    // spec: WIN32-OBJECT-6
    fn shell_and_ole_share_process_allocator_and_task_memory_lifecycle() {
        let mut m = fixture();
        const OUT: u32 = 0x410000;
        for context in [0, 2, 4] {
            assert_eq!(returned(get(&mut env(&mut m), context, OUT)), 0x80070057);
            assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0);
        }
        assert_eq!(returned(get(&mut env(&mut m), 1, OUT)), 0);
        let pointer = m.cpu.read_u32(OUT).unwrap();
        assert_eq!(returned(sh_get(&mut env(&mut m), GuestPtr::new(OUT))), 0);
        assert_eq!(m.cpu.read_u32(OUT).unwrap(), pointer);
        let this = GuestPtr::new(pointer);
        m.cpu.write(OUT + 16, &IUNKNOWN).unwrap();
        assert_eq!(
            returned(query(
                &mut env(&mut m),
                this,
                GuestPtr::new(OUT + 16),
                GuestPtr::new(OUT + 32)
            )),
            0
        );
        assert_eq!(m.cpu.read_u32(OUT + 32).unwrap(), pointer);
        for _ in 0..3 {
            assert_eq!(returned(release(&mut env(&mut m), this)), 1);
        }
        for size in [0, 1, 16, 33] {
            let block = returned(alloc(&mut env(&mut m), this, size));
            assert_ne!(block, 0);
            assert_eq!(
                returned(get_size(&mut env(&mut m), this, GuestPtr::new(block))),
                size
            );
            assert_eq!(
                returned(did_alloc(&mut env(&mut m), this, GuestPtr::new(block))),
                1
            );
            if size > 0 {
                m.cpu.write(block, &[0x51]).unwrap();
            }
            let grown = returned(realloc(&mut env(&mut m), this, GuestPtr::new(block), 65536));
            assert_ne!(grown, 0);
            if size > 0 {
                assert_eq!(m.cpu.read_u8(grown).unwrap(), 0x51);
            }
            assert_eq!(
                returned(super::super::ole_memory::free_value(
                    &mut env(&mut m),
                    grown
                )),
                0
            );
            assert!(!m.ext.get::<TaskMemory>().allocations.contains(&grown));
            assert_eq!(m.ext.get_mut::<Heaps>().size(&m.cpu, 0, grown), None);
        }
        let block = adv_os::com::task_alloc(&mut env(&mut m), 16);
        assert_eq!(
            returned(free(&mut env(&mut m), this, GuestPtr::new(block))),
            0
        );
        let foreign = m
            .ext
            .get_mut::<Heaps>()
            .alloc(&mut m.cpu, &mut m.sys.vm, 0, 16, false)
            .unwrap();
        assert_eq!(
            returned(did_alloc(&mut env(&mut m), this, GuestPtr::new(foreign))),
            1
        );
        assert_eq!(
            returned(free(&mut env(&mut m), this, GuestPtr::new(foreign))),
            0
        );
        assert_eq!(
            returned(get_size(&mut env(&mut m), this, GuestPtr::new(0))),
            u32::MAX
        );
        assert_eq!(
            returned(did_alloc(&mut env(&mut m), this, GuestPtr::new(0))),
            u32::MAX
        );
        assert_eq!(returned(minimize(&mut env(&mut m), this)), 0);
    }
}
