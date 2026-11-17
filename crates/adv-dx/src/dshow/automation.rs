//! IDispatch for the automation interfaces, as strmbase answers it: through
//! the quartz type library, whose ITypeInfo resolves names and invokes calls.
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};
use adv_os::com::Iid;

use super::*;

const LIBID_QUARTZTYPELIB: Iid = quartz(0x56a8_68b0);
const LOCALE_SYSTEM_DEFAULT: u32 = 0x800;

type Described = Box<dyn FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome>;

/// The type library interface describing `face`, if it derives from IDispatch.
fn described(face: Face) -> Option<Iid> {
    Some(match face {
        Face::MediaControl => IID_IMEDIACONTROL,
        Face::MediaEvent => IID_IMEDIAEVENT,
        Face::MediaPosition => IID_IMEDIAPOSITION,
        Face::BasicAudio => IID_IBASICAUDIO,
        Face::BasicVideo => IID_IBASICVIDEO,
        Face::VideoWindow => IID_IVIDEOWINDOW,
        _ => return None,
    })
}

pub(crate) fn call(env: &mut Env<'_>, face: Face, method: &str, this: u32) -> Option<Outcome> {
    let iid = described(face)?;
    let args: Vec<u32> = (1..=8).map(|i| env.arg(i)).collect();
    let forward = |slot: u32, args: Vec<u32>| -> Described {
        Box::new(move |env, info| match info {
            Ok(info) => guest_call(env, info, slot, args, |_, hr| Outcome::Return(hr)),
            Err(hr) => Outcome::Return(hr),
        })
    };
    let then = match method {
        "GetTypeInfoCount" => {
            let written = env.cpu.write_u32(args[0], 1).is_ok();
            return Some(Outcome::Return(if written { S_OK } else { E_POINTER }));
        }
        "GetTypeInfo" => {
            let output = args[2];
            Box::new(
                move |env: &mut Env<'_>, info: Result<u32, u32>| match info {
                    Ok(info) => guest_call(env, info, 1, vec![], move |env, _| {
                        let _ = env.cpu.write_u32(output, info);
                        Outcome::Return(S_OK)
                    }),
                    Err(hr) => Outcome::Return(hr),
                },
            ) as Described
        }
        // (riid, names, count, lcid, ids) → ITypeInfo::GetIDsOfNames(names, count, ids)
        "GetIDsOfNames" => forward(10, vec![args[1], args[2], args[4]]),
        // (id, riid, lcid, flags, params, result, exception, argument)
        "Invoke" => forward(
            11,
            vec![this, args[0], args[3], args[4], args[5], args[6], args[7]],
        ),
        _ => return None,
    };
    Some(type_info(env, iid, then))
}

/// The cached ITypeInfo for `iid`, loading the type library on first use.
fn type_info(env: &mut Env<'_>, iid: Iid, then: Described) -> Outcome {
    if let Some(&info) = state(env).type_infos.get(&iid) {
        return then(env, Ok(info));
    }
    let Some(scratch) = alloc(env, 20) else {
        return then(env, Err(E_OUTOFMEMORY));
    };
    let library = state(env).type_library;
    if library == 0 {
        let entry = env
            .ext
            .try_get::<host::Host>()
            .and_then(|host| (host.export)(env, "oleaut32.dll", "LoadRegTypeLib"));
        let Some(entry) = entry.filter(|_| env.cpu.write(scratch, &LIBID_QUARTZTYPELIB).is_ok())
        else {
            free(env, scratch);
            return then(env, Err(E_NOTIMPL));
        };
        let args = vec![scratch, 1, 0, LOCALE_SYSTEM_DEFAULT, scratch + 16];
        return Outcome::call(entry, args, move |env, resume| {
            let library = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            match resume.value() {
                S_OK if library != 0 => {
                    state(env).type_library = library;
                    type_info(env, iid, then)
                }
                S_OK => then(env, Err(E_POINTER)),
                hr => then(env, Err(hr)),
            }
        });
    }
    if env.cpu.write(scratch, &iid).is_err() {
        free(env, scratch);
        return then(env, Err(E_OUTOFMEMORY));
    }
    guest_call(
        env,
        library,
        6,
        vec![scratch, scratch + 16],
        move |env, hr| {
            let info = env.cpu.read_u32(scratch + 16).unwrap_or(0);
            free(env, scratch);
            if hr != S_OK || info == 0 {
                return then(env, Err(if hr == S_OK { E_POINTER } else { hr }));
            }
            state(env).type_infos.insert(iid, info);
            then(env, Ok(info))
        },
    )
}
