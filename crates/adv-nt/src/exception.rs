//! CPU exceptions, delivered as Wine's i386 signal handler delivers them:
//! the exception record and CONTEXT go on the thread's stack, and the thread
//! resumes in ntdll.dll's `KiUserExceptionDispatcher`, which searches the
//! vectored and frame handlers.

mod mapping;

use adv_cpu::backend::{ArchitecturalState, GuestExt};
use adv_machine::exception::{Checkpoint, Exception};
use adv_machine::{Env, Outcome};

use crate::context;

pub struct Record {
    pub code: u32,
    pub flags: u32,
    /// A nested EXCEPTION_RECORD, for an exception raised during dispatch.
    pub nested: u32,
    pub address: u32,
    pub count: usize,
    pub information: [u32; 15],
}

/// Where ntdll.dll takes exceptions and user APCs.
#[derive(Clone, Copy)]
pub struct Dispatchers {
    pub exception: u32,
    pub apc: u32,
}

/// `struct exc_stack_layout`: the arguments, the record, the CONTEXT and its
/// CONTEXT_EX, 64-byte aligned.
const RECORD: u32 = 0x08;
const CONTEXT: u32 = 0x58;
const LAYOUT_SIZE: u32 = 0x340;
/// Single step, direction and alignment check are clear in the dispatcher.
const DISPATCH_CLEARED_FLAGS: u32 = 0x100 | 0x400 | 0x4_0000;

/// The machine's exception handler for NT processes.
pub fn deliver(env: &mut Env<'_>, event: Exception, mut checkpoint: Checkpoint) -> Outcome {
    if matches!(event, Exception::Trap { vector: 45, .. })
        && matches!(checkpoint.state.regs[0], 1 | 3 | 4 | 5)
    {
        checkpoint.state.eip += 1;
        return Outcome::ResumeExecution(checkpoint);
    }
    if let Exception::Fault(ref fault) = event
        && let adv_cpu::backend::FaultKind::Write(address) = fault.kind
        && crate::memory::write_watch_fault(env, address)
    {
        return Outcome::ResumeExecution(checkpoint);
    }
    let mut record = match mapping::map(env.cpu, &event, &checkpoint.state) {
        Ok(record) => record,
        Err(reason) => return Outcome::Fail(reason),
    };
    if record.code == 0xc000_0005
        && record.information[0] == 8
        && env
            .ext
            .get_or_default::<crate::memory::Memory>()
            .execute_readable
    {
        record.information[0] = 0;
    }
    mapping::normalize_context(&record, &mut checkpoint.state);
    enter_dispatcher(env, checkpoint, &record)
}

/// Resumes the thread in the dispatcher with `record` and the CONTEXT of
/// `checkpoint` on its stack.
fn enter_dispatcher(env: &mut Env<'_>, mut checkpoint: Checkpoint, record: &Record) -> Outcome {
    let Some(&Dispatchers {
        exception: dispatcher,
        ..
    }) = env.ext.try_get::<Dispatchers>()
    else {
        return Outcome::Fail("no exception dispatcher".into());
    };

    let stack = (checkpoint.state.regs[4] - LAYOUT_SIZE) & !63;
    let mut frame = vec![0u8; LAYOUT_SIZE as usize];
    let mut put = |offset: u32, value: u32| {
        let offset = offset as usize;
        frame[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(0, stack + RECORD);
    put(4, stack + CONTEXT);
    put(RECORD, record.code);
    put(RECORD + 4, record.flags);
    put(RECORD + 8, record.nested);
    put(RECORD + 0xc, record.address);
    put(RECORD + 0x10, record.count as u32);
    for (index, value) in record.information[..record.count].iter().enumerate() {
        put(RECORD + 0x14 + 4 * index as u32, *value);
    }
    put_context(env, &mut frame, CONTEXT as usize, &checkpoint.state);
    if let Err(error) = env.cpu.write(stack, &frame) {
        return Outcome::Fail(format!("exception frame: {error}"));
    }
    debug_assert_eq!(env.cpu.read_u32(stack).ok(), Some(stack + RECORD));

    let state = &mut checkpoint.state;
    state.regs[4] = stack;
    state.eip = dispatcher;
    state.eflags &= !DISPATCH_CLEARED_FLAGS;
    Outcome::ResumeExecution(checkpoint)
}

/// Writes `state` as a CONTEXT at `offset` of `frame`, followed by its
/// CONTEXT_EX without extended state, as Wine's `context_init_xstate`.
pub fn put_context(env: &mut Env<'_>, frame: &mut [u8], offset: usize, state: &ArchitecturalState) {
    frame[offset..offset + context::SIZE].copy_from_slice(&context::encode(state));
    context::load_debug(env, env.context, &mut frame[offset..offset + context::SIZE]);
    let size = context::SIZE as u32;
    let extension = offset + context::SIZE;
    for (field, value) in [
        (0, size.wrapping_neg()),
        (4, size + 24),
        (8, size.wrapping_neg()),
        (12, size),
        (20, 25),
    ] {
        frame[extension + field..extension + field + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// NtContinue(*context, alertable): resumes the thread with the given
/// CONTEXT, after an APC that is due when alertable.
pub fn resume(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[context, alertable] = args else {
        unreachable!()
    };
    let mut bytes = [0u8; context::SIZE];
    if env.cpu.read(context, &mut bytes).is_err() {
        return Outcome::Return(crate::status::ACCESS_VIOLATION);
    }
    let mut state = match env.checkpoint() {
        Ok(checkpoint) => checkpoint.state,
        Err(error) => return Outcome::Fail(format!("NtContinue: {error}")),
    };
    if let Err(error) = context::decode(&bytes, &mut state) {
        return Outcome::Fail(format!("NtContinue: {error}"));
    }
    context::save_debug(env, env.context, &bytes);
    if alertable as u8 != 0 {
        return crate::sync::continue_alertable(env, state);
    }
    resume_with(env, state)
}

/// Resumes the thread with `state`.
pub fn resume_with(env: &mut Env<'_>, state: Box<ArchitecturalState>) -> Outcome {
    match env.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state = state;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(format!("NtContinue: {error}")),
    }
}

const MAXIMUM_PARAMETERS: usize = 15;

/// NtRaiseException(*record, *context, first_chance)
pub fn raise(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[record, context, first_chance] = args else {
        unreachable!()
    };
    let mut bytes = [0u8; 0x50];
    let mut image = [0u8; context::SIZE];
    if env.cpu.read(record, &mut bytes).is_err() || env.cpu.read(context, &mut image).is_err() {
        return Outcome::Return(crate::status::ACCESS_VIOLATION);
    }
    let word = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let count = (word(0x10) as usize).min(MAXIMUM_PARAMETERS);
    let mut information = [0; MAXIMUM_PARAMETERS];
    for (index, value) in information[..count].iter_mut().enumerate() {
        *value = word(0x14 + 4 * index);
    }
    let raised = Record {
        code: word(0),
        flags: word(4),
        nested: word(8),
        address: word(0xc),
        count,
        information,
    };
    if first_chance == 0 {
        // Nothing handled it; Wine would tell a debugger and end the process.
        return Outcome::Fail(format!(
            "unhandled exception {:#010x} at {:#010x}, parameters {:x?}",
            raised.code,
            raised.address,
            &raised.information[..count]
        ));
    }
    let mut checkpoint = match env.checkpoint() {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Outcome::Fail(format!("NtRaiseException: {error}")),
    };
    if let Err(error) = context::decode(&image, &mut checkpoint.state) {
        return Outcome::Fail(format!("NtRaiseException: {error}"));
    }
    context::save_debug(env, env.context, &image);
    enter_dispatcher(env, checkpoint, &raised)
}

/// Wine's loader.c stub_syscall raises EXCEPTION_WINE_STUB, rather than
/// returning an NTSTATUS. The exception names live in guest memory.
pub fn stub(env: &mut Env<'_>, name: &str) -> Outcome {
    let mut checkpoint = match env.checkpoint() {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Outcome::Fail(format!("stub context: {error}")),
    };
    let strings = checkpoint.state.regs[4] - 128;
    let mut bytes = b"ntdll\0".to_vec();
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    if env.cpu.write(strings, &bytes).is_err() {
        return Outcome::Return(crate::status::ACCESS_VIOLATION);
    }
    checkpoint.state.regs[4] = strings;
    let mut information = [0; 15];
    information[..2].copy_from_slice(&[strings, strings + 6]);
    let record = Record {
        code: 0x8000_0100,
        flags: 1,
        nested: 0,
        address: checkpoint.state.eip,
        count: 2,
        information,
    };
    enter_dispatcher(env, checkpoint, &record)
}
