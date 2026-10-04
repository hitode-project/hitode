//! UCRT exit arrays live in guest memory and use the common heap and mutex.
use super::*;
use adv_abi::{GuestPtr, win32};
use adv_cpu::backend::Prot;

const MAX_BYTES: u32 = 1024 * 1024;
const RUNTIME: &str = "api-ms-win-crt-runtime-l1-1-0.dll";

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
}

#[derive(Clone, Copy)]
enum Action {
    Initialize,
    Append(u32),
    Execute,
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

fn finish(env: &mut Env<'_>, lock: u32, result: u32) -> Outcome {
    let Some(adv_machine::Object::Mutex { owner, recursion }) = env.sys.objects.get_mut(lock)
    else {
        return Outcome::Fail("UCRT exit lock disappeared".into());
    };
    if *owner != Some(env.sys.current) {
        return Outcome::Fail("UCRT exit lock owner changed".into());
    }
    *recursion -= 1;
    if *recursion == 0 {
        *owner = None;
    }
    env.sys.signal_waiters(lock);
    Outcome::Return(result)
}

fn with_lock(env: &mut Env<'_>, pointer: GuestPtr<[u32; 3]>, action: Action) -> Outcome {
    let key = ("ucrtbase.dll".to_owned(), 8);
    let lock = *env
        .ext
        .get_or_default::<CrtState>()
        .locks
        .entry(key)
        .or_insert_with(|| {
            env.sys.objects.insert(adv_machine::Object::Mutex {
                owner: None,
                recursion: 0,
            })
        });
    if lock == 0 {
        return Outcome::Return(u32::MAX);
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
            return finish(env, lock, 0);
        }
        let result = if pointer.write(env.cpu, [0; 3]).is_ok() {
            0
        } else {
            u32::MAX
        };
        return finish(env, lock, result);
    }
    let Some([first, last, end]) = table(env, pointer) else {
        return finish(env, lock, u32::MAX);
    };
    match action {
        Action::Append(function) => {
            let used = last - first;
            let (first, end) = if last == end {
                let capacity = (end - first).saturating_mul(2).max(32 * 4);
                if capacity > MAX_BYTES {
                    return finish(env, lock, u32::MAX);
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
                    return finish(env, lock, u32::MAX);
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
                return finish(env, lock, u32::MAX);
            }
            finish(env, lock, 0)
        }
        Action::Execute => {
            let mut next = last;
            while next != first {
                next -= 4;
                let Ok(function) = env.cpu.read_u32(next) else {
                    return finish(env, lock, u32::MAX);
                };
                if pointer.write(env.cpu, [first, next, end]).is_err() {
                    return finish(env, lock, u32::MAX);
                }
                if function != 0 {
                    return Outcome::call(function, [], move |env, _| {
                        // Re-read after the callback: recursive registration can
                        // resize the table, and callbacks may yield or recurse.
                        perform(env, pointer, Action::Execute, lock)
                    });
                }
            }
            if pointer.write(env.cpu, [0; 3]).is_err() {
                return finish(env, lock, u32::MAX);
            }
            if first != 0 {
                let heaps = env.ext.get_mut::<Heaps>();
                if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, first) {
                    return finish(env, lock, u32::MAX);
                }
            }
            finish(env, lock, 0)
        }
        Action::Initialize => unreachable!(),
    }
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
        let mut env = Env {
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
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
}
