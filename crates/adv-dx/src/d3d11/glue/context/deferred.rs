//! Deferred command lists and immediate-context state objects.
use super::*;
use crate::d3d11::{caps::FeatureLevel, object::CommandList};
use adv_abi::gfx_iids::*;

pub(in crate::d3d11::glue) fn create_deferred(env: &mut Env<'_>) -> Outcome {
    let (device, flags, out) = (env.arg(0), env.arg(1), env.arg(2));
    call((|| {
        write(env, out, 0u32)?;
        if flags != 0 {
            return Err(Error::InvalidArgument);
        }
        if ops::object(env, table(env), device)?
            .device_state()
            .ok_or(Error::InvalidArgument)?
            .flags
            & 1
            != 0
        {
            return Err(crate::dxgi::Error::InvalidCall.into());
        }
        let context = alloc(env, FACE.0)?;
        if let Err(error) = table(env).attach_deferred(&mut env.sys.objects, context, device, flags)
        {
            free(env, context);
            return Err(error.into());
        }
        write(env, out, context)
    })())
}

pub(in crate::d3d11::glue) fn create_state(env: &mut Env<'_>) -> Outcome {
    let [device, flags, levels, count, sdk, interface, chosen, out] =
        std::array::from_fn(|i| env.arg(i as u32));
    status((|| {
        optional(env, out, 0u32)?;
        let interface: Iid = read(env, interface)?;
        let d3d10 = match interface {
            IID_ID3D10DEVICE | IID_ID3D10DEVICE1 => true,
            IID_ID3D11DEVICE | IID_ID3D11DEVICE1 => false,
            _ => return Err(Error::InvalidArgument),
        };
        if flags & !1 != 0 || count == 0 || sdk != 7 {
            return Err(Error::InvalidArgument);
        }
        let level = device::report(env, device)?.level;
        let configured = &env.ext.get::<State>().devices.reports;
        let selected = array::<u32>(env, levels, count)?
            .into_iter()
            .find_map(|requested| {
                FeatureLevel::ALL.into_iter().find(|candidate| {
                    *candidate as u32 == requested
                        && (*candidate == level || configured[candidate.column()].is_some())
                })
            })
            .ok_or(crate::d3d11::caps::Error::Unsupported)?;
        let new_report = configured[selected.column()].clone();
        optional(env, chosen, selected as u32)?;
        if out == 0 {
            return Ok(1);
        }
        let object = new_state(env, device, selected, d3d10)?;
        if selected > level
            && let Some(report) = new_report
        {
            ops::object_mut(env, table(env), device)?
                .device_state_mut()
                .unwrap()
                .report = report;
        }
        write(env, out, object)?;
        Ok(0)
    })())
}

fn new_state(
    env: &mut Env<'_>,
    device: u32,
    level: FeatureLevel,
    d3d10: bool,
) -> Result<u32, Error> {
    let pointer = alloc(env, "ID3DDeviceContextState")?;
    if let Err(error) =
        table(env).attach_context_state(&mut env.sys.objects, pointer, device, level, d3d10)
    {
        free(env, pointer);
        return Err(error.into());
    }
    Ok(pointer)
}

pub(super) fn swap_state(env: &mut Env<'_>) -> Outcome {
    let (context, next, out) = (env.arg(0), env.arg(1), env.arg(2));
    let table = table(env);
    let mut released = Release::default();
    let result = (|| {
        if next == 0 {
            return Ok(());
        }
        optional(env, out, 0u32)?;
        let device = owner(env)?;
        let immediate = table
            .get(&env.sys.objects, context)
            .and_then(Object::context_data)
            .filter(|context| context.deferred.is_none())
            .ok_or(Error::InvalidArgument)?;
        let current = immediate.active_state;
        let previous_level = immediate.level.unwrap();
        let previous_d3d10 = immediate.d3d10;
        if next == current {
            if out != 0 {
                table.add_ref(&mut env.sys.objects, current)?;
                write(env, out, current)?;
            }
            return Ok(());
        }
        let next_object = table
            .get_mut(&mut env.sys.objects, next)
            .filter(|object| object.device() == Some(device))
            .and_then(Object::context_state)
            .filter(|state| state.state.is_some())
            .ok_or(Error::InvalidArgument)?;
        let (next_level, next_d3d10) = (next_object.level, next_object.d3d10);
        table.retain_bound(&mut env.sys.objects, next)?;
        let previous_object = match (current, out) {
            (0, 0) => Ok(0),
            (0, _) => new_state(env, device, previous_level, previous_d3d10),
            (_, 0) => Ok(current),
            (_, _) => table
                .add_ref(&mut env.sys.objects, current)
                .map(|_| current)
                .map_err(Error::from),
        };
        let previous_object = match previous_object {
            Ok(pointer) => pointer,
            Err(error) => {
                table.drop_binding(&mut env.sys.objects, next, &mut released);
                return Err(error);
            }
        };
        let next_state = table
            .get_mut(&mut env.sys.objects, next)
            .unwrap()
            .context_state()
            .unwrap()
            .state
            .take()
            .unwrap();
        let context = table
            .get_mut(&mut env.sys.objects, context)
            .unwrap()
            .context_data_mut()
            .unwrap();
        let previous = std::mem::replace(&mut context.state, next_state);
        context.active_state = next;
        context.level = Some(next_level);
        context.d3d10 = next_d3d10;
        if previous_object == 0 {
            previous.groups().for_each_binding(|pointer| {
                table.drop_binding(&mut env.sys.objects, pointer, &mut released)
            });
        } else {
            table
                .get_mut(&mut env.sys.objects, previous_object)
                .unwrap()
                .context_state()
                .unwrap()
                .state = Some(previous);
        }
        table.drop_binding(&mut env.sys.objects, current, &mut released);
        optional(env, out, previous_object)
    })();
    released.references = result.map_or_else(Error::hresult, |()| 0);
    retire(env, released)
}

pub(super) fn get_type(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(u32::from(current(env).is_some()))
}

pub(super) fn get_context_flags(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(current(env).map_or(0, |list| list.flags))
}

pub(in crate::d3d11::glue) fn list_flags(env: &mut Env<'_>) -> Outcome {
    let flags = table(env)
        .get(&env.sys.objects, env.arg(0))
        .and_then(Object::command_list)
        .map_or(0, |list| list.flags);
    Outcome::Return(flags)
}

pub(super) fn current<'a>(env: &'a Env<'_>) -> Option<&'a CommandList> {
    table(env)
        .get(&env.sys.objects, env.arg(0))?
        .context_data()?
        .deferred
        .as_ref()
}

pub(super) fn finish_list(env: &mut Env<'_>) -> Outcome {
    let (context, restore, out) = (env.arg(0), env.arg(1) != 0, env.arg(2));
    let table = table(env);
    let mut released = Release::default();
    let result = (|| {
        optional(env, out, 0u32)?;
        let Some(flags) = current(env).map(|list| list.flags) else {
            return Err(crate::dxgi::Error::InvalidCall.into());
        };
        #[cfg(feature = "gpu")]
        recording::finish(env)?;
        let pointer = if out == 0 {
            0
        } else {
            let device = owner(env)?;
            let pointer = alloc(env, "ID3D11CommandList")?;
            let empty = CommandList {
                flags,
                ..Default::default()
            };
            if let Err(error) =
                table.attach_command_list(&mut env.sys.objects, pointer, device, empty)
            {
                free(env, pointer);
                return Err(error.into());
            }
            pointer
        };
        let deferred = table
            .get_mut(&mut env.sys.objects, context)
            .unwrap()
            .context_data_mut()
            .unwrap()
            .deferred
            .as_mut()
            .unwrap();
        let list = std::mem::replace(
            deferred,
            CommandList {
                flags,
                ..Default::default()
            },
        );
        if pointer == 0 {
            table.destroy_list(&mut env.sys.objects, list, &mut released);
        } else {
            *table
                .get_mut(&mut env.sys.objects, pointer)
                .unwrap()
                .command_list_mut()
                .unwrap() = list;
            write(env, out, pointer)?;
        }
        if !restore {
            clear_bindings(env, &mut released)?;
        }
        #[cfg(feature = "gpu")]
        if restore {
            recording::retain_current_bindings(env)?;
        }
        Ok(())
    })();
    released.references = result.map_or_else(Error::hresult, |()| 0);
    retire(env, released)
}

pub(super) fn execute_list(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(1);
    if owned(env, pointer).and_then(Object::command_list).is_none() {
        return Outcome::Return(0);
    }
    #[cfg(feature = "gpu")]
    if let Err(error) = replay(env, pointer) {
        return Outcome::Return(error.hresult());
    }
    if env.arg(2) == 0 {
        clear(env)
    } else {
        Outcome::Return(0)
    }
}

#[cfg(feature = "gpu")]
mod recording;
#[cfg(feature = "gpu")]
pub(in crate::d3d11) use recording::Recording;
#[cfg(feature = "gpu")]
pub(super) use recording::{
    map_resource, record, record_counters, replay, retain_binding, unmap_resource,
};
