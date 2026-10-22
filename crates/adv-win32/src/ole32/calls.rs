//! OLE helpers call the guest's actual queried interfaces and balance their references.
use super::*;
use adv_abi::{Handle, WideStr, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
const POINTER: u32 = 0x80004003;
const MEMORY: u32 = 0x8007000e;
pub(crate) fn method(env: &Env<'_>, this: u32, slot: u32) -> Result<u32, u32> {
    check_range(env.cpu, this, 4, Prot::READ).map_err(|_| POINTER)?;
    let table = env.cpu.read_u32(this).map_err(|_| POINTER)?;
    let entry = table
        .checked_add(slot.checked_mul(4).ok_or(POINTER)?)
        .ok_or(POINTER)?;
    check_range(env.cpu, entry, 4, Prot::READ).map_err(|_| POINTER)?;
    let target = env.cpu.read_u32(entry).map_err(|_| POINTER)?;
    check_range(env.cpu, target, 1, Prot::EXEC).map_err(|_| POINTER)?;
    Ok(target)
}
fn free(env: &mut Env<'_>, address: u32) {
    env.ext
        .get_mut::<adv_os::Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, address);
}
fn iid(first: u32) -> [u8; 16] {
    let mut iid = adv_os::com::IUNKNOWN;
    iid[..4].copy_from_slice(&first.to_le_bytes());
    iid
}
fn forward(
    env: &mut Env<'_>,
    object: u32,
    interface: [u8; 16],
    slot: u32,
    args: Vec<u32>,
    missing_success: bool,
) -> Outcome {
    let target = match method(env, object, 0) {
        Ok(target) => target,
        Err(error) => return Outcome::Return(error),
    };
    let Some(scratch) =
        env.ext
            .get_mut::<adv_os::Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, 20, true)
    else {
        return Outcome::Return(MEMORY);
    };
    if env.cpu.write(scratch, &interface).is_err() {
        free(env, scratch);
        return Outcome::Return(POINTER);
    }
    Outcome::call(
        target,
        [object, scratch, scratch + 16],
        move |env, result| {
            let result = result.value();
            if result & 0x80000000 != 0 {
                free(env, scratch);
                return Outcome::Return(if missing_success && result == 0x80004002 {
                    0
                } else {
                    result
                });
            }
            let pointer = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            let release = match method(env, pointer, 2) {
                Ok(target) => target,
                Err(error) => {
                    free(env, scratch);
                    return Outcome::Return(error);
                }
            };
            let target = match method(env, pointer, slot) {
                Ok(target) => target,
                Err(error) => {
                    return Outcome::call(release, [pointer], move |env, _| {
                        free(env, scratch);
                        Outcome::Return(error)
                    });
                }
            };
            let mut arguments = vec![pointer];
            arguments.extend(args);
            Outcome::call(target, arguments, move |env, value| {
                let value = value.value();
                // The method may have installed a vtable hook while executing guest code.
                let target = match method(env, pointer, 2) {
                    Ok(target) => target,
                    Err(error) => {
                        free(env, scratch);
                        return Outcome::Return(error);
                    }
                };
                Outcome::call(target, [pointer], move |env, _| {
                    free(env, scratch);
                    Outcome::Return(value)
                })
            })
        },
    )
}
pub(super) fn register(registry: &mut Registry) {
    for api in [draw_ABI, lock_ABI, blanket_ABI, equal_ABI] {
        registry.register(api);
    }
}
#[win32(ole32,name="IsEqualGUID",level=implement)]
fn equal(env: &mut Env<'_>, left: GuestPtr<[u8; 16]>, right: GuestPtr<[u8; 16]>) -> i32 {
    match (left.read(env.cpu), right.read(env.cpu)) {
        (Ok(a), Ok(b)) => i32::from(a == b),
        _ => 0,
    }
}
#[win32(ole32,name="OleDraw",level=implement)]
fn draw(
    env: &mut Env<'_>,
    object: GuestPtr<()>,
    aspect: u32,
    dc: Handle,
    bounds: GuestPtr<()>,
) -> Outcome {
    if check_range(env.cpu, bounds.addr(), 16, Prot::READ).is_err() {
        return Outcome::Return(0x80070057);
    }
    forward(
        env,
        object.addr(),
        iid(0x10d),
        3,
        vec![aspect, u32::MAX, 0, 0, 0, dc.0, bounds.addr(), 0, 0, 0],
        false,
    )
}
#[win32(ole32,name="OleLockRunning",level=implement)]
fn lock(env: &mut Env<'_>, object: GuestPtr<()>, lock: i32, last: i32) -> Outcome {
    forward(
        env,
        object.addr(),
        iid(0x126),
        6,
        vec![lock as u32, last as u32],
        true,
    )
}
#[win32(ole32,name="CoSetProxyBlanket",level=implement)]
fn blanket(
    env: &mut Env<'_>,
    proxy: GuestPtr<()>,
    authentication: u32,
    authorization: u32,
    principal: WideStr,
    level: u32,
    impersonation: u32,
    information: GuestPtr<()>,
    capabilities: u32,
) -> Outcome {
    forward(
        env,
        proxy.addr(),
        iid(0x13d),
        4,
        vec![
            proxy.addr(),
            authentication,
            authorization,
            principal.0.addr(),
            level,
            impersonation,
            information.addr(),
            capabilities,
        ],
        false,
    )
}

pub(crate) type ScalarThen = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;
/// Compose a scalar COM result while preserving yielding guest calls and host waits.
pub(crate) fn complete(env: &mut Env<'_>, outcome: Outcome, then: ScalarThen) -> Outcome {
    match outcome {
        Outcome::Return(value) => then(env, value),
        Outcome::Call {
            target,
            args,
            then: next,
            stack,
            unwindable,
        } => Outcome::Call {
            target,
            args,
            stack,
            unwindable,
            then: Box::new(move |env, value| {
                let outcome = next(env, value);
                complete(env, outcome, then)
            }),
        },
        Outcome::Block {
            wait,
            then: next,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, value| {
                let outcome = next(env, value);
                complete(env, outcome, then)
            }),
        },
        Outcome::Defer { then: next } => Outcome::defer(move |env| {
            let outcome = next(env);
            complete(env, outcome, then)
        }),
        Outcome::HostRequest { then: next } => Outcome::host_request(move |env| {
            let outcome = next(env);
            complete(env, outcome, then)
        }),
        terminal => terminal,
    }
}
