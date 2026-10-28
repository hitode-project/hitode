//! The implicit swap chain shares the device's presentation and backbuffer state.
use super::*;
use adv_abi::types::D3DDISPLAYMODE;

const IID_SWAP_CHAIN: Iid = [
    0xf2, 0x50, 0x49, 0x79, 0xfc, 0xad, 0x8a, 0x45, 0x90, 0x5e, 0x10, 0xa1, 0x0b, 0x0b, 0x50, 0x3b,
];

pub(super) fn register(registry: &mut Registry) {
    for api in [
        get_ABI,
        additional_ABI,
        chain_query_ABI,
        chain_add_ref_ABI,
        chain_release_ABI,
        chain_present_ABI,
        back_buffer_ABI,
        display_ABI,
        parameters_ABI,
        raster_ABI,
        front_ABI,
        get_device_ABI,
    ] {
        registry.register(api);
    }
}
unknown!(
    chain_query,
    chain_add_ref,
    chain_release,
    "IDirect3DSwapChain9::QueryInterface",
    "IDirect3DSwapChain9::AddRef",
    "IDirect3DSwapChain9::Release"
);
fn owner(env: &Env<'_>, pointer: u32) -> Option<Id> {
    match com(env).get::<Payload>(&env.sys.objects, pointer)? {
        Payload::SwapChain(owner) => Some(*owner),
        _ => None,
    }
}
fn device_pointer(env: &Env<'_>, pointer: u32) -> Option<GuestPtr<()>> {
    com(env)
        .canonical(&env.sys.objects, owner(env, pointer)?)
        .map(GuestPtr::new)
}
#[win32(d3d9, name = "IDirect3DDevice9::GetSwapChain", level = implement)]
fn get(env: &mut Env<'_>, this: GuestPtr<()>, index: u32, output: GuestPtr<u32>) -> u32 {
    if output.write(env.cpu, 0).is_err() {
        return INVALID;
    }
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    if index != 0 {
        return INVALID;
    }
    let owner = com(env)
        .identity::<Payload>(&env.sys.objects, this.addr())
        .unwrap();
    let pointer = if let Some(chain) = d.swap_chain {
        let pointer = com(env).canonical(&env.sys.objects, chain).unwrap();
        if add_ref(env, pointer).is_none() {
            return OUT_OF_MEMORY;
        }
        pointer
    } else {
        if com(env)
            .add_ref_identity(&mut env.sys.objects, owner)
            .is_none()
        {
            return OUT_OF_MEMORY;
        }
        let Some((pointer, chain)) = object(
            env,
            "IDirect3DSwapChain9",
            IID_SWAP_CHAIN,
            Payload::SwapChain(owner),
        ) else {
            let _ = com(env).release_identity(&mut env.sys.objects, owner);
            return OUT_OF_MEMORY;
        };
        if !com(env).retain_internal(&mut env.sys.objects, chain) {
            release(env, pointer);
            return OUT_OF_MEMORY;
        }
        if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
            d.swap_chain = Some(chain);
        }
        pointer
    };
    if output.write(env.cpu, pointer).is_err() {
        release(env, pointer);
        return INVALID;
    }
    0
}
#[win32(d3d9, name = "IDirect3DDevice9::CreateAdditionalSwapChain", level = implement)]
fn additional(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    _parameters: GuestPtr<D3DPRESENT_PARAMETERS>,
    output: GuestPtr<u32>,
) -> u32 {
    if output.write(env.cpu, 0).is_err() || device(env, this.addr()).is_none() {
        return INVALID;
    }
    adv_abi::warn_unavailable(
        env,
        "IDirect3DDevice9::CreateAdditionalSwapChain",
        "additional swap chains are unsupported",
    );
    UNAVAILABLE
}
#[win32(d3d9, name = "IDirect3DSwapChain9::Present", level = implement)]
fn chain_present(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    source: GuestPtr<RECT>,
    destination: GuestPtr<RECT>,
    window: Handle,
    dirty: GuestPtr<()>,
    flags: u32,
) -> Outcome {
    let Some(device) = device_pointer(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    if flags != 0 {
        adv_abi::warn_unavailable(
            env,
            "IDirect3DSwapChain9::Present",
            "nonzero presentation flags are unsupported",
        );
        return Outcome::Return(UNAVAILABLE);
    }
    present(env, device, source, destination, window, dirty)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetBackBuffer", level = implement)]
fn back_buffer(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    index: u32,
    kind: i32,
    output: GuestPtr<u32>,
) -> Outcome {
    let Some(device) = device_pointer(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    resources::back_buffer(env, device, 0, index, kind, output)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetDisplayMode", level = implement)]
fn display(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DDISPLAYMODE>) -> u32 {
    let Some(device) = device_pointer(env, this.addr()) else {
        return INVALID;
    };
    capabilities::device_display(env, device, 0, output)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetPresentParameters", level = implement)]
fn parameters(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<D3DPRESENT_PARAMETERS>,
) -> u32 {
    let Some(device) = device_pointer(env, this.addr()) else {
        return INVALID;
    };
    let value = super::device(env, device.addr()).unwrap().parameters;
    output.write(env.cpu, value).map_or(INVALID, |()| 0)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetRasterStatus", level = implement)]
fn raster(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<[u32; 2]>) -> u32 {
    if owner(env, this.addr()).is_none() {
        return INVALID;
    }
    let provider = env.ext.get::<State>().provider;
    let (_, height, hz) = (provider.display)(env.ext);
    let phase = if let Some((since, period)) = (provider.vsync)(env.ext) {
        since.as_secs_f64() / period.as_secs_f64()
    } else {
        env.sys.clock.now_us() as f64 * f64::from(hz) / 1_000_000.0
    };
    // Model a scan with one blanking line after the visible display rows.
    let line = (phase.fract() * f64::from(height + 1)) as u32;
    let blank = line >= height;
    output
        .write(env.cpu, [u32::from(blank), if blank { 0 } else { line }])
        .map_or(INVALID, |()| 0)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetFrontBufferData", level = implement)]
fn front(env: &mut Env<'_>, this: GuestPtr<()>, destination: GuestPtr<()>) -> Outcome {
    let Some(device) = device_pointer(env, this.addr()) else {
        return Outcome::Return(INVALID);
    };
    readback::front(env, device, 0, destination)
}
#[win32(d3d9, name = "IDirect3DSwapChain9::GetDevice", level = implement)]
fn get_device(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    let Some(owner) = owner(env, this.addr()) else {
        return INVALID;
    };
    child_device(env, owner, output)
}
