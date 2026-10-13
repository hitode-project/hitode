//! Authored mutant/semaphore calls exercise the same alias and wait owners.
use super::*;

fn named(machine: &mut Machine<JitCpu>, leaf: &str, offset: u32) -> (u32, u32, u32) {
    let ansi = DATA + offset;
    machine
        .cpu
        .write(ansi, &[leaf.as_bytes(), &[0]].concat())
        .unwrap();
    let wide = ansi + 0x80;
    let words: Vec<_> = format!("Local\\{leaf}\0")
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(wide, &words).unwrap();
    let attrs = ansi + 0x100;
    let native: Vec<_> = format!(r"\Sessions\0\BaseNamedObjects\{leaf}")
        .encode_utf16()
        .collect();
    let buffer = attrs + 40;
    machine
        .cpu
        .write(
            buffer,
            &native
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    for (index, value) in [24, 0, attrs + 24, 0x80, 0, 0].into_iter().enumerate() {
        machine
            .cpu
            .write_u32(attrs + index as u32 * 4, value)
            .unwrap();
    }
    machine
        .cpu
        .write(attrs + 24, &(native.len() as u16 * 2).to_le_bytes())
        .unwrap();
    machine
        .cpu
        .write(attrs + 26, &(native.len() as u16 * 2).to_le_bytes())
        .unwrap();
    machine.cpu.write_u32(attrs + 28, buffer).unwrap();
    (ansi, wide, attrs)
}

#[test]
fn mutant_and_semaphore_names_native_opens_counts_recursion_and_errors_share_one_owner() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let (mutex_a, mutex_w, mutex_attrs) = named(&mut machine, "authored-mutant", 0x1000);
        let (sem_a, sem_w, sem_attrs) = named(&mut machine, "authored-semaphore", 0x1400);
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateMutexA",
            &[I(0), I(1), I(mutex_a)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "CreateMutexW",
            &[I(0), I(0), I(mutex_w)],
            DATA + 4,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 8);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtOpenMutant",
            &[I(DATA + 12), I(0x1f0001), I(mutex_attrs)],
            DATA + 16,
        );
        call(
            &mut machine,
            &mut code,
            "OpenMutexW",
            &[I(0x1f0001), I(1), I(mutex_w)],
            DATA + 20,
        );
        call(
            &mut machine,
            &mut code,
            "GetHandleInformation",
            &[M(DATA + 20), I(DATA + 24)],
            DATA + 28,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 12), I(0)],
            DATA + 32,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseMutant",
            &[M(DATA + 4), I(DATA + 36)],
            DATA + 40,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseMutex",
            &[M(DATA + 20)],
            DATA + 44,
        );
        code.mov_mem_imm(DATA + 48, 0x12345678);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseMutant",
            &[M(DATA), I(DATA + 48)],
            DATA + 52,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseMutex",
            &[M(DATA)],
            DATA + 56,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 60);
        call(
            &mut machine,
            &mut code,
            "CreateSemaphoreW",
            &[I(0), I(1), I(2), I(sem_w)],
            DATA + 64,
        );
        call(
            &mut machine,
            &mut code,
            "CreateSemaphoreA",
            &[I(0), I(0), I(10), I(sem_a)],
            DATA + 68,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 72);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtOpenSemaphore",
            &[I(DATA + 76), I(0x1f0003), I(sem_attrs)],
            DATA + 80,
        );
        call(
            &mut machine,
            &mut code,
            "OpenSemaphoreA",
            &[I(0x1f0003), I(1), I(sem_a)],
            DATA + 84,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 76), I(0)],
            DATA + 88,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseSemaphore",
            &[M(DATA + 84), I(2), I(DATA + 92)],
            DATA + 96,
        );
        code.mov_mem_imm(DATA + 100, 0x12345678);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseSemaphore",
            &[M(DATA + 68), I(1), I(DATA + 100)],
            DATA + 104,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseSemaphore",
            &[M(DATA + 64), I(0), I(DATA + 100)],
            DATA + 108,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 112);
        run(&mut machine, code);
        for offset in [4, 12, 20] {
            assert!(
                machine
                    .sys
                    .objects
                    .same_object(word(&machine, 0), word(&machine, offset))
            );
        }
        for offset in [68, 76, 84] {
            assert!(
                machine
                    .sys
                    .objects
                    .same_object(word(&machine, 64), word(&machine, offset))
            );
        }
        assert_eq!(
            (
                word(&machine, 8),
                word(&machine, 16),
                word(&machine, 24),
                word(&machine, 28)
            ),
            (183, 0, 1, 1)
        );
        assert_eq!(
            (
                word(&machine, 32),
                word(&machine, 36),
                word(&machine, 40),
                word(&machine, 44)
            ),
            (0, u32::MAX, 0, 1)
        );
        assert_eq!(
            (
                word(&machine, 48),
                word(&machine, 52),
                word(&machine, 56),
                word(&machine, 60)
            ),
            (0x12345678, 0xc0000046, 0, 288)
        );
        assert_eq!(
            (
                word(&machine, 72),
                word(&machine, 80),
                word(&machine, 88),
                word(&machine, 92),
                word(&machine, 96)
            ),
            (183, 0, 0, 0, 1)
        );
        assert_eq!(
            (
                word(&machine, 100),
                word(&machine, 104),
                word(&machine, 108),
                word(&machine, 112)
            ),
            (0x12345678, 0xc0000047, 0, 87)
        );
        assert_eq!(
            machine.sys.objects.handle_flags(word(&machine, 84)),
            Some(1)
        );
        assert!(matches!(
            machine.sys.objects.get(word(&machine, 64)),
            Some(Object::Semaphore { count: 2, max: 2 })
        ));
    }
}

#[test]
// spec: PERS-BRIDGE-4
fn all_six_native_boundaries_execute_live_hooks_and_obey_changed_inputs_and_statuses() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let (mutex_a, mutex_w, _) = named(&mut machine, "live-mutant", 0x1000);
        let (sem_a, sem_w, _) = named(&mut machine, "live-semaphore", 0x1400);
        for (name, slot, argc, change) in [
            ("NtCreateMutant", 0, 4, Change::Word(16, 1)),
            ("NtOpenMutant", 1, 3, Change::None),
            ("NtReleaseMutant", 2, 2, Change::Deny),
            ("NtCreateSemaphore", 3, 5, Change::Word(16, 1)),
            ("NtOpenSemaphore", 4, 3, Change::None),
            ("NtReleaseSemaphore", 5, 3, Change::Word(8, 2)),
        ] {
            hook(&mut machine, name, slot, argc, change, false);
        }
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateMutexA",
            &[I(0), I(0), I(mutex_a)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "CreateMutexW",
            &[I(0), I(0), I(mutex_w)],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut code,
            "OpenMutexA",
            &[I(0x1f0001), I(0), I(mutex_a)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "OpenMutexW",
            &[I(0x1f0001), I(1), I(mutex_w)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseMutex",
            &[M(DATA)],
            DATA + 16,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 20);
        call(
            &mut machine,
            &mut code,
            "CreateSemaphoreA",
            &[I(0), I(0), I(3), I(sem_a)],
            DATA + 24,
        );
        call(
            &mut machine,
            &mut code,
            "CreateSemaphoreW",
            &[I(0), I(0), I(9), I(sem_w)],
            DATA + 28,
        );
        call(
            &mut machine,
            &mut code,
            "OpenSemaphoreA",
            &[I(0x1f0003), I(0), I(sem_a)],
            DATA + 32,
        );
        call(
            &mut machine,
            &mut code,
            "OpenSemaphoreW",
            &[I(0x1f0003), I(1), I(sem_w)],
            DATA + 36,
        );
        call(
            &mut machine,
            &mut code,
            "ReleaseSemaphore",
            &[M(DATA + 32), I(1), I(DATA + 40)],
            DATA + 44,
        );
        run(&mut machine, code);
        for offset in [4, 8, 12] {
            assert!(
                machine
                    .sys
                    .objects
                    .same_object(word(&machine, 0), word(&machine, offset))
            );
        }
        for offset in [28, 32, 36] {
            assert!(
                machine
                    .sys
                    .objects
                    .same_object(word(&machine, 24), word(&machine, offset))
            );
        }
        assert_eq!((word(&machine, 16), word(&machine, 20)), (0, 5));
        assert!(matches!(
            machine.sys.objects.get(word(&machine, 0)),
            Some(Object::Mutex {
                owner: Some(0),
                recursion: 1
            })
        ));
        assert_eq!((word(&machine, 40), word(&machine, 44)), (1, 1));
        assert!(matches!(
            machine.sys.objects.get(word(&machine, 24)),
            Some(Object::Semaphore { count: 3, max: 3 })
        ));
        for (slot, count) in [(0, 2), (1, 2), (2, 1), (3, 2), (4, 2), (5, 1)] {
            assert_eq!(word(&machine, 0x800 + slot * 0x40), count);
        }
        assert_eq!(word(&machine, 0x800 + 24), 0x80);
        assert_eq!(word(&machine, 0x840 + 24), 2);
        assert_eq!(word(&machine, 0x8c0 + 24), 0x80);
        assert_eq!(word(&machine, 0x900 + 24), 2);
        let diagnostics = machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.structural_mismatches, 0);
        assert!(diagnostics.warnings.is_empty());
    }
}

#[test]
fn failure_outputs_follow_native_count_validation_and_preserve_release_state_and_aliases() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let (_, mutex_w, mutex_attrs) = named(&mut machine, "failure-mutant", 0x1000);
        let (_, sem_w, sem_attrs) = named(&mut machine, "failure-semaphore", 0x1400);
        machine.cpu.map(DATA + 0x4000, 4096, Prot::READ).unwrap();
        for offset in [0x400, 0x404, 0x408, 0x40c, 0x410, 0x414, 0x418, 0x41c] {
            machine.cpu.write_u32(DATA + offset, 0x12345678).unwrap();
        }
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateMutexW",
            &[I(0), I(1), I(mutex_w)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "CreateSemaphoreW",
            &[I(0), I(1), I(2), I(sem_w)],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut code,
            "test!sync_aliases",
            &[],
            DATA + 0x50,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtCreateSemaphore",
            &[I(DATA + 0x400), I(0x1f0003), I(0), I(u32::MAX), I(2)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtCreateSemaphore",
            &[I(DATA + 0x404), I(0x1f0003), I(0), I(0), I(0)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtCreateMutant",
            &[I(DATA + 0x408), I(0x100000), I(0), I(0)],
            DATA + 16,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtOpenMutant",
            &[I(DATA + 0x40c), I(0x100000), I(mutex_attrs)],
            DATA + 20,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtOpenSemaphore",
            &[I(DATA + 0x410), I(0x1f0003), I(mutex_attrs)],
            DATA + 24,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseMutant",
            &[M(DATA), I(DATA + 0x4000)],
            DATA + 28,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseSemaphore",
            &[M(DATA + 4), I(1), I(DATA + 0x4000)],
            DATA + 32,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseSemaphore",
            &[M(DATA + 4), I(u32::MAX), I(DATA + 0x414)],
            DATA + 36,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtReleaseSemaphore",
            &[M(DATA + 4), I(2), I(DATA + 0x418)],
            DATA + 40,
        );
        code.mov_mem_imm(sem_attrs + 16, DATA + 0x500); // unsupported security descriptor.
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtCreateSemaphore",
            &[I(DATA + 0x41c), I(0x1f0003), I(sem_attrs), I(0), I(1)],
            DATA + 44,
        );
        call(
            &mut machine,
            &mut code,
            "test!sync_aliases",
            &[],
            DATA + 0x54,
        );
        run(&mut machine, code);
        // Invalid native count/max clears the creation handle; unsupported
        // profiles and failed releases retain the caller's output bytes.
        assert_eq!(word(&machine, 0x404), 0);
        assert_eq!(word(&machine, 0x400), 0);
        for offset in [0x408, 0x40c, 0x410, 0x414, 0x418, 0x41c] {
            assert_eq!(word(&machine, offset), 0x12345678);
        }
        for offset in [8, 12, 36] {
            assert_eq!(word(&machine, offset), 0xc000000d);
        }
        for offset in [16, 20, 44] {
            assert_eq!(word(&machine, offset), 0xc00000bb);
        }
        assert_eq!(word(&machine, 24), 0xc0000024);
        assert_eq!(
            (word(&machine, 28), word(&machine, 32), word(&machine, 40)),
            (0xc0000005, 0xc0000005, 0xc0000047)
        );
        assert!(matches!(
            machine.sys.objects.get(word(&machine, 0)),
            Some(Object::Mutex {
                owner: Some(0),
                recursion: 1
            })
        ));
        assert!(matches!(
            machine.sys.objects.get(word(&machine, 4)),
            Some(Object::Semaphore { count: 1, max: 2 })
        ));
        assert_eq!(
            word(&machine, 0x50),
            word(&machine, 0x54),
            "rejected operations allocate no aliases"
        );
    }
}

pub(super) fn hold_until_waiter(
    env: &mut adv_machine::Env<'_>,
    handle: u32,
) -> adv_machine::Outcome {
    use adv_machine::{Outcome, Wait};
    let waiting = env.sys.contexts.iter().any(|context| {
        context.id != env.context
            && match context.waiting_on() {
                Some(Wait::Object { handle: other, .. }) => {
                    env.sys.objects.same_object(handle, *other)
                }
                Some(Wait::Objects { handles, .. }) => handles
                    .iter()
                    .any(|other| env.sys.objects.same_object(handle, *other)),
                _ => false,
            }
    });
    if waiting {
        env.cpu.write_u32(DATA + 0x7c0, 1).unwrap();
        Outcome::Return(0)
    } else {
        Outcome::block(Wait::Yield, move |env, _| hold_until_waiter(env, handle))
    }
}

#[test]
fn native_mutant_and_semaphore_releases_wake_actual_win32_object_waiters() {
    for jit in [false, true] {
        for semaphore in [false, true] {
            let mut machine = fixture(jit);
            let mut code = Asm::new(CODE);
            if semaphore {
                call(
                    &mut machine,
                    &mut code,
                    "CreateSemaphoreA",
                    &[I(0), I(0), I(1), I(0)],
                    DATA,
                );
            } else {
                call(
                    &mut machine,
                    &mut code,
                    "CreateMutexW",
                    &[I(0), I(1), I(0)],
                    DATA,
                );
            }
            let worker = CODE + 0x6000;
            let mut routine = Asm::new(worker);
            let wait = machine
                .resolve_import("kernel32.dll", "WaitForSingleObject")
                .unwrap();
            routine
                .mov_eax_esp(4)
                .push_imm(u32::MAX)
                .push_eax()
                .call(wait)
                .mov_mem_eax(DATA + 0x50);
            if !semaphore {
                routine
                    .mov_eax_esp(4)
                    .push_eax()
                    .call(
                        machine
                            .resolve_import("kernel32.dll", "ReleaseMutex")
                            .unwrap(),
                    )
                    .mov_mem_eax(DATA + 0x54);
            }
            routine.bytes.extend([0x31, 0xc0]);
            routine.ret_n(4);
            machine.cpu.write(worker, &routine.bytes).unwrap();
            call(
                &mut machine,
                &mut code,
                "CreateThread",
                &[I(0), I(0), I(worker), M(DATA), I(0), I(0)],
                DATA + 4,
            );
            call(
                &mut machine,
                &mut code,
                "test!hold_sync_waiter",
                &[M(DATA)],
                DATA + 8,
            );
            if semaphore {
                call(
                    &mut machine,
                    &mut code,
                    "ntdll.dll!NtReleaseSemaphore",
                    &[M(DATA), I(1), I(DATA + 12)],
                    DATA + 16,
                );
            } else {
                call(
                    &mut machine,
                    &mut code,
                    "ntdll.dll!NtReleaseMutant",
                    &[M(DATA), I(DATA + 12)],
                    DATA + 16,
                );
            }
            call(
                &mut machine,
                &mut code,
                "WaitForSingleObject",
                &[M(DATA + 4), I(u32::MAX)],
                DATA + 20,
            );
            run(&mut machine, code);
            assert_eq!(
                word(&machine, 0x7c0),
                1,
                "release happened only after a real object wait"
            );
            assert_eq!(
                (
                    word(&machine, 12),
                    word(&machine, 16),
                    word(&machine, 20),
                    word(&machine, 0x50)
                ),
                (0, 0, 0, 0)
            );
            if semaphore {
                assert!(matches!(
                    machine.sys.objects.get(word(&machine, 0)),
                    Some(Object::Semaphore { count: 0, max: 1 })
                ));
            } else {
                assert_eq!(word(&machine, 0x54), 1);
                assert!(matches!(
                    machine.sys.objects.get(word(&machine, 0)),
                    Some(Object::Mutex {
                        owner: None,
                        recursion: 0
                    })
                ));
            }
        }
    }
}
