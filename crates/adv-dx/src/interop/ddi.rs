//! Legacy Direct3D runtime driver exports shared by the graphics libraries.
use adv_abi::{Api, Conv, Env, Handler, ImplementationLevel, Outcome, Registry};
use adv_cpu::backend::{Fault, FaultKind, GuestExt, Reg};

const INVALID_PARAMETER: u32 = 0xc000000d;
const UNSUCCESSFUL: u32 = 0xc0000001;
const E_INVALIDARG: u32 = 0x80070057;

const ADAPTER_CALLBACKS: [(&str, u8, Handler); 5] = [
    ("OpenAdapter10::CalcPrivateDeviceSize", 2, |_| {
        Outcome::Return(4)
    }),
    ("OpenAdapter10::CreateDevice", 2, create_device),
    ("OpenAdapter10::CloseAdapter", 1, success),
    ("OpenAdapter10::GetSupportedVersions", 3, versions),
    ("OpenAdapter10::GetCaps", 2, caps),
];

pub(crate) fn register(registry: &mut Registry, module: &'static str) {
    for name in [
        "D3DKMTCloseAdapter",
        "D3DKMTCreateAllocation",
        "D3DKMTCreateContext",
        "D3DKMTCreateDevice",
        "D3DKMTCreateSynchronizationObject",
        "D3DKMTDestroyAllocation",
        "D3DKMTDestroyContext",
        "D3DKMTDestroyDevice",
        "D3DKMTDestroySynchronizationObject",
        "D3DKMTGetContextSchedulingPriority",
        "D3DKMTGetDisplayModeList",
        "D3DKMTGetMultisampleMethodList",
        "D3DKMTGetRuntimeData",
        "D3DKMTGetSharedPrimaryHandle",
        "D3DKMTLock",
        "D3DKMTPresent",
        "D3DKMTQueryAllocationResidency",
        "D3DKMTRender",
        "D3DKMTSetAllocationPriority",
        "D3DKMTSetContextSchedulingPriority",
        "D3DKMTSetDisplayMode",
        "D3DKMTSetDisplayPrivateDriverFormat",
        "D3DKMTSetGammaRamp",
        "D3DKMTSetVidPnSourceOwner",
        "D3DKMTSignalSynchronizationObject",
        "D3DKMTUnlock",
        "D3DKMTWaitForSynchronizationObject",
        "D3DKMTWaitForVerticalBlankEvent",
    ] {
        register_proc(registry, module, name, 1, success);
    }
    for (name, handler) in [
        ("D3DKMTOpenResource", invalid as Handler),
        ("D3DKMTQueryResourceInfo", invalid),
        ("D3DKMTEscape", escape),
        ("D3DKMTGetDeviceState", device_state),
        ("D3DKMTOpenAdapterFromHdc", adapter_from_hdc),
        ("D3DKMTQueryAdapterInfo", adapter_info),
        ("OpenAdapter10", open_adapter),
        ("OpenAdapter10_2", open_adapter2),
        ("OpenAdapter10::DeviceMethod", access_violation),
    ] {
        register_proc(registry, module, name, 1, handler);
    }
    for (name, argc, handler) in ADAPTER_CALLBACKS {
        register_proc(registry, module, name, argc, handler);
    }
}

pub(crate) fn register_proc(
    registry: &mut Registry,
    module: &'static str,
    name: &'static str,
    argc: u8,
    handler: Handler,
) {
    registry.register(Api {
        module,
        name,
        handler,
        level: ImplementationLevel::Implement,
        conv: Conv::Stdcall { argc },
    });
}

fn success(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

fn invalid(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(INVALID_PARAMETER)
}

fn escape(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(0);
    if env.cpu.read_u32(args + 8) == Ok(0x15)
        && let Ok(data) = env.cpu.read_u32(args + 16)
    {
        let _ = env.cpu.write(data, &[1]);
    }
    success(env)
}

fn device_state(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(0);
    let result = match env.cpu.read_u32(args + 4) {
        Ok(1) => env.cpu.write_u32(args + 8, 1),
        Ok(2) => env.cpu.write(args + 8, &[0; 40]),
        _ => return Outcome::Return(UNSUCCESSFUL),
    };
    Outcome::Return(result.map_or(INVALID_PARAMETER, |()| 0))
}

#[derive(Default)]
struct Luid(u64);

fn adapter_from_hdc(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(0);
    if env.ext.try_get::<Luid>().is_none() {
        env.ext.insert(Luid::default());
    }
    let luid = &mut env.ext.get_mut::<Luid>().0;
    *luid += 1;
    let data = [0xf0fu32, *luid as u32, (*luid >> 32) as u32];
    Outcome::Return(
        env.cpu
            .write(args + 4, bytemuck::bytes_of(&data))
            .map_or(INVALID_PARAMETER, |()| 0),
    )
}

fn adapter_info(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(0);
    let Ok(kind) = env.cpu.read_u32(args + 4) else {
        return invalid(env);
    };
    let result = match kind {
        1 => {
            let Ok(data) = env.cpu.read_u32(args + 8) else {
                return invalid(env);
            };
            if env.cpu.read_u32(data) == Ok(0) {
                return Outcome::Return(UNSUCCESSFUL);
            }
            let module = &env.sys.thunks.get(env.thunk).module;
            let path: Vec<_> = format!(r"C:\windows\system32\{module}")
                .encode_utf16()
                .chain([0])
                .collect();
            let capacity = env.cpu.read_u32(args + 12).unwrap_or(0) as usize / 2;
            env.cpu.write(
                data,
                bytemuck::cast_slice(&path[..path.len().min(capacity)]),
            )
        }
        0xb => env
            .cpu
            .read_u32(args + 8)
            .and_then(|data| env.cpu.write_u32(data, 0)),
        0x18 => {
            if env.cpu.read_u32(args + 12) != Ok(4) {
                return invalid(env);
            }
            env.cpu.read_u32(args + 8).and_then(|data| {
                let value = env.cpu.read_u32(data)?;
                env.cpu.write_u32(data, (value & !2) | 5)
            })
        }
        _ => return success(env),
    };
    Outcome::Return(result.map_or(INVALID_PARAMETER, |()| 0))
}

fn open_adapter(env: &mut Env<'_>) -> Outcome {
    adapter(env, 3)
}

fn open_adapter2(env: &mut Env<'_>) -> Outcome {
    adapter(env, 5)
}

fn adapter(env: &mut Env<'_>, slots: usize) -> Outcome {
    let args = env.arg(0);
    let module = env.sys.thunks.get(env.thunk).module.clone();
    let result = (|| {
        let table = env.cpu.read_u32(args + 20)?;
        for (index, &(name, _, _)) in ADAPTER_CALLBACKS[..slots].iter().enumerate() {
            let callback = env.sys.thunks.publish(env.cpu, &module, name)?;
            env.cpu.write_u32(table + index as u32 * 4, callback)?;
        }
        env.cpu.write_u32(args + 4, 0xf07)
    })();
    Outcome::Return(result.map_or(E_INVALIDARG, |()| 0))
}

fn access_violation(env: &mut Env<'_>) -> Outcome {
    Outcome::Raise(Fault {
        kind: FaultKind::Read(env.arg(0).wrapping_sub(0x6b08)),
        pc: env.cpu.reg(Reg::Eip),
    })
}

fn create_device(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(1);
    let slots = match env.cpu.read_u32(args + 4) {
        Ok(0x000a0001 | 0x000a0003 | 0x000a0006 | 0x000a0009) => 101,
        Ok(0x000a0002 | 0x000a0004 | 0x000a0007 | 0x000a000a) => 103,
        Ok(0x000b0009..=0x000b000b) => 150,
        Ok(0x000b000e | 0x000b000f) => 155,
        Ok(0x000b0010) => 164,
        Ok(0x000b0011 | 0x000b0020) => 176,
        _ => return Outcome::Return(E_INVALIDARG),
    };
    let module = env.sys.thunks.get(env.thunk).module.clone();
    let result = (|| {
        let table = env.cpu.read_u32(args + 16)?;
        let callback = env
            .sys
            .thunks
            .publish(env.cpu, &module, "OpenAdapter10::DeviceMethod")?;
        env.cpu
            .write(table, bytemuck::cast_slice(&[callback; 176][..slots]))
    })();
    Outcome::Return(result.map_or(E_INVALIDARG, |()| 0))
}

fn versions(env: &mut Env<'_>) -> Outcome {
    // D3D10UMDDI supported-version constants. The Windows order awaits a value capture.
    const VERSIONS: [u64; 9] = [
        0x000a0001_00040000,
        0x000a0002_00010000,
        0x000b000a_00020000,
        0x000a0006_00000000,
        0x000a0007_00000000,
        0x000a0009_00000000,
        0x000a000a_00000000,
        0x000b000b_00020000,
        0x000b000f_00000000,
    ];
    let (entries, output) = (env.arg(1), env.arg(2));
    let result: Result<u32, adv_cpu::backend::MemFault> = (|| {
        let count = if output == 0 {
            0
        } else {
            env.cpu.read_u32(entries)?
        };
        env.cpu.write_u32(entries, VERSIONS.len() as u32)?;
        if output != 0 {
            if count < VERSIONS.len() as u32 {
                return Ok(0x8007007a);
            }
            env.cpu.write(output, bytemuck::bytes_of(&VERSIONS))?;
        }
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or(E_INVALIDARG))
}

fn caps(env: &mut Env<'_>) -> Outcome {
    let args = env.arg(1);
    // Published DDI structures and flags. The Windows values await capture.
    let data: &[u32] = match env.cpu.read_u32(args) {
        Ok(128) => &[5],
        Ok(129) => &[2],
        Ok(130) => &[0x77],
        Ok(131 | 134) => &[0, 0],
        Ok(132 | 137) => &[0],
        _ => return Outcome::Return(0x80004001),
    };
    let result: Result<u32, adv_cpu::backend::MemFault> = (|| {
        let output = env.cpu.read_u32(args + 8)?;
        let size = env.cpu.read_u32(args + 12)?;
        if size != size_of_val(data) as u32 {
            return Ok(E_INVALIDARG);
        }
        env.cpu.write(output, bytemuck::cast_slice(data))?;
        Ok(0)
    })();
    Outcome::Return(result.unwrap_or(E_INVALIDARG))
}
