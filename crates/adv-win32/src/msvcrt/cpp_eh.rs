//! x86 MSVC C++ EH metadata is guest memory. Search and cleanup use the live
//! Windows exception chain; catch funclets retain their guest establisher EBP.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

const CODE: u32 = 0xe06d7363;
const MAGIC: u32 = 0x19930520;
type Done = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;

struct Object {
    address: u32,
    info: u32,
}
struct Cleanup {
    stack: u32,
}
struct Catch {
    object: RefCell<Option<Rc<Object>>>,
    frame: u32,
    low: i32,
}
#[derive(Default)]
pub(super) struct Thread {
    pending: Vec<Rc<Object>>,
    catches: Vec<Weak<Catch>>,
    translating: Weak<()>,
    cleanup: Vec<Weak<Cleanup>>,
}
fn eh_module(module: &str) -> &str {
    match crt_module(module) {
        "vcruntime140.dll" => "ucrtbase.dll",
        module => module,
    }
}
fn thread<'a>(env: &'a mut Env<'_>) -> &'a mut Thread {
    let key = (
        eh_module(&env.sys.thunks.get(env.thunk).module).to_owned(),
        env.context,
    );
    env.ext
        .get_or_default::<CrtState>()
        .cpp_eh_threads
        .entry(key)
        .or_default()
}

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("_CxxThrowException", throw),
            ("__uncaught_exception", uncaught),
            ("__uncaught_exceptions", uncaught_count),
            ("__CxxFrameHandler", frame_handler),
            ("__CxxFrameHandler2", frame_handler),
            ("__CxxFrameHandler3", frame_handler),
        ],
    );
}

fn read<const N: usize>(env: &Env<'_>, address: u32) -> Result<[u32; N], MemFault> {
    let mut result = [0; N];
    for (i, value) in result.iter_mut().enumerate() {
        *value = word(env, address.wrapping_add(i as u32 * 4))?;
    }
    Ok(result)
}
fn word(env: &Env<'_>, address: u32) -> Result<u32, MemFault> {
    GuestPtr::new(address).read(env.cpu)
}
fn put(env: &mut Env<'_>, address: u32, value: u32) -> Result<(), MemFault> {
    GuestPtr::new(address).write(env.cpu, value)
}

fn throw(env: &mut Env<'_>) -> Outcome {
    let (address, info) = (env.arg(0), env.arg(1));
    if cpp_runtime::is_terminating(env) {
        return crate::kernel32::exit_process(env, 3);
    }
    let object = if info == 0 {
        let active = thread(env)
            .catches
            .iter()
            .rev()
            .filter_map(Weak::upgrade)
            .find_map(|catch| catch.object.borrow().clone());
        match active {
            Some(object) => object,
            None => return cpp_runtime::terminate(env),
        }
    } else {
        Rc::new(Object { address, info })
    };
    let information = [MAGIC, object.address, object.info];
    thread(env).pending.push(object);
    crate::kernel32::raise_status(env, CODE, 1, &information, env.caller())
}

#[derive(Clone, Copy)]
struct Function {
    frame: u32,
    states: u32,
    unwind: u32,
    tries: u32,
    blocks: u32,
    synchronous: bool,
}
#[derive(Clone, Copy)]
struct Block {
    low: i32,
    high: i32,
    catch_high: i32,
    count: u32,
    handlers: u32,
}
#[derive(Clone, Copy)]
struct Handler {
    adjectives: u32,
    ty: u32,
    displacement: i32,
    entry: u32,
}
#[derive(Clone, Copy)]
struct Conversion {
    properties: u32,
    ty: u32,
    mdisp: i32,
    pdisp: i32,
    vdisp: i32,
    size: u32,
    copy: u32,
}

fn function(env: &Env<'_>, frame: u32, info: u32) -> Result<Function, MemFault> {
    let [magic, states, unwind, tries, blocks] = read(env, info)?;
    // VC6, VC7 and VC8+ retain these absolute x86 metadata fields.
    if !(MAGIC..=MAGIC + 2).contains(&(magic & 0x1fffffff)) || states > 65536 || tries > 65536 {
        return Err(MemFault {
            addr: info,
            len: 20,
        });
    }
    Ok(Function {
        frame,
        states,
        unwind,
        tries,
        blocks,
        synchronous: magic & 0x1fffffff >= MAGIC + 2 && word(env, info.wrapping_add(32))? & 1 != 0,
    })
}
fn block(env: &Env<'_>, function: Function, index: u32) -> Result<Block, MemFault> {
    let [low, high, catch_high, count, handlers] =
        read(env, function.blocks.wrapping_add(index * 20))?;
    Ok(Block {
        low: low as i32,
        high: high as i32,
        catch_high: catch_high as i32,
        count,
        handlers,
    })
}
fn handler(env: &Env<'_>, block: Block, index: u32) -> Result<Handler, MemFault> {
    let [adjectives, ty, displacement, entry] = read(env, block.handlers.wrapping_add(index * 16))?;
    Ok(Handler {
        adjectives,
        ty,
        displacement: displacement as i32,
        entry,
    })
}
fn conversion(env: &Env<'_>, address: u32) -> Result<Conversion, MemFault> {
    let [properties, ty, mdisp, pdisp, vdisp, size, copy] = read(env, address)?;
    Ok(Conversion {
        properties,
        ty,
        mdisp: mdisp as i32,
        pdisp: pdisp as i32,
        vdisp: vdisp as i32,
        size,
        copy,
    })
}
fn same_type(env: &Env<'_>, a: u32, b: u32) -> Result<bool, MemFault> {
    if a == b {
        return Ok(true);
    }
    for offset in 8..65536 {
        let x = GuestPtr::<u8>::new(a.wrapping_add(offset)).read(env.cpu)?;
        let y = GuestPtr::<u8>::new(b.wrapping_add(offset)).read(env.cpu)?;
        if x != y {
            return Ok(false);
        }
        if x == 0 {
            return Ok(true);
        }
    }
    Err(MemFault {
        addr: a,
        len: 65536,
    })
}
fn matching(
    env: &Env<'_>,
    object: &Object,
    handler: Handler,
) -> Result<Option<Conversion>, MemFault> {
    let [attributes, _, _, array] = read(env, object.info)?;
    if attributes & 1 != 0 && handler.adjectives & 1 == 0
        || attributes & 2 != 0 && handler.adjectives & 2 == 0
    {
        return Ok(None);
    }
    let count = word(env, array)?;
    if count > 65536 {
        return Err(MemFault {
            addr: array,
            len: 4,
        });
    }
    for i in 0..count {
        let candidate = conversion(env, word(env, array.wrapping_add(4 + i * 4))?)?;
        if (candidate.properties & 2 == 0 || handler.adjectives & 8 != 0)
            && same_type(env, handler.ty, candidate.ty)?
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}
fn adjusted(env: &Env<'_>, object: u32, c: Conversion) -> Result<u32, MemFault> {
    let mut result = object.wrapping_add(c.mdisp as u32);
    if c.pdisp >= 0 {
        let vbptr = object.wrapping_add(c.pdisp as u32);
        result = result
            .wrapping_add(c.pdisp as u32)
            .wrapping_add(word(env, word(env, vbptr)?.wrapping_add(c.vdisp as u32))?);
    }
    Ok(result)
}

fn frame_handler(env: &mut Env<'_>) -> Outcome {
    let (record, frame, context, info) =
        (env.arg(0), env.arg(1), env.arg(2), env.cpu.reg(Reg::Eax));
    let function = match function(env, frame, info) {
        Ok(f) => f,
        Err(e) => return memory_failure(e),
    };
    let [code, flags] = match read(env, record) {
        Ok(r) => r,
        Err(e) => return memory_failure(e),
    };
    if flags & 6 != 0 {
        return unwind(env, function, -1, 0, Box::new(|_| Outcome::Return(1)));
    }
    if code != CODE {
        return translate(env, function, record, context);
    }
    if thread(env)
        .cleanup
        .iter()
        .filter_map(Weak::upgrade)
        .any(|guard| frame >= guard.stack)
    {
        return cpp_runtime::terminate(env);
    }
    let [magic, address, info] = match read(env, record.wrapping_add(20)) {
        Ok(r) => r,
        Err(e) => return memory_failure(e),
    };
    if !(MAGIC..=MAGIC + 2).contains(&magic) {
        return Outcome::Return(1);
    }
    let object = thread(env)
        .pending
        .iter()
        .rev()
        .find(|o| o.address == address && o.info == info)
        .cloned()
        .unwrap_or_else(|| Rc::new(Object { address, info }));
    let state = match word(env, frame.wrapping_add(8)) {
        Ok(s) => s as i32,
        Err(e) => return memory_failure(e),
    };
    for i in 0..function.tries {
        let block = match block(env, function, i) {
            Ok(b) => b,
            Err(e) => return memory_failure(e),
        };
        if state < block.low || state > block.high {
            continue;
        }
        if block.count > 65536 {
            return Outcome::Fail("invalid C++ catch count".into());
        }
        for j in 0..block.count {
            let handler = match handler(env, block, j) {
                Ok(h) => h,
                Err(e) => return memory_failure(e),
            };
            let candidate = if handler.ty == 0 {
                None
            } else {
                match matching(env, &object, handler) {
                    Ok(Some(c)) => Some(c),
                    Ok(None) => continue,
                    Err(e) => return memory_failure(e),
                }
            };
            return seh::unwind_global(
                env,
                frame,
                record,
                Box::new(move |env| {
                    unwind(
                        env,
                        function,
                        block.low,
                        0,
                        Box::new(move |env| {
                            enter_catch(env, function, block, handler, candidate, object)
                        }),
                    )
                }),
            );
        }
    }
    Outcome::Return(1)
}

fn unwind(env: &mut Env<'_>, function: Function, stop: i32, mut count: u32, then: Done) -> Outcome {
    loop {
        let current = match word(env, function.frame.wrapping_add(8)) {
            Ok(v) => v as i32,
            Err(e) => return memory_failure(e),
        };
        if current <= stop {
            return then(env);
        }
        if current < 0 || current as u32 >= function.states || count >= 65536 {
            return Outcome::Fail("invalid C++ unwind state".into());
        }
        let [previous, action] = match read(env, function.unwind.wrapping_add(current as u32 * 8)) {
            Ok(v) => v,
            Err(e) => return memory_failure(e),
        };
        if let Err(e) = put(env, function.frame.wrapping_add(8), previous) {
            return memory_failure(e);
        }
        if action != 0 {
            let ebp = env.cpu.reg(Reg::Ebp);
            env.cpu.set_reg(Reg::Ebp, function.frame.wrapping_add(12));
            let guard = cleanup_guard(env);
            return Outcome::call(action, [], move |env, _| {
                drop(guard);
                env.cpu.set_reg(Reg::Ebp, ebp);
                unwind(env, function, stop, count + 1, then)
            })
            .unwindable();
        }
        count += 1;
    }
}

fn enter_catch(
    env: &mut Env<'_>,
    function: Function,
    block: Block,
    handler: Handler,
    candidate: Option<Conversion>,
    object: Rc<Object>,
) -> Outcome {
    let destination = function
        .frame
        .wrapping_add(12)
        .wrapping_add(handler.displacement as u32);
    let caught = object.clone();
    let then: Done = Box::new(move |env| call_catch(env, function, block, handler, caught));
    let Some(candidate) = candidate else {
        return then(env);
    };
    if handler.displacement == 0 {
        return then(env);
    }
    let source = match adjusted(env, object.address, candidate) {
        Ok(p) => p,
        Err(e) => return memory_failure(e),
    };
    if handler.adjectives & 8 != 0 {
        return match put(env, destination, source) {
            Ok(()) => then(env),
            Err(e) => memory_failure(e),
        };
    }
    if candidate.copy != 0 && candidate.properties & 1 == 0 {
        let ecx = env.cpu.reg(Reg::Ecx);
        env.cpu.set_reg(Reg::Ecx, destination);
        let mut args = vec![source];
        if candidate.properties & 4 != 0 {
            args.push(1);
        }
        let guard = cleanup_guard(env);
        return Outcome::call(candidate.copy, args, move |env, _| {
            drop(guard);
            env.cpu.set_reg(Reg::Ecx, ecx);
            then(env)
        })
        .unwindable();
    }
    // Simple types carry their actual bytes; pointer catches adjust the pointee.
    if candidate.properties & 1 != 0 && candidate.size == 4 {
        let value = match word(env, object.address) {
            Ok(v) => v,
            Err(e) => return memory_failure(e),
        };
        let value = if value == 0 {
            0
        } else {
            match adjusted(env, value, candidate) {
                Ok(v) => v,
                Err(e) => return memory_failure(e),
            }
        };
        if let Err(e) = put(env, destination, value) {
            return memory_failure(e);
        }
    } else {
        if let Err(e) = adv_abi::guest::check_range(
            env.cpu,
            source,
            candidate.size,
            adv_cpu::backend::Prot::READ,
        ) {
            return memory_failure(e);
        }
        if let Err(e) = adv_abi::guest::check_range(
            env.cpu,
            destination,
            candidate.size,
            adv_cpu::backend::Prot::WRITE,
        ) {
            return memory_failure(e);
        }
        let mut buffer = [0; 4096];
        let mut offset = 0;
        while offset < candidate.size {
            let length = (candidate.size - offset).min(4096);
            if let Err(e) = env
                .cpu
                .read(source + offset, &mut buffer[..length as usize])
                .and_then(|()| {
                    env.cpu
                        .write(destination + offset, &buffer[..length as usize])
                })
            {
                return memory_failure(e);
            }
            offset += length;
        }
    }
    then(env)
}

fn call_catch(
    env: &mut Env<'_>,
    function: Function,
    block: Block,
    handler: Handler,
    object: Rc<Object>,
) -> Outcome {
    if let Err(e) = put(env, function.frame.wrapping_add(8), block.catch_high as u32) {
        return memory_failure(e);
    }
    let state = thread(env);
    state.pending.retain(|o| !Rc::ptr_eq(o, &object));
    state.catches.retain(|c| c.strong_count() != 0);
    // Retire displaced catch ownership before invoking an outer catch. The
    // continuation itself is retired only by checked machine restoration.
    let mut retired = Vec::new();
    for catch in state.catches.iter().filter_map(Weak::upgrade) {
        if (catch.frame < function.frame || catch.frame == function.frame && catch.low >= block.low)
            && let Some(object) = catch.object.borrow_mut().take()
        {
            retired.push(object);
        }
    }
    let then: Done = Box::new(move |env| start_catch(env, function, block, handler, object));
    destroy_retired(env, retired, then)
}

fn destroy_retired(env: &mut Env<'_>, mut retired: Vec<Rc<Object>>, then: Done) -> Outcome {
    while let Some(object) = retired.pop() {
        if Rc::strong_count(&object) == 1 {
            return destroy(
                env,
                object,
                Box::new(move |env| destroy_retired(env, retired, then)),
            );
        }
    }
    then(env)
}

fn start_catch(
    env: &mut Env<'_>,
    function: Function,
    block: Block,
    handler: Handler,
    object: Rc<Object>,
) -> Outcome {
    let guard = Rc::new(Catch {
        object: RefCell::new(Some(object)),
        frame: function.frame,
        low: block.low,
    });
    thread(env).catches.push(Rc::downgrade(&guard));
    let ebp = env.cpu.reg(Reg::Ebp);
    env.cpu.set_reg(Reg::Ebp, function.frame.wrapping_add(12));
    Outcome::call(handler.entry, [], move |env, result| {
        env.cpu.set_reg(Reg::Ebp, ebp);
        let object = guard.object.borrow_mut().take();
        drop(guard);
        let next = result.value();
        let then: Done = Box::new(move |env| {
            let previous = match word(env, function.unwind.wrapping_add(block.low as u32 * 8)) {
                Ok(s) => s,
                Err(e) => return memory_failure(e),
            };
            if let Err(e) = put(env, function.frame.wrapping_add(8), previous) {
                return memory_failure(e);
            }
            let stack = match word(env, function.frame.wrapping_sub(4)) {
                Ok(s) => s,
                Err(e) => return memory_failure(e),
            };
            match env.checkpoint_for_stack_unwind(stack) {
                Ok(mut checkpoint) => {
                    checkpoint.state.regs[5] = function.frame.wrapping_add(12);
                    checkpoint.state.eip = next;
                    Outcome::ResumeExecution(checkpoint)
                }
                Err(reason) => Outcome::Fail(format!("C++ catch continuation: {reason}")),
            }
        });
        match object {
            Some(object) if Rc::strong_count(&object) == 1 => destroy(env, object, then),
            _ => then(env),
        }
    })
    .unwindable()
}

fn destroy(env: &mut Env<'_>, object: Rc<Object>, then: Done) -> Outcome {
    let destructor = match word(env, object.info.wrapping_add(4)) {
        Ok(d) => d,
        Err(e) => return memory_failure(e),
    };
    if destructor == 0 {
        return then(env);
    }
    let ecx = env.cpu.reg(Reg::Ecx);
    env.cpu.set_reg(Reg::Ecx, object.address);
    let guard = cleanup_guard(env);
    Outcome::call(destructor, [], move |env, _| {
        drop(guard);
        env.cpu.set_reg(Reg::Ecx, ecx);
        drop(object);
        then(env)
    })
}

fn cleanup_guard(env: &mut Env<'_>) -> Rc<Cleanup> {
    let guard = Rc::new(Cleanup {
        stack: env.cpu.reg(Reg::Esp),
    });
    let state = thread(env);
    state.cleanup.retain(|guard| guard.strong_count() != 0);
    state.cleanup.push(Rc::downgrade(&guard));
    guard
}

fn translate(env: &mut Env<'_>, function: Function, record: u32, context: u32) -> Outcome {
    let translator = cpp_runtime::translator(env);
    if translator == 0 || function.synchronous || thread(env).translating.upgrade().is_some() {
        return Outcome::Return(1);
    }
    let code = match word(env, record) {
        Ok(code) => code,
        Err(fault) => return memory_failure(fault),
    };
    let pointers = env.cpu.reg(Reg::Esp).wrapping_sub(8) & !3;
    if let Err(fault) = GuestPtr::new(pointers).write(env.cpu, [record, context]) {
        return memory_failure(fault);
    }
    let guard = Rc::new(());
    thread(env).translating = Rc::downgrade(&guard);
    Outcome::call_on_stack(translator, [code, pointers], pointers, move |_, _| {
        drop(guard);
        Outcome::Return(1)
    })
    .unwindable()
}

fn uncaught_count(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(thread(env).pending.len() as u32)
}

pub(super) fn uncaught(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(u32::from(!thread(env).pending.is_empty()))
}

pub(crate) fn unhandled(env: &mut Env<'_>, code: u32) -> Option<Outcome> {
    if code != CODE {
        return None;
    }
    let owner = env
        .ext
        .try_get::<CrtState>()?
        .cpp_eh_threads
        .iter()
        .find(|((_, id), state)| *id == env.context && !state.pending.is_empty())
        .map(|((module, _), _)| module.clone())?;
    Some(cpp_runtime::terminate_for(env, &owner))
}
