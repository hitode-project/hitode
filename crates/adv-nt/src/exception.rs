//! CPU exceptions, delivered as Wine's i386 signal handler delivers them:
//! the exception record and CONTEXT go on the thread's stack, and the thread
//! resumes in ntdll.dll's `KiUserExceptionDispatcher`, which searches the
//! vectored and frame handlers.

mod mapping;

use adv_cpu::backend::GuestExt;
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

/// Where ntdll.dll takes exceptions.
pub struct Dispatcher(pub u32);

/// `struct exc_stack_layout`: the arguments, the record, the CONTEXT and its
/// CONTEXT_EX, 64-byte aligned.
const RECORD: u32 = 0x08;
const CONTEXT: u32 = 0x58;
const CONTEXT_EX: u32 = 0x324;
const LAYOUT_SIZE: u32 = 0x340;
/// Single step, direction and alignment check are clear in the dispatcher.
const DISPATCH_CLEARED_FLAGS: u32 = 0x100 | 0x400 | 0x4_0000;

/// The machine's exception handler for NT processes.
pub fn deliver(env: &mut Env<'_>, event: Exception, mut checkpoint: Checkpoint) -> Outcome {
    let record = match mapping::map(env.cpu, &event, &checkpoint.state) {
        Ok(record) => record,
        Err(reason) => return Outcome::Fail(reason),
    };
    mapping::normalize_context(&record, &mut checkpoint.state);
    enter_dispatcher(env, checkpoint, &record)
}

/// Resumes the thread in the dispatcher with `record` and the CONTEXT of
/// `checkpoint` on its stack.
fn enter_dispatcher(env: &mut Env<'_>, mut checkpoint: Checkpoint, record: &Record) -> Outcome {
    let Some(&Dispatcher(dispatcher)) = env.ext.try_get::<Dispatcher>() else {
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
    // CONTEXT_EX without extended state, as Wine's context_init_xstate.
    let size = context::SIZE as u32;
    put(CONTEXT_EX, size.wrapping_neg());
    put(CONTEXT_EX + 4, size + 24);
    put(CONTEXT_EX + 8, size.wrapping_neg());
    put(CONTEXT_EX + 12, size);
    put(CONTEXT_EX + 20, 25);
    let encoded = context::encode(&checkpoint.state);
    frame[CONTEXT as usize..CONTEXT as usize + context::SIZE].copy_from_slice(&encoded);
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

/// NtContinue(*context, alert): resumes the thread with the given CONTEXT.
pub fn resume(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let mut bytes = [0u8; context::SIZE];
    if env.cpu.read(args[0], &mut bytes).is_err() {
        return Outcome::Return(crate::status::ACCESS_VIOLATION);
    }
    let mut checkpoint = match env.checkpoint() {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Outcome::Fail(format!("NtContinue: {error}")),
    };
    if let Err(error) = context::decode(&bytes, &mut checkpoint.state) {
        return Outcome::Fail(format!("NtContinue: {error}"));
    }
    Outcome::ResumeExecution(checkpoint)
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
    enter_dispatcher(env, checkpoint, &raised)
}
