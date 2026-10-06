//! Guest creation policy over the existing fixed-function device. The requested
//! VP mode is independent of the host adapter and never selects another renderer.
use super::*;
use adv_abi::types::D3DDEVICE_CREATION_PARAMETERS;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Software,
    Hardware,
    Mixed,
}
pub(super) struct State {
    parameters: D3DDEVICE_CREATION_PARAMETERS,
    mode: Mode,
    software: bool,
}
impl State {
    pub(super) fn new(adapter: u32, kind: i32, focus: u32, flags: u32) -> Result<Self, u32> {
        let mode = match flags & 0xe0 {
            0x20 => Mode::Software,
            0x40 => Mode::Hardware,
            0x80 => Mode::Mixed,
            _ => return Err(INVALID),
        };
        if flags & 0x10 != 0 && mode != Mode::Hardware {
            return Err(INVALID);
        }
        // PURE and other unimplemented creation policies must not silently
        // become an ordinary device. MULTITHREADED uses the existing M:1 owner.
        if flags & !(2 | 4 | 0xe0) != 0 {
            return Err(UNAVAILABLE);
        }
        Ok(Self {
            parameters: D3DDEVICE_CREATION_PARAMETERS {
                AdapterOrdinal: adapter,
                DeviceType: kind,
                hFocusWindow: focus,
                BehaviorFlags: flags,
            },
            mode,
            software: mode == Mode::Software,
        })
    }
    fn set_software(&mut self, software: bool) -> Result<(), u32> {
        if self.mode != Mode::Mixed && software != (self.mode == Mode::Software) {
            return Err(INVALID);
        }
        self.software = software;
        Ok(())
    }
    pub(super) fn reset(&mut self) {
        self.software = self.mode == Mode::Software;
    }
    pub(super) fn allows_buffer_usage(&self, usage: u32) -> bool {
        self.mode != Mode::Hardware || usage & 0x10 == 0
    }
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        parameters_ABI,
        set_software_ABI,
        get_software_ABI,
        set_vertex_ABI,
        get_vertex_ABI,
        set_pixel_ABI,
        get_pixel_ABI,
        set_dialogs_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(d3d9, name = "IDirect3DDevice9::SetDialogBoxMode", level = implement)]
fn set_dialogs(env: &mut Env<'_>, this: GuestPtr<()>, enable_dialogs: i32) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    if d.scene {
        return INVALID;
    }
    // Windowed devices ignore the mode. Fullscreen dialogs need a lockable,
    // single-sampled DISCARD back buffer in a GDI format (X8R8G8B8, X1R5G5B5
    // or R5G6B5). Dialogs are ordinary USER windows here, so nothing else changes.
    let p = &d.parameters;
    if p.Windowed == 0
        && enable_dialogs != 0
        && (p.Flags & 1 == 0
            || p.MultiSampleType != 0
            || p.SwapEffect != 1
            || !matches!(p.BackBufferFormat, 22 | 24 | 23))
    {
        return INVALID;
    }
    0
}
#[win32(d3d9, name = "IDirect3DDevice9::GetCreationParameters", level = implement)]
fn parameters(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    output: GuestPtr<D3DDEVICE_CREATION_PARAMETERS>,
) -> u32 {
    let Some(d) = device(env, this.addr()) else {
        return INVALID;
    };
    let value = d.creation.parameters;
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9, name = "IDirect3DDevice9::SetSoftwareVertexProcessing", level = implement)]
fn set_software(env: &mut Env<'_>, this: GuestPtr<()>, software: i32) -> u32 {
    let Some(id) = com(env).identity::<Payload>(&env.sys.objects, this.addr()) else {
        return INVALID;
    };
    let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, id) else {
        return INVALID;
    };
    d.creation
        .set_software(software != 0)
        .map_or_else(|error| error, |()| 0)
}
#[win32(d3d9, name = "IDirect3DDevice9::GetSoftwareVertexProcessing", level = implement)]
fn get_software(env: &mut Env<'_>, this: GuestPtr<()>) -> i32 {
    i32::from(device(env, this.addr()).is_some_and(|d| d.creation.software))
}
fn set_shader(env: &Env<'_>, this: u32, shader: u32) -> u32 {
    // The fixed-function state has no programmable shader. NULL selects that
    // known state; unsupported shader pointers are never accepted or dereferenced.
    if device(env, this).is_some() && shader == 0 {
        0
    } else {
        INVALID
    }
}
fn get_shader(env: &mut Env<'_>, this: u32, output: GuestPtr<u32>) -> u32 {
    if device(env, this).is_some() && output.write(env.cpu, 0).is_ok() {
        0
    } else {
        INVALID
    }
}
#[win32(d3d9, name = "IDirect3DDevice9::SetVertexShader", level = implement)]
fn set_vertex(env: &mut Env<'_>, this: GuestPtr<()>, shader: GuestPtr<()>) -> u32 {
    set_shader(env, this.addr(), shader.addr())
}
#[win32(d3d9, name = "IDirect3DDevice9::GetVertexShader", level = implement)]
fn get_vertex(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_shader(env, this.addr(), output)
}
#[win32(d3d9, name = "IDirect3DDevice9::SetPixelShader", level = implement)]
fn set_pixel(env: &mut Env<'_>, this: GuestPtr<()>, shader: GuestPtr<()>) -> u32 {
    set_shader(env, this.addr(), shader.addr())
}
#[win32(d3d9, name = "IDirect3DDevice9::GetPixelShader", level = implement)]
fn get_pixel(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<u32>) -> u32 {
    get_shader(env, this.addr(), output)
}

#[cfg(test)]
mod tests;
