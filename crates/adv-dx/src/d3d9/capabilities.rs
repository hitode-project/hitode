//! Queries describe the implemented guest device, never the physical adapter's
//! unrelated shader/texture capabilities. Keep unsupported paths discoverable.
use super::*;
use adv_abi::types::{D3DADAPTER_IDENTIFIER9, D3DCAPS9, D3DDISPLAYMODE};

pub(super) fn register(registry: &mut Registry) {
    for api in [
        identifier_ABI,
        display_ABI,
        mode_count_ABI,
        mode_ABI,
        adapter_monitor_ABI,
        check_type_ABI,
        check_format_ABI,
        check_multisample_ABI,
        check_depth_ABI,
        check_conversion_ABI,
        factory_caps_ABI,
        device_caps_ABI,
        device_display_ABI,
    ] {
        registry.register(api);
    }
}
fn factory(env: &Env<'_>, this: u32, adapter: u32) -> bool {
    adapter == 0
        && matches!(
            com(env).get::<Payload>(&env.sys.objects, this),
            Some(Payload::Factory)
        )
}
fn available(env: &Env<'_>) -> bool {
    env.ext
        .get::<State>()
        .gpu
        .as_ref()
        .is_some_and(|g| g.context().check().is_ok())
}
fn mode_value(env: &Env<'_>) -> D3DDISPLAYMODE {
    let (width, height, refresh) = (env.ext.get::<State>().provider.display)(env.ext);
    D3DDISPLAYMODE {
        Width: width,
        Height: height,
        RefreshRate: refresh,
        Format: 22,
    }
}
fn put<T: bytemuck::Pod>(env: &mut Env<'_>, output: GuestPtr<T>, value: T) -> u32 {
    if output.write(env.cpu, value).is_ok() {
        0
    } else {
        INVALID
    }
}
fn ascii<const N: usize>(bytes: &[u8]) -> [i8; N] {
    let mut result = [0; N];
    for (to, &from) in result.iter_mut().take(N - 1).zip(bytes) {
        *to = from as i8;
    }
    result
}
#[win32(d3d9,name="IDirect3D9::GetAdapterIdentifier",level=implement)]
fn identifier(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    flags: u32,
    output: GuestPtr<D3DADAPTER_IDENTIFIER9>,
) -> u32 {
    if !factory(env, this.addr(), adapter) {
        return INVALID;
    }
    if flags & !2 != 0 {
        return INVALID;
    }
    // A stable virtual adapter identity. No host driver credentials or invented
    // WHQL certification; this implementation is what the guest is querying.
    put(
        env,
        output,
        D3DADAPTER_IDENTIFIER9 {
            Driver: ascii(b"adv-d3d9"),
            Description: ascii(b"ADV virtual D3D9 adapter"),
            DeviceName: ascii(b"\\\\.\\DISPLAY1"),
            DriverVersion: 1,
            DeviceIdentifier: [0x66445641, 0x4e87461b, 0x219a38aa, 0x015eda1e],
            ..D3DADAPTER_IDENTIFIER9::default()
        },
    )
}
#[win32(d3d9,name="IDirect3D9::GetAdapterDisplayMode",level=implement)]
fn display(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    output: GuestPtr<D3DDISPLAYMODE>,
) -> u32 {
    if !factory(env, this.addr(), adapter) {
        return INVALID;
    }
    put(env, output, mode_value(env))
}
#[win32(d3d9,name="IDirect3D9::GetAdapterModeCount",level=implement)]
fn mode_count(env: &mut Env<'_>, this: GuestPtr<()>, adapter: u32, format: u32) -> u32 {
    u32::from(factory(env, this.addr(), adapter) && format == 22)
}
#[win32(d3d9,name="IDirect3D9::EnumAdapterModes",level=implement)]
fn mode(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    format: u32,
    index: u32,
    output: GuestPtr<D3DDISPLAYMODE>,
) -> u32 {
    if !factory(env, this.addr(), adapter) || format != 22 || index != 0 {
        return INVALID;
    }
    put(env, output, mode_value(env))
}
const HMONITOR_PRIMARY: u32 = 0x0001_0001;
#[win32(d3d9, name = "IDirect3D9::GetAdapterMonitor", level = implement)]
fn adapter_monitor(env: &mut Env<'_>, this: GuestPtr<()>, adapter: u32) -> adv_abi::Handle {
    if !factory(env, this.addr(), adapter) {
        return adv_abi::Handle(0);
    }
    adv_abi::Handle(HMONITOR_PRIMARY)
}
#[win32(d3d9,name="IDirect3DDevice9::GetDisplayMode",level=implement)]
fn device_display(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    swap: u32,
    output: GuestPtr<D3DDISPLAYMODE>,
) -> u32 {
    if device(env, this.addr()).is_none() || swap != 0 {
        return INVALID;
    }
    put(env, output, mode_value(env))
}
#[win32(d3d9,name="IDirect3D9::CheckDeviceType",level=implement)]
fn check_type(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    format: u32,
    back: u32,
    _windowed: i32,
) -> u32 {
    if !factory(env, this.addr(), adapter) || kind != 1 {
        return INVALID;
    }
    if available(env) && matches!(format, 21 | 22) && matches!(back, 0 | 21 | 22) {
        0
    } else {
        UNAVAILABLE
    }
}
#[win32(d3d9,name="IDirect3D9::CheckDeviceFormat",level=implement)]
fn check_format(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    format: u32,
    usage: u32,
    resource: i32,
    check: u32,
) -> u32 {
    if !factory(env, this.addr(), adapter) || kind != 1 || check == 0 {
        return INVALID;
    }
    // Color surface/texture: sampled, dynamic or manual render-target mips.
    // Autogen, sRGB, depth textures, cubes and volumes remain unadvertised.
    let supported = (matches!(check, 21 | 22)
        && match resource {
            1 => usage == 0 || usage == 1,
            3 => matches!(usage, 0 | 1 | 0x200),
            _ => false,
        })
        || (resource == 1 && usage == 2 && depth::format(check).is_some());
    if available(env) && format == 22 && supported {
        0
    } else {
        UNAVAILABLE
    }
}
#[win32(d3d9,name="IDirect3D9::CheckDeviceMultiSampleType",level=implement)]
fn check_multisample(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    format: u32,
    _windowed: i32,
    samples: i32,
    quality: GuestPtr<u32>,
) -> u32 {
    if !factory(env, this.addr(), adapter) || !(0..=16).contains(&samples) {
        return INVALID;
    }
    if kind != 1 {
        return 0x8876086b;
    } // D3DERR_INVALIDDEVICE
    if !available(env)
        || !(matches!(format, 21 | 22) || depth::format(format).is_some())
        || samples != 0
    {
        return UNAVAILABLE;
    }
    if quality.is_null() {
        0
    } else {
        put(env, quality, 1)
    }
}
#[win32(d3d9,name="IDirect3D9::CheckDepthStencilMatch",level=implement)]
fn check_depth(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    format: u32,
    target: u32,
    depth: u32,
) -> u32 {
    if !factory(env, this.addr(), adapter) || kind != 1 {
        INVALID
    } else if available(env)
        && format == 22
        && matches!(target, 21 | 22)
        && depth::format(depth).is_some()
    {
        0
    } else {
        UNAVAILABLE
    }
}
#[win32(d3d9,name="IDirect3D9::CheckDeviceFormatConversion",level=implement)]
fn check_conversion(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    source: u32,
    target: u32,
) -> u32 {
    if !factory(env, this.addr(), adapter) || kind != 1 {
        return INVALID;
    }
    if available(env) && matches!(source, 21 | 22) && target == 22 {
        0
    } else {
        UNAVAILABLE
    }
}
fn caps(env: &Env<'_>) -> Result<D3DCAPS9, u32> {
    let g = env.ext.get::<State>().gpu.as_ref().ok_or(UNAVAILABLE)?;
    g.context().check().map_err(|_| DEVICE_LOST)?;
    // Full-square textures must also fit the 16 MiB upload limit. Drawing uses
    // 40-byte neutral vertices; 131072 triangles fit that same bounded upload.
    let texture_size = g
        .context()
        .device
        .limits()
        .max_texture_dimension_2d
        .min(2048);
    Ok(D3DCAPS9 {
        DeviceType: 1,
        NumberOfAdaptersInGroup: 1,
        Caps2: 0x20000000,                 // dynamic textures
        PresentationIntervals: 0x80000001, // IMMEDIATE and ONE (DEFAULT also paces).
        DevCaps: 0x40 | 0x200 | 0x400 | 0x800,
        PrimitiveMiscCaps: 0x10 | 0x20 | 0x40 | 0x80 | 0x800 | 0x20000,
        RasterCaps: 0x01000010,
        ZCmpCaps: 0xff,
        StencilCaps: 0x1ff,
        AlphaCmpCaps: 0xff,
        SrcBlendCaps: 0x7ff,
        DestBlendCaps: 0x7ff,
        ShadeCaps: 0x8 | 0x4000,
        TextureCaps: 1 | 4 | 0x40 | 0x4000,
        TextureFilterCaps: 0x03000300 | 0x00030000,
        TextureAddressCaps: 1 | 2 | 4 | 0x10,
        LineCaps: 1 | 4 | 8,
        MaxTextureWidth: texture_size,
        MaxTextureHeight: texture_size,
        MaxTextureRepeat: 32768,
        MaxTextureAspectRatio: texture_size,
        MaxAnisotropy: 1,
        MaxVertexW: 1.0,
        FVFCaps: 1,
        // Do not advertise general modulation/add/subtract until all argument
        // combinations work. Selected textured-quad combinations already draw.
        TextureOpCaps: 1 | 2 | 4,
        MaxTextureBlendStages: 1,
        MaxSimultaneousTextures: 1,
        MaxPointSize: 1.0,
        MaxPrimitiveCount: 131072,
        MaxStreams: 16,
        MaxVertexIndex: 0x000f_ffff,
        MaxStreamStride: 2048,
        DevCaps2: 1, // STREAMOFFSET: SetStreamSource offsets are used by drawing.
        NumSimultaneousRTs: 1,
        ..D3DCAPS9::default()
    })
}
#[win32(d3d9,name="IDirect3D9::GetDeviceCaps",level=implement)]
fn factory_caps(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    adapter: u32,
    kind: i32,
    output: GuestPtr<D3DCAPS9>,
) -> u32 {
    if !factory(env, this.addr(), adapter) || kind != 1 {
        return INVALID;
    }
    match caps(env) {
        Ok(c) => put(env, output, c),
        Err(e) => e,
    }
}
#[win32(d3d9,name="IDirect3DDevice9::GetDeviceCaps",level=implement)]
fn device_caps(env: &mut Env<'_>, this: GuestPtr<()>, output: GuestPtr<D3DCAPS9>) -> u32 {
    if device(env, this.addr()).is_none() {
        return INVALID;
    }
    match caps(env) {
        Ok(c) => put(env, output, c),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_backend_and_invalid_outputs_do_not_report_a_working_renderer() {
        let mut m = super::super::tests::machine();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
        let this = GuestPtr::new(pointer);
        let address = alloc(&mut env, 4096).unwrap();
        let output = GuestPtr::<D3DCAPS9>::new(address);
        assert_eq!(factory_caps(&mut env, this, 0, 1, output), UNAVAILABLE);
        assert_eq!(check_type(&mut env, this, 0, 1, 22, 21, 1), UNAVAILABLE);
        assert_eq!(
            check_format(&mut env, this, 0, 1, 22, 0, 3, 21),
            UNAVAILABLE
        );
        assert_eq!(identifier(&mut env, this, 0, 0, GuestPtr::new(0)), INVALID);
        // Failed argument validation must not overwrite caller storage.
        env.cpu.write(address, &[0xa5; 1100]).unwrap();
        assert_eq!(
            identifier(&mut env, this, 1, 0, GuestPtr::new(address)),
            INVALID
        );
        let mut bytes = [0; 1100];
        env.cpu.read(address, &mut bytes).unwrap();
        assert_eq!(bytes, [0xa5; 1100]);
    }
    #[test]
    fn capabilities_exclude_unimplemented_paths_and_bound_the_usable_upload() {
        let mut m = super::super::tests::machine();
        let gpu = pollster::block_on(Gpu::request(true)).expect("capability GPU fixture adapter");
        set_gpu(&mut m.ext, gpu).unwrap();
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let (pointer, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
        let this = GuestPtr::new(pointer);
        let value = caps(&env).unwrap();
        assert_eq!(
            value.DevCaps & (0x10000 | 0x100000),
            0,
            "no hardware VP or pure device"
        );
        assert_eq!(
            (value.VertexShaderVersion, value.PixelShaderVersion),
            (0, 0)
        );
        assert_eq!(
            (value.CubeTextureFilterCaps, value.VolumeTextureFilterCaps,),
            (0, 0)
        );
        assert_eq!(value.StencilCaps, 0x1ff);
        assert_eq!(value.ZCmpCaps, 0xff);
        assert_eq!(value.TextureCaps & 0x4000, 0x4000, "manual mip chains");
        assert_eq!(
            value.TextureFilterCaps & 0x30000,
            0x30000,
            "point and linear mip filters"
        );
        assert_eq!(value.TextureCaps & (0x800 | 0x2000), 0);
        assert_eq!((value.MaxStreams, value.MaxVertexIndex), (16, 0x000f_ffff));
        assert_eq!(value.DevCaps2, 1, "only stream offsets are implemented");
        assert!(
            u64::from(value.MaxTextureWidth) * u64::from(value.MaxTextureHeight) * 4
                <= 16 * 1024 * 1024
        );
        assert!(u64::from(value.MaxPrimitiveCount) * 3 * 40 <= 16 * 1024 * 1024);
        for (usage, kind, format) in [
            (0, 3, 21),
            (0x200, 3, 22),
            (0, 1, 21),
            (1, 3, 21),
            (1, 1, 22),
            (2, 1, 75),
            (2, 1, 77),
            (2, 1, 80),
        ] {
            assert_eq!(
                check_format(&mut env, this, 0, 1, 22, usage, kind, format),
                0
            );
        }
        for (usage, kind, format) in [
            (2, 3, 75),
            (2, 1, 70),
            (0x400, 3, 21),
            (0, 4, 21),
            (0, 5, 21),
        ] {
            assert_eq!(
                check_format(&mut env, this, 0, 1, 22, usage, kind, format),
                UNAVAILABLE
            );
        }
        assert_eq!(check_depth(&mut env, this, 0, 1, 22, 21, 75), 0);
        assert_eq!(check_type(&mut env, this, 0, 1, 22, 21, 0), 0);
        assert_eq!(check_type(&mut env, this, 0, 1, 99, 21, 0), UNAVAILABLE);
        assert_eq!(
            check_multisample(&mut env, this, 0, 1, 21, 1, 2, GuestPtr::new(0)),
            UNAVAILABLE
        );
        assert_eq!(adapter_monitor(&mut env, this, 0), adv_abi::Handle(HMONITOR_PRIMARY));
        assert_eq!(adapter_monitor(&mut env, this, 1), adv_abi::Handle(0));
        assert_eq!(adapter_monitor(&mut env, GuestPtr::new(0), 0), adv_abi::Handle(0));
        let at = alloc(&mut env, 4096).unwrap();
        assert_eq!(factory_caps(&mut env, this, 0, 1, GuestPtr::new(at)), 0);
        assert_eq!(GuestPtr::<D3DCAPS9>::new(at).read(env.cpu).unwrap(), value);
        assert_eq!(
            factory_caps(&mut env, this, 0, 1, GuestPtr::new(0)),
            INVALID
        );
    }
}
