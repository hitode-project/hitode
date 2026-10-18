//! Default activation scopes share each guest thread's lifetime. Creating or
//! activating a non-default SxS context still requires loader-owned binding.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{CtxId, Env, Outcome};
use std::collections::BTreeMap;

#[derive(Default)]
struct Activation {
    last_cookie: u32,
    stacks: BTreeMap<CtxId, Vec<u32>>,
}

fn state<'a>(env: &'a mut Env<'_>) -> &'a mut Activation {
    let contexts = &env.sys.contexts;
    let state = env.ext.get_or_default::<Activation>();
    state.stacks.retain(|context, stack| {
        !stack.is_empty()
            && contexts
                .get(*context as usize)
                .is_some_and(|context| !context.is_done())
    });
    state
}

fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    crate::util::set_last_error(env, error);
    0
}

fn create(env: &mut Env<'_>, context: GuestPtr<()>, api: &'static str) -> Handle {
    let error = if check_range(env.cpu, context.addr(), 32, Prot::READ).is_err() {
        998
    } else if env.cpu.read_u32(context.addr()).ok() != Some(32)
        || env.cpu.read_u32(context.addr() + 4).unwrap_or(u32::MAX) & !0xff != 0
    {
        87
    } else {
        warn_unavailable(
            env,
            api,
            "side-by-side activation context binding is unavailable",
        );
        14001 // ERROR_SXS_CANT_GEN_ACTCTX
    };
    crate::util::set_last_error(env, error);
    Handle(u32::MAX)
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-createactctxw
#[win32(kernel32, name = "CreateActCtxW", level = safe_fail)]
fn create_w(env: &mut Env<'_>, context: GuestPtr<()>) -> Handle {
    create(env, context, "kernel32.dll!CreateActCtxW")
}
#[win32(kernel32, name = "CreateActCtxA", level = safe_fail)]
fn create_a(env: &mut Env<'_>, context: GuestPtr<()>) -> Handle {
    create(env, context, "kernel32.dll!CreateActCtxA")
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-activateactctx
#[win32(kernel32, name = "ActivateActCtx", level = implement)]
fn activate(env: &mut Env<'_>, context: Handle, output: GuestPtr<u32>) -> i32 {
    if context.0 == u32::MAX {
        return fail(env, 87);
    }
    if context.0 != 0 {
        warn_unavailable(
            env,
            "kernel32.dll!ActivateActCtx",
            "non-default side-by-side activation context binding is unavailable",
        );
        return fail(env, 120);
    }
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let Some(cookie) = state(env).last_cookie.checked_add(1) else {
        return fail(env, 8);
    };
    if env.cpu.write_u32(output.addr(), cookie).is_err() {
        return fail(env, 998);
    }
    let context = env.context;
    let state = state(env);
    state.last_cookie = cookie;
    state.stacks.entry(context).or_default().push(cookie);
    1
}

fn pop(env: &mut Env<'_>, context: CtxId, cookie: u32) {
    let state = state(env);
    if let Some(stack) = state.stacks.get_mut(&context)
        && let Some(index) = stack.iter().position(|entry| *entry == cookie)
    {
        stack.truncate(index);
        if stack.is_empty() {
            state.stacks.remove(&context);
        }
    }
}

/// A caught exception abandons this continuation, keeping its activation stack.
/// A handler which resumes the API epilogue may finish the forced operation.
fn after_exception(
    env: &mut Env<'_>,
    outcome: Outcome,
    context: CtxId,
    forced_cookie: Option<u32>,
    epilogue: (u32, u32),
) -> Outcome {
    match outcome {
        Outcome::Call {
            target,
            args,
            then,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            then: Box::new(move |env, result| {
                let outcome = then(env, result);
                after_exception(env, outcome, context, forced_cookie, epilogue)
            }),
            stack,
            unwindable,
        },
        Outcome::ResumeExecution(mut checkpoint)
            if (checkpoint.state.eip, checkpoint.state.regs[4]) == epilogue =>
        {
            if let Some(cookie) = forced_cookie {
                pop(env, context, cookie);
            }
            checkpoint.state.regs[0] = 1;
            Outcome::ResumeExecution(checkpoint)
        }
        outcome => outcome,
    }
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-deactivateactctx
#[win32(kernel32, name = "DeactivateActCtx", level = implement)]
fn deactivate(env: &mut Env<'_>, flags: u32, cookie: u32) -> Outcome {
    if flags & !1 != 0 {
        return Outcome::Return(fail(env, 87) as u32);
    }
    if cookie == 0 {
        return Outcome::Return(1);
    }
    let context = env.context;
    let position = state(env).stacks.get(&context).and_then(|stack| {
        stack
            .iter()
            .position(|entry| *entry == cookie)
            .map(|index| (index, stack.len()))
    });
    let Some((index, length)) = position else {
        // Native x86 rejects unknown or foreign-thread cookies noncontinuably.
        return super::exception::raise(env, 0xc000_000d, 1, 0, GuestPtr::new(0));
    };
    if index + 1 == length {
        pop(env, context, cookie);
        return Outcome::Return(1);
    }
    // Native x86 reports early deactivation for both supported flag values.
    let epilogue = (env.cpu.reg(Reg::Eip), env.cpu.reg(Reg::Esp));
    let outcome = super::exception::raise(env, 0xc015_000f, 0, 0, GuestPtr::new(0));
    after_exception(
        env,
        outcome,
        context,
        (flags == 1).then_some(cookie),
        epilogue,
    )
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-getcurrentactctx
#[win32(kernel32, name = "GetCurrentActCtx", level = implement)]
fn current(env: &mut Env<'_>, output: GuestPtr<Handle>) -> i32 {
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || env.cpu.write_u32(output.addr(), 0).is_err()
    {
        return fail(env, 998);
    }
    state(env);
    1
}

fn query_unavailable(env: &mut Env<'_>) -> i32 {
    warn_unavailable(
        env,
        "kernel32.dll!QueryActCtxW",
        "side-by-side activation context information is unavailable",
    );
    fail(env, 50) // ERROR_NOT_SUPPORTED
}

/// A NULL current handle also occurs in a manifest-bound Windows process.
/// Until the loader binds SxS contexts, reject known manifests rather than
/// describing their process/module scope as the empty default context.
fn query_scope(env: &Env<'_>, flags: u32, handle: u32) -> Result<bool, u32> {
    let image = flags & 24;
    let Some(modules) = adv_os::try_modules(env.ext) else {
        return if image == 0 { Ok(true) } else { Err(87) };
    };
    let main = env
        .cpu
        .read_u32(adv_machine::layout::PEB_ADDR + adv_machine::layout::peb::IMAGE_BASE)
        .ok()
        .and_then(|base| modules.by_handle(base));
    let selected = match image {
        8 => modules.by_handle(handle),
        16 => modules.containing(handle),
        _ => main,
    }
    .ok_or(87u32)?;
    if !modules.get(selected).executable_visible() {
        return Err(87);
    }
    let has_manifest = |id| {
        let module = modules.get(id);
        module
            .resources
            .as_ref()
            .is_some_and(|root| root.get(&adv_pe::ResId::Id(24)).is_some())
            || env.ext.try_get::<crate::Win32>().is_some_and(|win32| {
                win32
                    .vfs
                    .lookup(&format!("{}.manifest", module.path))
                    .is_some()
            })
    };
    Ok(!has_manifest(selected) && main.is_none_or(|id| !has_manifest(id)))
}

// https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-queryactctxw
#[win32(kernel32, name = "QueryActCtxW", level = implement)]
fn query(
    env: &mut Env<'_>,
    flags: u32,
    context: Handle,
    sub_instance: GuestPtr<()>,
    class: u32,
    buffer: GuestPtr<()>,
    bytes: u32,
    required: GuestPtr<u32>,
) -> i32 {
    if required.addr() != 0
        && (check_range(env.cpu, required.addr(), 4, Prot::WRITE).is_err()
            || env.cpu.write_u32(required.addr(), 0).is_err())
    {
        return fail(env, 998);
    }
    let image = flags & 24;
    // Native x86 additionally accepts the legacy process-default bit 1.
    if flags & !0x8000_001d != 0
        || image == 24
        || (image != 0 && flags & 5 != 0)
        || (image != 0 && context.0 == 0)
        || (flags & 5 != 0 && context.0 != 0)
        || (buffer.addr() == 0 && (bytes != 0 || required.addr() == 0))
        || (class == 2 && flags & 0x8000_0000 != 0)
        || (matches!(class, 5 | 6) && sub_instance.addr() != 0)
    {
        return fail(env, 87);
    }
    let size = match class {
        1 => 8,  // ACTIVATION_CONTEXT_BASIC_INFORMATION (x86)
        2 => 48, // ACTIVATION_CONTEXT_DETAILED_INFORMATION (x86)
        5 => 12, // ACTIVATION_CONTEXT_RUN_LEVEL_INFORMATION
        6 => 0,  // No compatibility payload exists in an unbound default scope.
        3 | 4 if sub_instance.addr() != 0 => return query_unavailable(env),
        _ => return fail(env, 87),
    };
    if image == 0 && context.0 != 0 {
        return if context.0 == u32::MAX {
            fail(env, 87)
        } else {
            query_unavailable(env)
        };
    }
    match query_scope(env, flags, context.0) {
        Ok(true) => {}
        Ok(false) => return query_unavailable(env),
        Err(error) => return fail(env, error),
    }
    if bytes < size {
        // The native empty detailed scope reports zero on an undersized query;
        // Basic and RunLevel report their fixed sizes, including size probes.
        if required.addr() != 0 && class != 2 {
            let _ = env.cpu.write_u32(required.addr(), size);
        }
        return fail(env, 122);
    }
    if size != 0
        && (check_range(env.cpu, buffer.addr(), size, Prot::WRITE).is_err()
            || env
                .cpu
                .write(buffer.addr(), &[0; 48][..size as usize])
                .is_err())
    {
        return fail(env, 998);
    }
    if required.addr() != 0 {
        let _ = env.cpu.write_u32(required.addr(), size);
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_w_ABI);
    registry.register(create_a_ABI);
    registry.register(activate_ABI);
    registry.register(deactivate_ABI);
    registry.register(current_ABI);
    registry.register(query_ABI);
}

#[cfg(test)]
mod query_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{JitCpu, backend::Guest};
    use adv_machine::{Clock, Machine, Resume, layout::PARK_ADDR};

    fn machine() -> Machine<JitCpu> {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        for name in ["ActivateActCtx", "DeactivateActCtx", "GetCurrentActCtx"] {
            assert!(registry.dynamic_available("kernel32.dll", name));
        }
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("first", PARK_ADDR, &[], 65536).unwrap();
        machine.spawn("second", PARK_ADDR, &[], 65536).unwrap();
        machine.cpu.map(0x410000, 4096, Prot::RW).unwrap();
        machine
    }

    fn env(machine: &mut Machine<JitCpu>, context: CtxId) -> Env<'_> {
        machine.sys.current = context;
        Env {
            context,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
    }

    fn push(env: &mut Env<'_>) -> u32 {
        assert_eq!(activate(env, Handle(0), GuestPtr::new(0x410000)), 1);
        env.cpu.read_u32(0x410000).unwrap()
    }

    #[test]
    fn default_scopes_preserve_errors_and_retire_with_the_thread() {
        let mut machine = machine();
        let (first, second);
        {
            let mut env = env(&mut machine, 0);
            crate::util::set_last_error(&mut env, 0x12345678);
            first = push(&mut env);
            second = push(&mut env);
            assert_ne!(first, 0);
            assert_ne!(first, second);
            assert_eq!(current(&mut env, GuestPtr::new(0x410004)), 1);
            assert_eq!(env.cpu.read_u32(0x410004).unwrap(), 0);
            assert!(matches!(deactivate(&mut env, 0, 0), Outcome::Return(1)));
            assert_eq!(state(&mut env).stacks[&0], [first, second]);
            assert!(matches!(
                deactivate(&mut env, 1, second),
                Outcome::Return(1)
            ));
            assert!(matches!(deactivate(&mut env, 0, first), Outcome::Return(1)));
            assert_eq!(crate::util::last_error(&env), 0x12345678);
            assert!(state(&mut env).stacks.is_empty());
            env.cpu.write_u32(0x410004, 0xcccccccc).unwrap();
            for (handle, output, error) in [
                (u32::MAX, 0x410004, 87),
                (1, 0x410004, 120),
                (0, 0x410ffe, 998),
            ] {
                assert_eq!(activate(&mut env, Handle(handle), GuestPtr::new(output)), 0);
                assert_eq!(crate::util::last_error(&env), error);
                assert_eq!(env.cpu.read_u32(0x410004).unwrap(), 0xcccccccc);
                assert!(state(&mut env).stacks.is_empty());
            }
            assert!(matches!(deactivate(&mut env, 2, 0), Outcome::Return(0)));
            assert_eq!(crate::util::last_error(&env), 87);
        }
        let third = push(&mut env(&mut machine, 1));
        assert_ne!(third, first);
        assert_ne!(third, second);
        machine.sys.terminate_context_stopped(1, 0);
        let mut env = env(&mut machine, 0);
        assert!(state(&mut env).stacks.is_empty());
        assert!(push(&mut env) > third);
        state(&mut env).last_cookie = u32::MAX;
        env.cpu.write_u32(0x410004, 0xcccccccc).unwrap();
        assert_eq!(activate(&mut env, Handle(0), GuestPtr::new(0x410004)), 0);
        assert_eq!(crate::util::last_error(&env), 8);
        assert_eq!(env.cpu.read_u32(0x410004).unwrap(), 0xcccccccc);
    }

    fn seh(env: &mut Env<'_>) {
        let top = env.sys.contexts[env.context as usize].stack_hi;
        let frame = top - 32;
        env.cpu
            .write_u32(env.sys.contexts[env.context as usize].teb, frame)
            .unwrap();
        env.cpu.write_u32(frame, u32::MAX).unwrap();
        env.cpu.write_u32(frame + 4, PARK_ADDR).unwrap();
        env.cpu.set_reg(Reg::Esp, top - 128);
        env.cpu.set_reg(Reg::Eip, PARK_ADDR);
    }

    #[test]
    fn deactivation_errors_use_seh_and_only_resume_finishes_a_forced_pop() {
        let mut machine = machine();
        let cookie;
        {
            let mut env = env(&mut machine, 0);
            seh(&mut env);
            cookie = push(&mut env);
            let second = push(&mut env);
            crate::util::set_last_error(&mut env, 0x12345678);
            let outcome = deactivate(&mut env, 0, cookie);
            let Outcome::Call { ref args, .. } = outcome else {
                panic!()
            };
            assert_eq!(env.cpu.read_u32(args[0]).unwrap(), 0xc015000f);
            assert_eq!(env.cpu.read_u32(args[0] + 4).unwrap(), 0);
            drop(outcome); // A nonlocal handler exit discards this continuation.
            assert_eq!(state(&mut env).stacks[&0], [cookie, second]);
            let Outcome::Call { then, .. } = deactivate(&mut env, 1, cookie) else {
                panic!()
            };
            let Outcome::ResumeExecution(checkpoint) = then(&mut env, Resume::Returned(0)) else {
                panic!()
            };
            assert_eq!(checkpoint.state.regs[0], 1);
            assert!(state(&mut env).stacks.is_empty());
            assert_eq!(crate::util::last_error(&env), 0x12345678);
            push(&mut env);
        }
        let foreign = state(&mut env(&mut machine, 0)).stacks[&0][0];
        let mut env = env(&mut machine, 1);
        seh(&mut env);
        let Outcome::Call { args, then, .. } = deactivate(&mut env, 0, foreign) else {
            panic!()
        };
        assert_eq!(env.cpu.read_u32(args[0]).unwrap(), 0xc000000d);
        assert_eq!(env.cpu.read_u32(args[0] + 4).unwrap(), 1);
        assert!(matches!(
            then(&mut env, Resume::Returned(0)),
            Outcome::Fail(_)
        ));
        assert_eq!(state(&mut env).stacks[&0], [foreign]);
    }

    #[test]
    fn creation_failure_never_fabricates_a_context() {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        assert!(registry.dynamic_available("kernel32.dll", "CreateActCtxW"));
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.spawn("activation", PARK_ADDR, &[], 65536).unwrap();
        let data = 0x410000;
        machine.cpu.map(data, 4096, Prot::RW).unwrap();
        for (pointer, size, flags, expected) in [
            (0, 32, 0, 998),
            (data, 31, 0, 87),
            (data, 32, 0x100, 87),
            (data, 32, 0, 14001),
        ] {
            machine.cpu.write_u32(data, size).unwrap();
            machine.cpu.write_u32(data + 4, flags).unwrap();
            let mut env = Env {
                context: 0,
                cpu: &mut machine.cpu,
                sys: &mut machine.sys,
                ext: &mut machine.ext,
                thunk: 0,
            };
            assert_eq!(create_w(&mut env, GuestPtr::new(pointer)).0, u32::MAX);
            assert_eq!(crate::util::last_error(&env), expected);
        }
    }
}
