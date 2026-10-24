//! Wine's wait_suspend and abort_thread at machine scheduling boundaries.
use crate::{
    context, protocol, server,
    status::{self, Error},
};
use adv_cpu::backend::ArchitecturalState;
use adv_machine::{CtxId, Env, Machine};
use std::collections::BTreeMap;

struct Suspended {
    image: [u8; context::SIZE],
    state: ArchitecturalState,
    initial: Option<u32>,
    sent: bool,
}
#[derive(Default)]
struct Signals(BTreeMap<CtxId, Suspended>);
#[derive(Default)]
struct Pending(Vec<CtxId>);
pub(crate) fn queue(env: &mut Env<'_>, id: CtxId) {
    env.ext.get_or_default::<Pending>().0.push(id);
    env.sys.suspend_context(id);
}

pub fn suspend(env: &mut Env<'_>) -> Result<(), Error> {
    if env
        .ext
        .get_or_default::<Signals>()
        .0
        .contains_key(&env.context)
    {
        return Ok(());
    }
    let state = env
        .cpu
        .capture_architecture()
        .map_err(|e| Error::Unsupported(e.to_string()))?;
    let initial = (env.sys.set_initial_registers(env.context, &[])
        || env.ext.get::<super::Ntdll>().get("LdrInitializeThunk")
            == Some(env.cpu.reg(adv_cpu::backend::Reg::Eip)))
    .then(|| env.sys.contexts[env.context as usize].stack_hi - 16 - context::SIZE as u32);
    let mut image = context::encode(&state);
    if let Some(at) = initial {
        env.cpu
            .read(at, &mut image)
            .map_err(|_| status::ACCESS_VIOLATION)?;
    }
    env.ext.get_mut::<Signals>().0.insert(
        env.context,
        Suspended {
            image,
            state,
            initial,
            sent: false,
        },
    );
    select(env)
}

fn select(env: &mut Env<'_>) -> Result<(), Error> {
    let context = env.context;
    let mut suspended = env.ext.get_mut::<Signals>().0.remove(&context).unwrap();
    let mut previous = 0;
    let mut result = [0; 40];
    loop {
        let wire = context::to_server(&suspended.image);
        let data = if suspended.sent { &[][..] } else { &wire[..] };
        let mut request = protocol::SelectRequest {
            prev_apc: previous,
            flags: 2,
            cookie: u64::MAX - 1,
            timeout: 0,
            ..Default::default()
        };
        let mut reply_data = vec![0; 64 + 3440];
        let (reply, size) = server::request(env, &mut request, &[&result, data], &mut reply_data)?;
        suspended.sent = true;
        if reply.header.error == 0x100 {
            result = crate::apc::invoke(env, &reply_data[..64], false)?;
            previous = reply.apc_handle;
            continue;
        }
        if reply.signaled == 0 {
            server::begin_wait(env)?;
            if env.sys.contexts[context as usize].suspend_count == 0 {
                env.sys.suspend_context(context);
            }
            env.ext.get_mut::<Signals>().0.insert(context, suspended);
        } else {
            if size >= 64 + 1720 {
                context::from_server(&reply_data[64..], &mut suspended.image);
            }
            if let Some(at) = suspended.initial {
                env.cpu
                    .write(at, &suspended.image)
                    .map_err(|_| status::ACCESS_VIOLATION)?;
            } else {
                // A signal can interrupt either guest code or a parked host call.
                // Only register parts the server changed replace that saved state.
                context::decode(&suspended.image, &mut suspended.state)
                    .map_err(Error::Unsupported)?;
                env.cpu
                    .restore_architecture(&suspended.state)
                    .map_err(|e| Error::Unsupported(e.to_string()))?;
            }
            if env.sys.contexts[context as usize].suspend_count != 0 {
                env.sys.resume_context(context);
            }
        }
        return Ok(());
    }
}

pub fn deliver(machine: &mut Machine<adv_cpu::Cpu>, tid: i32, sig: i32) -> Result<(), String> {
    let ids: Vec<_> = machine
        .sys
        .contexts
        .iter()
        .filter(|c| !c.is_done() && (tid < 0 || c.id as i32 + 1 == tid))
        .map(|c| c.id)
        .collect();
    for id in ids {
        machine
            .visit_context(Some(id), |env| {
                if sig == libc::SIGQUIT || sig == libc::SIGKILL {
                    env.ext.get_or_default::<Signals>().0.remove(&id);
                    env.ext
                        .get_or_default::<Pending>()
                        .0
                        .retain(|&context| context != id);
                    server::end_thread(env, id);
                    env.sys.terminate_context_stopped(id, 0);
                    Ok(())
                } else if sig == libc::SIGUSR1 {
                    suspend(env)
                } else {
                    Ok(())
                }
            })?
            .map_err(|e| format!("Wine signal: {e:?}"))?;
    }
    Ok(())
}

pub fn wake(machine: &mut Machine<adv_cpu::Cpu>) -> Result<bool, String> {
    let pending = std::mem::take(&mut machine.ext.get_or_default::<Pending>().0);
    for id in pending {
        machine
            .visit_context(Some(id), suspend)?
            .map_err(|e| format!("initial suspend: {e:?}"))?;
    }
    let ids: Vec<_> = machine
        .ext
        .try_get::<Signals>()
        .map(|s| s.0.keys().copied().collect())
        .unwrap_or_default();
    let mut changed = false;
    for id in ids {
        changed |= machine
            .visit_context(Some(id), |env| match server::poll_wake(env) {
                Ok(Some(wake)) if wake.cookie == 0 => {
                    env.ext.get_mut::<Signals>().0.remove(&id);
                    server::end_thread(env, id);
                    env.sys.terminate_context_stopped(id, wake.signaled);
                    Ok(true)
                }
                Ok(Some(_)) => select(env).map(|_| true),
                Ok(None) => Ok(false),
                Err(e) => Err(e),
            })?
            .map_err(|e| format!("Wine signal wake: {e:?}"))?;
    }
    Ok(changed)
}
