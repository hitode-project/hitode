//! The graph's IBasicAudio, IBasicVideo2 and IVideoWindow when no host
//! renderer answers them: like quartz, each call goes to the newest guest
//! member exposing the interface.

use super::*;

/// The member answering the interface, with a reference, or 0.
type Target = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

/// Forward `method` of `face` to the newest guest member exposing it. With
/// none, IBasicAudio answers E_NOTIMPL and the video interfaces E_NOINTERFACE.
pub(super) fn call(env: &mut Env<'_>, graph: u32, face: Face, method: &str) -> Outcome {
    let (iid, missing) = match face {
        Face::BasicAudio => (IID_IBASICAUDIO, E_NOTIMPL),
        Face::BasicVideo if method == "GetPreferredAspectRatio" => {
            (IID_IBASICVIDEO2, E_NOINTERFACE)
        }
        Face::BasicVideo => (IID_IBASICVIDEO, E_NOINTERFACE),
        _ => (IID_IVIDEOWINDOW, E_NOINTERFACE),
    };
    let Some((slot, argc)) = interface_methods(face.interface())
        .into_iter()
        .find(|&(name, _, _)| name == method)
        .map(|(_, conv, slot)| match conv {
            adv_machine::Conv::Stdcall { argc } | adv_machine::Conv::Cdecl { argc } => {
                (slot, u32::from(argc))
            }
            adv_machine::Conv::CdeclVariadic { fixed } => (slot, u32::from(fixed)),
        })
    else {
        return Outcome::Return(E_NOTIMPL);
    };
    let args: Vec<u32> = (1..argc).map(|i| env.arg(i)).collect();
    let members: VecDeque<u32> = g(env, graph)
        .members
        .iter()
        .rev()
        .filter(|m| m.host.is_none())
        .map(|m| m.filter)
        .collect();
    let Some(scratch) = alloc(env, 20) else {
        return Outcome::Return(E_OUTOFMEMORY);
    };
    if env.cpu.write(scratch, &iid).is_err() {
        free(env, scratch);
        return Outcome::Return(E_OUTOFMEMORY);
    }
    next(
        env,
        members,
        scratch,
        Box::new(move |env, target| {
            if target == 0 {
                return Outcome::Return(missing);
            }
            guest_call(env, target, slot.into(), args, move |env, hr| {
                guest_call(env, target, 2, vec![], move |_, _| Outcome::Return(hr))
            })
        }),
    )
}

/// The first of `members` answering the interface at `scratch`, with a
/// reference, or 0.
fn next(env: &mut Env<'_>, mut members: VecDeque<u32>, scratch: u32, then: Target) -> Outcome {
    let Some(member) = members.pop_front() else {
        free(env, scratch);
        return then(env, 0);
    };
    let _ = env.cpu.write_u32(scratch + 16, 0);
    guest_call(
        env,
        member,
        0,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let target = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            if hr == S_OK && target != 0 {
                free(env, scratch);
                return then(env, target);
            }
            next(env, members, scratch, then)
        },
    )
}
