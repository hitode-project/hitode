//! UCRT exit arrays live in guest memory and use the common heap and mutex.
use super::*;
use adv_abi::{GuestPtr, win32};
use adv_cpu::backend::Prot;

const MAX_BYTES: u32 = 1024 * 1024;
const RUNTIME: &str = "api-ms-win-crt-runtime-l1-1-0.dll";
const PROCESS_TABLE: &str = "_onexit_table";

pub(super) fn register(reg: &mut Registry) {
    for api in [
        initialize_ABI,
        append_ABI,
        execute_ABI,
        initterm_ABI,
        initterm_e_ABI,
    ] {
        reg.register(api);
        reg.register(adv_abi::Api {
            module: RUNTIME,
            ..api
        });
    }
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for api in [onexit_ABI, atexit_ABI, cexit_ABI, exit_ABI, fast_exit_ABI] {
            reg.register(adv_abi::Api { module, ..api });
        }
    }
    for module in ["ucrtbase.dll", RUNTIME] {
        // Public UCRT _onexit/atexit are static shims, not DLL exports.
        for api in [crt_atexit_ABI, cexit_ABI, exit_ABI, fast_exit_ABI] {
            reg.register(adv_abi::Api { module, ..api });
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Initialize,
    Append(u32),
    OnExit(u32),
    Execute,
    Cleanup,
    Terminate(u32),
}

fn table(env: &Env<'_>, pointer: GuestPtr<[u32; 3]>) -> Option<[u32; 3]> {
    adv_abi::guest::check_range(env.cpu, pointer.addr(), 12, Prot::RW).ok()?;
    let value = pointer.read(env.cpu).ok()?;
    let [first, last, end] = value;
    if first == 0 {
        return (last == 0 && end == 0).then_some(value);
    }
    let capacity = end.checked_sub(first)?;
    let used = last.checked_sub(first)?;
    (capacity != 0
        && capacity <= MAX_BYTES
        && capacity % 4 == 0
        && used <= capacity
        && used % 4 == 0
        && env.cpu.accessible(first, capacity, Prot::RW)
        && env.ext.get::<Heaps>().owner(first).is_some())
    .then_some(value)
}

fn finish(env: &mut Env<'_>, lock: u32, result: u32, action: Action) -> Outcome {
    let Some(adv_machine::Object::Mutex { owner, recursion }) = env.sys.objects.get_mut(lock)
    else {
        return Outcome::Fail("UCRT exit lock disappeared".into());
    };
    if *owner != Some(env.context) {
        return Outcome::Fail("UCRT exit lock owner changed".into());
    }
    *recursion -= 1;
    if *recursion == 0 {
        *owner = None;
    }
    env.sys.signal_waiters(lock);
    match action {
        Action::OnExit(function) => Outcome::Return(if result == 0 { function } else { 0 }),
        Action::Terminate(code) if result == 0 => crate::kernel32::exit_process(env, code),
        Action::Cleanup | Action::Terminate(_) if result != 0 => {
            Outcome::Fail("CRT exit table is invalid or inaccessible".into())
        }
        _ => Outcome::Return(result),
    }
}

fn with_lock(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>, action: Action) -> Outcome {
    let key = (
        crt_module(&env.sys.thunks.get(env.thunk).module).to_owned(),
        8,
    );
    let lock = match env.ext.get_or_default::<CrtState>().locks.get(&key) {
        Some(&lock) => lock,
        None => {
            let lock = env.sys.objects.insert(adv_machine::Object::Mutex {
                owner: None,
                recursion: 0,
            });
            if lock != 0 {
                env.ext.get_mut::<CrtState>().locks.insert(key, lock);
            }
            lock
        }
    };
    if lock == 0 {
        return match action {
            Action::OnExit(_) => Outcome::Return(0),
            Action::Cleanup | Action::Terminate(_) => {
                Outcome::Fail("CRT exit lock allocation failed".into())
            }
            _ => Outcome::Return(u32::MAX),
        };
    }
    if env.sys.try_acquire(lock) {
        return perform(env, pointer, action, lock);
    }
    Outcome::block(
        adv_machine::Wait::Object {
            handle: lock,
            deadline: None,
        },
        move |env, _| perform(env, pointer, action, lock),
    )
}

fn perform(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>, action: Action, lock: u32) -> Outcome {
    if matches!(action, Action::Initialize) {
        // The CRT calls initialization on zero/sentinel tables, never on an
        // active table. Preserve a live initialized table on repeated calls.
        if let Some([first, _, _]) = table(env, pointer)
            && first != 0
        {
            return finish(env, lock, 0, action);
        }
        let result = if pointer.write(env.cpu, [0; 3]).is_ok() {
            0
        } else {
            u32::MAX
        };
        return finish(env, lock, result, action);
    }
    let Some([first, last, end]) = table(env, pointer) else {
        return finish(env, lock, u32::MAX, action);
    };
    match action {
        Action::Append(function) | Action::OnExit(function) => {
            let used = last - first;
            let (first, end) = if last == end {
                let capacity = (end - first).saturating_mul(2).max(32 * 4);
                if capacity > MAX_BYTES {
                    return finish(env, lock, u32::MAX, action);
                }
                let heaps = env.ext.get_mut::<Heaps>();
                let result = if first == 0 {
                    heaps.alloc(
                        env.cpu,
                        &mut env.sys.vm,
                        heaps.process_heap,
                        capacity,
                        false,
                    )
                } else {
                    heaps.realloc(
                        env.cpu,
                        &mut env.sys.vm,
                        heaps.process_heap,
                        first,
                        capacity,
                        false,
                        false,
                    )
                };
                let Some(base) = result else {
                    return finish(env, lock, u32::MAX, action);
                };
                (base, base + capacity)
            } else {
                (first, end)
            };
            if env.cpu.write_u32(first + used, function).is_err()
                || pointer
                    .write(env.cpu, [first, first + used + 4, end])
                    .is_err()
            {
                return finish(env, lock, u32::MAX, action);
            }
            finish(env, lock, 0, action)
        }
        Action::Execute | Action::Cleanup | Action::Terminate(_) => {
            let mut next = last;
            while next != first {
                next -= 4;
                let Ok(function) = env.cpu.read_u32(next) else {
                    return finish(env, lock, u32::MAX, action);
                };
                if pointer.write(env.cpu, [first, next, end]).is_err() {
                    return finish(env, lock, u32::MAX, action);
                }
                if function != 0 {
                    return Outcome::call(function, [], move |env, _| {
                        // Re-read after the callback: recursive registration can
                        // resize the table, and callbacks may yield or recurse.
                        perform(env, pointer, action, lock)
                    });
                }
            }
            if pointer.write(env.cpu, [0; 3]).is_err() {
                return finish(env, lock, u32::MAX, action);
            }
            if first != 0 {
                let heaps = env.ext.get_mut::<Heaps>();
                if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, first) {
                    return finish(env, lock, u32::MAX, action);
                }
            }
            finish(env, lock, 0, action)
        }
        Action::Initialize => unreachable!(),
    }
}

fn process_table(env: &mut Env<'_>, create: bool) -> Option<GuestPtr<[u32; 3]>> {
    let key = (
        crt_module(&env.sys.thunks.get(env.thunk).module).to_owned(),
        PROCESS_TABLE.to_owned(),
    );
    if let Some(pointer) = env.ext.get_or_default::<CrtState>().globals.get(&key) {
        return Some(GuestPtr::new(*pointer));
    }
    if !create {
        return None;
    }
    let pointer = allocate(env, 12, true);
    if pointer == 0 {
        return None;
    }
    env.ext.get_mut::<CrtState>().globals.insert(key, pointer);
    Some(GuestPtr::new(pointer))
}

/// Process teardown frees pending registrations without calling them. In
/// particular, Kernel32 ExitProcess and CRT _exit do not become CRT exit().
pub(super) fn release_process_tables(env: &mut Env<'_>) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let headers: Vec<_> = state
        .globals
        .iter()
        .filter(|((_, name), _)| name == PROCESS_TABLE)
        .map(|(key, &address)| (key.clone(), address))
        .collect();
    for (key, address) in headers {
        let pointer = GuestPtr::new(address);
        let Some([first, _, _]) = table(env, pointer) else {
            return Err("CRT process exit table was corrupted".into());
        };
        pointer.write(env.cpu, [0; 3]).map_err(|e| e.to_string())?;
        let heaps = env.ext.get_mut::<Heaps>();
        if first != 0 && !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, first) {
            return Err("CRT pending exit array release failed".into());
        }
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, address) {
            return Err("CRT exit header release failed".into());
        }
        env.ext.get_mut::<CrtState>().globals.remove(&key);
        if let Some(lock) = env.ext.get_mut::<CrtState>().locks.remove(&(key.0, 8))
            && env.sys.objects.close(lock).is_none()
        {
            return Err("CRT exit lock release failed".into());
        }
    }
    Ok(())
}

#[win32(msvcr90, name="_onexit", level=implement)]
fn onexit(env: &mut Env<'_>, callback: GuestPtr<()>) -> Outcome {
    match process_table(env, true) {
        Some(pointer) => with_lock(env, pointer, Action::OnExit(callback.addr())),
        None => Outcome::Return(0),
    }
}

fn register_exit(env: &mut Env<'_>, callback: GuestPtr<()>) -> Outcome {
    match process_table(env, true) {
        Some(pointer) => with_lock(env, pointer, Action::Append(callback.addr())),
        None => Outcome::Return(u32::MAX),
    }
}
#[win32(msvcr90, name="atexit", level=implement)]
fn atexit(env: &mut Env<'_>, callback: GuestPtr<()>) -> Outcome {
    register_exit(env, callback)
}
#[win32(ucrtbase, name="_crt_atexit", level=implement)]
fn crt_atexit(env: &mut Env<'_>, callback: GuestPtr<()>) -> Outcome {
    register_exit(env, callback)
}
#[win32(msvcr90, name="_cexit", level=implement)]
fn cexit(env: &mut Env<'_>) -> Outcome {
    match process_table(env, false) {
        Some(pointer) => with_lock(env, pointer, Action::Cleanup),
        None => Outcome::Return(0),
    }
}
#[win32(msvcr90, name="exit", level=implement)]
fn exit(env: &mut Env<'_>, status: i32) -> Outcome {
    match process_table(env, false) {
        Some(pointer) => with_lock(env, pointer, Action::Terminate(status as u32)),
        None => crate::kernel32::exit_process(env, status as u32),
    }
}
#[win32(msvcr90, name="_exit", level=implement)]
fn fast_exit(env: &mut Env<'_>, status: i32) -> Outcome {
    crate::kernel32::exit_process(env, status as u32)
}

#[win32(ucrtbase, name="_initialize_onexit_table", level=implement)]
fn initialize(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>) -> Outcome {
    with_lock(env, pointer, Action::Initialize)
}
#[win32(ucrtbase, name="_register_onexit_function", level=implement)]
fn append(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>, callback: GuestPtr<()>) -> Outcome {
    with_lock(env, pointer, Action::Append(callback.addr()))
}
#[win32(ucrtbase, name="_execute_onexit_table", level=implement)]
fn execute(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>) -> Outcome {
    with_lock(env, pointer, Action::Execute)
}
#[win32(ucrtbase, name="_initterm", level=implement)]
fn initterm(env: &mut Env<'_>, begin: GuestPtr<u32>, end: GuestPtr<u32>) -> Outcome {
    initializers(env, begin.addr(), end.addr(), false)
}
#[win32(ucrtbase, name="_initterm_e", level=implement)]
fn initterm_e(env: &mut Env<'_>, begin: GuestPtr<u32>, end: GuestPtr<u32>) -> Outcome {
    initializers(env, begin.addr(), end.addr(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    fn returned(value: Outcome) -> u32 {
        match value {
            Outcome::Return(n) => n,
            _ => panic!("unexpected callback or wait"),
        }
    }
    #[test]
    fn invalid_output_and_table_bounds_preserve_allocation_and_unlock() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        super::super::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(".", "onexit.exe", "target/onexit-unit"),
        )
        .unwrap();
        machine.spawn("exit", PARK_ADDR, &[], 65536).unwrap();
        machine.cpu.map(0x510000, 8192, Prot::RW).unwrap();
        let entry = machine
            .resolve_import("ucrtbase.dll", "_initialize_onexit_table")
            .unwrap();
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE,
        };
        let pointer = GuestPtr::<[u32; 3]>::new(0x510000);
        assert_eq!(returned(initialize(&mut env, pointer)), 0);
        assert_eq!(returned(append(&mut env, pointer, GuestPtr::new(0))), 0);
        let original = pointer.read(env.cpu).unwrap();
        assert!(env.ext.get::<Heaps>().owner(original[0]).is_some());
        env.cpu.protect(0x510000, 4096, Prot::READ).unwrap();
        assert_eq!(
            returned(append(&mut env, pointer, GuestPtr::new(123))),
            u32::MAX
        );
        assert_eq!(pointer.read(env.cpu).unwrap(), original);
        env.cpu.protect(0x510000, 4096, Prot::RW).unwrap();
        pointer
            .write(
                env.cpu,
                [original[0], original[1], original[0] + MAX_BYTES + 4],
            )
            .unwrap();
        assert_eq!(returned(execute(&mut env, pointer)), u32::MAX);
        pointer.write(env.cpu, original).unwrap();
        assert_eq!(returned(execute(&mut env, pointer)), 0);
        assert_eq!(pointer.read(env.cpu).unwrap(), [0; 3]);
        assert!(env.ext.get::<Heaps>().owner(original[0]).is_none());
        let lock = env.ext.get::<CrtState>().locks[&("ucrtbase.dll".to_owned(), 8)];
        assert!(matches!(
            env.sys.objects.get(lock),
            Some(adv_machine::Object::Mutex {
                owner: None,
                recursion: 0
            })
        ));
    }

    #[test]
    fn pending_process_tables_and_locks_are_released_once_without_callbacks() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        super::super::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        crate::Win32::install(
            &mut machine,
            crate::Win32Config::new(
                "/nonexistent/exit-release",
                "fixture.exe",
                "/nonexistent/exit-release-data",
            ),
        )
        .unwrap();
        machine
            .spawn("pending exit tables", PARK_ADDR, &[], 65536)
            .unwrap();
        for (module, name) in [
            ("msvcr90.dll", "_onexit"),
            ("ucrtbase.dll", "_crt_atexit"),
            (RUNTIME, "_crt_atexit"),
        ] {
            let entry = machine.resolve_import(module, name).unwrap();
            let mut env = Env {
                context: machine.sys.current,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: (entry - adv_machine::layout::THUNK_BASE)
                    / adv_machine::layout::THUNK_STRIDE,
            };
            // This deliberately unmapped callback must only be stored, and
            // must not be executed during bare process-memory teardown.
            let outcome = if name == "_onexit" {
                onexit(&mut env, GuestPtr::new(0x510000))
            } else {
                crt_atexit(&mut env, GuestPtr::new(0x510000))
            };
            assert_eq!(
                returned(outcome),
                if name == "_onexit" { 0x510000 } else { 0 }
            );
        }
        let state = machine.ext.get::<CrtState>();
        let allocations: Vec<_> = state
            .globals
            .iter()
            .filter(|((_, name), _)| name == PROCESS_TABLE)
            .map(|((module, _), &header)| {
                (
                    header,
                    GuestPtr::<[u32; 3]>::new(header)
                        .read(&machine.cpu)
                        .unwrap()[0],
                    state.locks[&(module.clone(), 8)],
                )
            })
            .collect();
        assert_eq!(allocations.len(), 2); // UCRT/API-set share the same header.
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        release_process_tables(&mut env).unwrap();
        release_process_tables(&mut env).unwrap();
        for (header, array, lock) in allocations {
            assert!(env.ext.get::<Heaps>().owner(header).is_none());
            assert!(env.ext.get::<Heaps>().owner(array).is_none());
            assert!(env.sys.objects.get(lock).is_none());
        }
    }
}
