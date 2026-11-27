use super::*;

const TEXELS: u32 = 0x430000;
const SAMPLE_LEVEL: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_sample_level.dxbc");
const TEXEL_LOAD: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_texel_load.dxbc");
const SAMPLE_GRAD: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_sample_grad.dxbc");

struct SampleOutput {
    target: u32,
    view: u32,
    staging: u32,
}

impl SampleOutput {
    fn new(r: &mut Raster) -> Self {
        let (target, view, staging) = r.target(64, 2);
        r.machine.cpu.map(TEXELS, 65536, Prot::RW).unwrap();
        Self {
            target,
            view,
            staging,
        }
    }

    fn draw(&self, r: &mut Raster, parameters: [f32; 4], count: usize) -> Vec<[u32; 4]> {
        r.draw(0..64, parameters);
        self.read(r, count)
    }

    fn read(&self, r: &mut Raster, count: usize) -> Vec<[u32; 4]> {
        r.call(47, &[self.staging, self.target]);
        let map = map(&mut r.machine, r.context, self.staging, 1);
        let texels = (0..count as u32)
            .map(|x| {
                GuestPtr::new(map.pData + x * 16)
                    .read(&r.machine.cpu)
                    .unwrap()
            })
            .collect();
        r.call(15, &[self.staging, 0]);
        texels
    }

    fn check(&self, r: &mut Raster, name: &str, parameters: [f32; 4], tolerance: f32) {
        let measured = |driver| {
            let name = format!("{driver}:{name}");
            include_str!("../../sampling.tsv")
                .lines()
                .find_map(|line| {
                    let (title, values) = line.split_once('\t')?;
                    (title == name).then_some(values)
                })
                .unwrap()
                .split('\t')
                .map(|sample| {
                    sample
                        .split(',')
                        .map(|f| u32::from_str_radix(f, 16).unwrap())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let mut hardware = measured("hw");
        let mut warp = measured("warp");
        if name.starts_with("stencil-") {
            for word in hardware.iter_mut().chain(&mut warp).flatten() {
                *word = (*word as f32).to_bits();
            }
        }
        let pixels = self.draw(r, parameters, warp.len());
        for (x, (actual, warp)) in pixels.iter().zip(&warp).enumerate() {
            for (channel, (&value, &reference)) in actual.iter().zip(warp).enumerate() {
                let bc_color = channel < 3
                    && (name.starts_with("bc3-")
                        || name == "bc1-3color-blue-red-transparent" && x % 4 == 2);
                let tolerance = if bc_color { 1. / 255. } else { tolerance };
                let difference = (f32::from_bits(value) - f32::from_bits(reference)).abs();
                let bc_code = bc_color
                    && ((f32::from_bits(value) * 255.).round()
                        - (f32::from_bits(reference) * 255.).round())
                    .abs()
                        <= 1.;
                assert!(
                    value == reference || difference <= tolerance || bc_code,
                    "{name}[{x}][{channel}] is {value:08x}, hardware {:08x}, WARP {reference:08x}",
                    hardware[x][channel]
                );
            }
        }
    }
}

fn sampled(r: &mut Raster, format: i32, size: [u32; 2], levels: &[Vec<u32>], misc: u32) -> u32 {
    let [width, height] = size;
    let mut at = TEXELS;
    let init: Vec<_> = levels
        .iter()
        .enumerate()
        .map(|(mip, texels)| {
            let bytes = bytemuck::cast_slice(texels);
            r.machine.cpu.write(at, bytes).unwrap();
            let subresource = D3D11_SUBRESOURCE_DATA {
                pSysMem: at,
                SysMemPitch: (width >> mip).max(1) * 4,
                SysMemSlicePitch: bytes.len() as u32,
            };
            at += bytes.len() as u32;
            subresource
        })
        .collect();
    r.machine
        .cpu
        .write(INIT, bytemuck::cast_slice(&init))
        .unwrap();
    let image = r.image(
        D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: levels.len() as u32,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: Bind::SHADER,
            MiscFlags: misc,
            ..Default::default()
        },
        INIT,
    );
    let view = create_object(&mut r.machine, r.device, 7, &[image, 0]);
    write_value(&mut r.machine, ARRAY, view);
    r.call(8, &[0, 1, ARRAY]);
    image
}

fn sampler(r: &mut Raster, filter: i32, address: i32, [bias, min, max]: [f32; 3]) {
    let state = r.object(
        23,
        D3D11_SAMPLER_DESC {
            Filter: filter,
            AddressU: address,
            AddressV: address,
            AddressW: address,
            ComparisonFunc: 1,
            MaxAnisotropy: if filter == 0x55 { 16 } else { 0 },
            MipLODBias: bias,
            MinLOD: min,
            MaxLOD: max,
            BorderColor: [1., 0.25, 0., 1.],
        },
    );
    write_value(&mut r.machine, ARRAY, state);
    r.call(10, &[0, 1, ARRAY]);
}

#[test]
fn windows_sampler_addressing_and_srgb_through_guest_vtables() {
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    sampled(
        &mut r,
        28,
        [4, 1],
        &[vec![0xff000010, 0xff000050, 0xff000090, 0xff0000d0]],
        0,
    );
    r.shader(15, 9, SAMPLE_LEVEL);
    for (name, address) in [
        ("address-wrap-u=(i-32+0.5)/16", 1),
        ("address-mirror", 2),
        ("address-clamp", 3),
        ("address-border-1,0.25,0,1", 4),
        ("address-mirror-once", 5),
    ] {
        sampler(&mut r, 0, address, [0., 0., 1000.]);
        output.check(&mut r, name, [-31.5, 1. / 16., 0., 0.], 0.);
    }
    sampler(&mut r, 0x15, 4, [0., 0., 1000.]);
    output.check(
        &mut r,
        "address-border-linear-edge-u=(i-8)/64",
        [-8., 1. / 64., 0., 0.],
        1. / 255.,
    );

    sampled(
        &mut r,
        29,
        [256, 1],
        &[(0..256).map(|i| 0xff000000 | i).collect()],
        0,
    );
    r.shader(15, 9, TEXEL_LOAD);
    for (lane, name) in [
        "srgb-decode-texel-4i",
        "srgb-decode-texel-4i+1",
        "srgb-decode-texel-4i+2",
        "srgb-decode-texel-4i+3",
    ]
    .into_iter()
    .enumerate()
    {
        output.check(&mut r, name, [4., lane as f32, 0., 0.], 0.43 / 255.);
    }
    sampled(&mut r, 29, [2, 1], &[vec![0xff000000, 0xffffffff]], 0);
    r.shader(15, 9, SAMPLE_LEVEL);
    sampler(&mut r, 0x15, 3, [0., 0., 1000.]);
    output.check(
        &mut r,
        "srgb-filter-black-white-u=(0.5+i/64)/2",
        [32., 1. / 128., 0., 0.],
        1. / 255.,
    );
}

// spec: GFX-REC-7
#[test]
fn dynamic_texture_sampling_sees_gpu_copies_before_mapping() {
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    let source = sampled(&mut r, 28, [2, 2], &[vec![0xff00_00ff; 4]], 0);
    let dynamic = texture(&mut r.machine, r.device, 2, Bind::SHADER, 0x10000, 0, 2);
    let view = create_object(&mut r.machine, r.device, 7, &[dynamic, 0]);
    write_value(&mut r.machine, ARRAY, view);
    r.call(8, &[0, 1, ARRAY]);
    r.shader(15, 9, TEXEL_LOAD);
    r.call(47, &[dynamic, source]);
    let pixels = output.draw(&mut r, [0., 0., 0., 0.], 1);
    assert_eq!(pixels, [[1.0f32, 0., 0., 1.].map(f32::to_bits)]);

    let write_texel = |r: &mut Raster, value| {
        let mapped = map(&mut r.machine, r.context, dynamic, 4);
        write_value(&mut r.machine, mapped.pData, value);
        r.call(15, &[dynamic, 0]);
    };
    write_texel(&mut r, 0xff00_ff00u32);
    r.draw(0..1, [0.; 4]);
    #[cfg(feature = "d3d9-counters")]
    let upload = gpu::traffic(&r.machine.ext).unwrap().upload_bytes;
    #[cfg(feature = "d3d9-counters")]
    let views = gpu::capture_counters(&r.machine.ext)
        .unwrap()
        .image_views_created;
    r.draw(1..2, [0.; 4]);
    #[cfg(feature = "d3d9-counters")]
    assert_eq!(
        gpu::traffic(&r.machine.ext).unwrap().upload_bytes - upload,
        64
    );

    write_texel(&mut r, 0xffff_0000u32);
    r.draw(2..3, [0.; 4]);
    assert_eq!(
        output.read(&mut r, 3),
        [[0.0f32, 1., 0., 1.], [0., 1., 0., 1.], [0., 0., 1., 1.]]
            .map(|rgba| rgba.map(f32::to_bits)),
    );
    #[cfg(feature = "d3d9-counters")]
    assert_eq!(
        gpu::capture_counters(&r.machine.ext)
            .unwrap()
            .image_views_created,
        views
    );
}

#[test]
fn windows_sampler_lod_and_resource_clamp_through_guest_vtables() {
    // D3D11.3 §7.18.16.2 allows filtering at the format's precision.
    const LINEAR_UNORM8_TOLERANCE: f32 = 1. / 255.;
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    let levels: Vec<_> = (0..7)
        .map(|mip| vec![0xff000000 | (mip * 32 + 16); (64usize >> mip).pow(2)])
        .collect();
    let resource = sampled(&mut r, 28, [64, 64], &levels, Misc::RESOURCE_CLAMP);
    r.shader(15, 9, SAMPLE_GRAD);
    for (name, filter, lod, y, tolerance) in [
        ("lod-mip-point-lambda=i/8", 0, [0., 0., 1000.], 0., 0.),
        (
            "lod-mip-linear-lambda=i/8",
            1,
            [0., 0., 1000.],
            0.,
            LINEAR_UNORM8_TOLERANCE,
        ),
        ("lod-mip-point-bias1", 0, [1., 0., 1000.], 0., 0.),
        (
            "lod-mip-linear-minlod1.5-maxlod3",
            1,
            [0., 1.5, 3.],
            0.,
            LINEAR_UNORM8_TOLERANCE,
        ),
        ("lod-mip-point-bias-0.25", 0, [-0.25, 0., 1000.], 0., 0.),
        (
            "lod-anisotropic-gradient-x=2^(i/8)/64-y=1/64",
            1,
            [0., 0., 1000.],
            1. / 64.,
            LINEAR_UNORM8_TOLERANCE,
        ),
    ] {
        sampler(&mut r, filter, 3, lod);
        output.check(&mut r, name, [0.5, 0.5, y, 0.], tolerance);
    }

    r.shader(15, 9, SAMPLE_LEVEL);
    sampler(&mut r, 1, 3, [0., 0., 1000.]);
    r.call(55, &[resource, 1.5f32.to_bits()]);
    let sample = output.draw(&mut r, [0.5, 1., 0., 0.], 1)[0];
    assert!((f32::from_bits(sample[0]) - 64. / 255.).abs() <= 0.5 / 255.);
    r.shader(15, 9, TEXEL_LOAD);
    assert_eq!(output.draw(&mut r, [0., 0., 0., 0.], 1)[0], [0; 4]);
    let available = output.draw(&mut r, [0., 0., 0., 1.], 1)[0];
    assert_eq!(available[0], (48f32 / 255.).to_bits());
    r.call(55, &[resource, 6.25f32.to_bits()]);
    assert_eq!(output.draw(&mut r, [0., 0., 0., 6.], 1)[0], [0; 4]);
}

const DEPTH_WRITE: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_depth_write.dxbc");
const DEPTH_VERTEX: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_depth_vertex.dxbc");
const STENCIL_LOAD: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_stencil_load.dxbc");
const FULLSCREEN: &[u8] =
    include_bytes!("../../../../../../tests/fixtures/d3d11/guest_fullscreen.dxbc");

fn depth_views(
    r: &mut Raster,
    resource: i32,
    depth: i32,
    sampled_depth: i32,
    stencil: i32,
) -> (u32, u32, u32, Option<u32>) {
    let texture = r.texture(64, 1, resource, Bind::SHADER | Bind::DEPTH_STENCIL, 0);
    let dsv = adv_abi::types::D3D11_DEPTH_STENCIL_VIEW_DESC {
        Format: depth,
        ViewDimension: 3,
        Flags: 0,
        Anonymous: [0; 12],
    };
    write_value(&mut r.machine, DESCRIPTOR, dsv);
    let dsv = create_object(&mut r.machine, r.device, 10, &[texture, DESCRIPTOR]);
    let mut srv = adv_abi::types::D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: sampled_depth,
        ViewDimension: 4,
        Anonymous: bytemuck::cast([0u32, 1, 0, 0]),
    };
    write_value(&mut r.machine, DESCRIPTOR, srv);
    let sampled = create_object(&mut r.machine, r.device, 7, &[texture, DESCRIPTOR]);
    let stencil_view = (stencil != 0).then(|| {
        srv.Format = stencil;
        write_value(&mut r.machine, DESCRIPTOR, srv);
        create_object(&mut r.machine, r.device, 7, &[texture, DESCRIPTOR])
    });
    (texture, dsv, sampled, stencil_view)
}

fn bind_depth(r: &mut Raster, dsv: u32, state: u32, reference: u32, [min, max]: [f32; 2]) {
    r.call(33, &[0, 0, dsv]);
    r.call(36, &[state, reference]);
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_VIEWPORT {
            Width: 64.,
            Height: 1.,
            MinDepth: min,
            MaxDepth: max,
            ..Default::default()
        },
    );
    r.call(44, &[1, DESCRIPTOR]);
}

fn clear_depth(r: &mut Raster, dsv: u32, flags: u32, depth: f32, stencil: u32) {
    r.call(53, &[dsv, flags, depth.to_bits(), stencil]);
}

fn bind_for_read(r: &mut Raster, output: &SampleOutput, srv: u32, shader: &[u8], raster: u32) {
    r.targets(64, &[output.view]);
    r.call(36, &[0, 0]);
    r.call(43, &[raster]);
    write_value(&mut r.machine, ARRAY, [srv, r.constants]);
    r.call(8, &[0, 1, ARRAY]);
    r.call(16, &[0, 1, ARRAY + 4]);
    r.shader(12, 11, FULLSCREEN);
    r.shader(15, 9, shader);
}

fn depth_raster(r: &mut Raster, bias: i32, slope: f32, clamp: f32, clip: bool) -> u32 {
    r.object(
        22,
        D3D11_RASTERIZER_DESC {
            FillMode: 3,
            CullMode: 1,
            DepthBias: bias,
            SlopeScaledDepthBias: slope,
            DepthBiasClamp: clamp,
            DepthClipEnable: i32::from(clip),
            ScissorEnable: 1,
            ..Default::default()
        },
    )
}

// spec: GFX-REC-7
#[test]
fn guest_constant_buffer_offset_zero_fills_the_unbound_tail() {
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    let raster = depth_raster(&mut r, 0, 0., 0., true);
    let (_, dsv, sampled, _) = depth_views(&mut r, 39, 40, 41, 0);
    let mut constants = [0.75f32; 68];
    constants[64..].fill(0.25);
    let dynamic = buffer(&mut r.machine, r.device, Bind::CONSTANT, 272);
    update_dynamic(&mut r.machine, r.context, dynamic, &constants);
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 272,
            BindFlags: Bind::CONSTANT,
            ..Default::default()
        },
    );
    let default = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    r.call(47, &[default, dynamic]);
    write_value(&mut r.machine, ARRAY, [default, 16u32, 16]);
    r.call(123, &[0, 1, ARRAY, ARRAY + 4, ARRAY + 8]);
    r.shader(15, 9, DEPTH_WRITE);
    bind_depth(&mut r, dsv, 0, 0, [0., 1.]);
    clear_depth(&mut r, dsv, 1, 1., 0);
    r.draw(0..64, [0.; 4]);
    bind_for_read(&mut r, &output, sampled, TEXEL_LOAD, raster);
    let texels = output.draw(&mut r, [1., 0., 0., 0.], 64);
    assert!(
        texels[..4]
            .iter()
            .all(|pixel| pixel[0] == 0.25f32.to_bits())
    );
    assert!(texels[4..].iter().all(|pixel| pixel[0] == 0));
}

#[test]
fn windows_depth_and_stencil_through_guest_vtables() {
    use adv_abi::types::{D3D11_DEPTH_STENCIL_DESC, D3D11_DEPTH_STENCILOP_DESC};
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    let raster = depth_raster(&mut r, 0, 0., 0., true);
    let depth_values = buffer(&mut r.machine, r.device, Bind::CONSTANT, 256);
    let depths = buffer(&mut r.machine, r.device, Bind::CONSTANT, 16);
    write_value(&mut r.machine, ARRAY, depths);
    r.call(7, &[0, 1, ARRAY]);
    let keep = D3D11_DEPTH_STENCILOP_DESC {
        StencilFailOp: 1,
        StencilDepthFailOp: 1,
        StencilPassOp: 1,
        StencilFunc: 8,
    };
    let always = r.object(
        21,
        D3D11_DEPTH_STENCIL_DESC {
            DepthEnable: 1,
            DepthWriteMask: 1,
            DepthFunc: 8,
            FrontFace: keep,
            BackFace: keep,
            ..Default::default()
        },
    );
    let mut input = std::array::from_fn::<f32, 64, _>(|i| i as f32 / 63.);
    input[..10].copy_from_slice(&[
        0.5,
        1. / 3.,
        0.5 + 0.5 / 65535.,
        0.5 + 0.49 / 65535.,
        0.5 + 0.5 / 16777215.,
        0.5 + 0.49 / 16777215.,
        1. - 1. / 16777216.,
        1e-30,
        1.5,
        -0.5,
    ]);
    update_dynamic(&mut r.machine, r.context, depth_values, &input);
    for (name, resource, depth, sampled_depth, stencil, tolerance) in [
        ("d16", 53, 55, 56, 0, 1. / 65535.),
        ("d24s8", 44, 45, 46, 47, 1. / 16777215.),
        ("d32f", 39, 40, 41, 0, 0.),
        ("d32fs8", 19, 20, 21, 22, 0.),
    ] {
        let (image, dsv, sampled, stencil_view) =
            depth_views(&mut r, resource, depth, sampled_depth, stencil);
        if name == "d24s8" {
            let staging = r.texture(64, 1, resource, 0, 0);
            clear_depth(&mut r, dsv, 3, 1. / 3., 0x71);
            assert_eq!(r.read(image, staging, 2, 4), [0x71555555; 2]);
        }
        {
            r.call(43, &[raster]);
            r.shader(12, 11, FULLSCREEN);
            r.shader(15, 9, DEPTH_WRITE);
            write_value(&mut r.machine, ARRAY, depth_values);
            r.call(16, &[0, 1, ARRAY]);
            bind_depth(&mut r, dsv, always, 0, [0., 1.]);
            clear_depth(&mut r, dsv, 1, 1., 0);
            r.draw(0..64, [0.; 4]);
            bind_for_read(&mut r, &output, sampled, TEXEL_LOAD, raster);
            output.check(&mut r, &format!(
                "depth-{name}-svdepth-0.5,1/3,0.5+half16,0.5+0.49_16,0.5+half24,0.5+0.49_24,1-2^-24,1e-30,1.5,-0.5,i/63"
            ), [1., 0., 0., 0.], tolerance);
            clear_depth(&mut r, dsv, 1, 1. / 3., 0);
            output.check(
                &mut r,
                &format!("depth-{name}-clear-1/3"),
                [1., 0., 0., 0.],
                tolerance,
            );

            for (test, (bias, slope, clamp), z, clip, viewport) in [
                ("bias1-at-0.5", (1, 0., 0.), [0.5; 3], true, [0., 1.]),
                ("bias100-at-0.5", (100, 0., 0.), [0.5; 3], true, [0., 1.]),
                ("bias-1-at-0.5", (-1, 0., 0.), [0.5; 3], true, [0., 1.]),
                (
                    "slope2-sloped-at-0.5",
                    (0, 2., 0.),
                    [0.25, 1.25, 0.25],
                    true,
                    [0., 1.],
                ),
                (
                    "bias100-clamp1e-5-at-0.5",
                    (100, 0., 1e-5),
                    [0.5; 3],
                    true,
                    [0., 1.],
                ),
                (
                    "none-sloped-at-0.5",
                    (0, 0., 0.),
                    [0.25, 1.25, 0.25],
                    true,
                    [0., 1.],
                ),
                ("noclip-z1.5", (0, 0., 0.), [1.5; 3], false, [0., 1.]),
                (
                    "viewport-0.25-0.75-z0",
                    (0, 0., 0.),
                    [0.; 3],
                    true,
                    [0.25, 0.75],
                ),
            ] {
                let [z0, z1, z2] = z;
                update_dynamic(&mut r.machine, r.context, depths, &[z0, z1, z2, 0f32]);
                r.shader(12, 11, DEPTH_VERTEX);
                r.call(9, &[0, 0, 0]);
                let biased = depth_raster(&mut r, bias, slope, clamp, clip);
                r.call(43, &[biased]);
                bind_depth(&mut r, dsv, always, 0, viewport);
                clear_depth(&mut r, dsv, 1, 1., 0);
                r.draw(0..64, [0.; 4]);
                bind_for_read(&mut r, &output, sampled, TEXEL_LOAD, raster);
                output.check(
                    &mut r,
                    &format!("depth-{name}-{test}"),
                    [1., 0., 0., 0.],
                    tolerance,
                );
            }
        }
        if let Some(stencil_view) = stencil_view {
            for (test, operation, initial, reference, mask, draws) in [
                ("incrsat-from-fe-x2", 4, 0xfe, 0, 0xff, 2),
                ("incr-from-fe-x2", 7, 0xfe, 0, 0xff, 2),
                ("decrsat-from-1-x2", 5, 1, 0, 0xff, 2),
                ("decr-from-1-x2", 8, 1, 0, 0xff, 2),
                ("invert-from-5a", 6, 0x5a, 0, 0xff, 1),
                ("replace-ff-writemask-0f-from-0", 3, 0, 0xff, 0x0f, 1),
                ("incr-writemask-f0-from-0f", 7, 0x0f, 0, 0xf0, 1),
            ] {
                let face = D3D11_DEPTH_STENCILOP_DESC {
                    StencilPassOp: operation,
                    ..keep
                };
                let state = r.object(
                    21,
                    D3D11_DEPTH_STENCIL_DESC {
                        DepthFunc: 8,
                        StencilEnable: 1,
                        StencilReadMask: 0xff,
                        StencilWriteMask: mask,
                        FrontFace: face,
                        BackFace: face,
                        ..Default::default()
                    },
                );
                r.shader(12, 11, FULLSCREEN);
                r.call(9, &[0, 0, 0]);
                r.call(43, &[raster]);
                bind_depth(&mut r, dsv, state, reference, [0., 1.]);
                clear_depth(&mut r, dsv, 2, 1., initial);
                for _ in 0..draws {
                    r.draw(0..64, [0.; 4]);
                }
                bind_for_read(&mut r, &output, stencil_view, STENCIL_LOAD, raster);
                output.check(&mut r, &format!("stencil-{name}-{test}"), [0.; 4], 0.);
            }
        }
    }
}

#[test]
fn windows_block_compression_through_guest_vtables() {
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../../tests/fixtures/d3d11/guest_block_load.dxbc"),
    );
    let indices = (0..16).fold(0u64, |bits, i| bits | (i % 8) << (3 * i));
    let bc4 = |a, b| {
        let mut block = [0; 8];
        block[..2].copy_from_slice(&[a, b]);
        block[2..].copy_from_slice(&indices.to_le_bytes()[..6]);
        block
    };
    let bc1 = [0, 0xf8, 0x1f, 0, 0xe4, 0xe4, 0xe4, 0xe4];
    let bc3 = [bc4(255, 0), bc1].concat();
    let mut bc7 = 1u128 << 6;
    for channel in 0..4 {
        bc7 |= 127 << (7 + 14 * channel);
    }
    bc7 |= 1 << 63;
    for i in 1..16 {
        bc7 |= (i as u128) << (64 + 4 * i);
    }
    for (name, format, data, tolerance) in [
        ("bc1-4color-red-blue", 71, bc1.to_vec(), 1. / 255.),
        (
            "bc1-3color-blue-red-transparent",
            71,
            vec![0x1f, 0, 0, 0xf8, 0xe4, 0xe4, 0xe4, 0xe4],
            0.,
        ),
        (
            "bc1-odd-endpoints-0x0841",
            71,
            vec![0x41, 0x08, 0, 0, 0xe4, 0xe4, 0xe4, 0xe4],
            1. / 255.,
        ),
        ("bc3-alpha8value-color4", 77, bc3, 0.03),
        ("bc4-unorm-8value", 80, bc4(255, 0).to_vec(), 0.03),
        ("bc4-unorm-6value", 80, bc4(0, 255).to_vec(), 0.03),
        (
            "bc4-snorm-127-to-minus127",
            81,
            bc4(0x7f, 0x81).to_vec(),
            0.03,
        ),
        (
            "bc7-mode6-255-to-0-index-i",
            98,
            bc7.to_le_bytes().to_vec(),
            0.,
        ),
    ] {
        r.machine.cpu.write(TEXELS, &data).unwrap();
        write_value(
            &mut r.machine,
            INIT,
            D3D11_SUBRESOURCE_DATA {
                pSysMem: TEXELS,
                SysMemPitch: data.len() as u32,
                SysMemSlicePitch: data.len() as u32,
            },
        );
        let image = r.texture(4, 4, format, Bind::SHADER, INIT);
        let view = create_object(&mut r.machine, r.device, 7, &[image, 0]);
        write_value(&mut r.machine, ARRAY, view);
        r.call(8, &[0, 1, ARRAY]);
        output.check(&mut r, name, [0.; 4], tolerance);
    }
}

#[test]
fn windows_comparison_sampling_through_guest_vtables() {
    let mut r = Raster::new();
    let output = SampleOutput::new(&mut r);
    sampled(
        &mut r,
        41,
        [2, 1],
        &[vec![0.25f32.to_bits(), 0.75f32.to_bits()]],
        0,
    );
    r.shader(
        15,
        9,
        include_bytes!("../../../../../../tests/fixtures/d3d11/guest_compare_sample.dxbc"),
    );
    for (name, filter, comparison, params) in [
        (
            "cmp-point-less-ref=i/32-at-texel0",
            0x80,
            2,
            [0.25, 0., 0., 1. / 32.],
        ),
        (
            "cmp-point-lessequal-ref=i/32-at-texel0",
            0x80,
            4,
            [0.25, 0., 0., 1. / 32.],
        ),
        (
            "cmp-linear-less-ref0.5-u=(i)/64",
            0x95,
            2,
            [0., 1. / 64., 0.5, 0.],
        ),
    ] {
        let s = r.object(
            23,
            D3D11_SAMPLER_DESC {
                Filter: filter,
                AddressU: 3,
                AddressV: 3,
                AddressW: 3,
                ComparisonFunc: comparison,
                MaxLOD: f32::MAX,
                ..Default::default()
            },
        );
        write_value(&mut r.machine, ARRAY, s);
        r.call(10, &[0, 1, ARRAY]);
        output.check(&mut r, name, params, 0.);
    }
}

#[test]
fn windows_anisotropic_sampling_through_guest_vtables() {
    let mut r = Raster::new();
    if gpu::caps(&r.machine.ext)
        .unwrap()
        .adapter
        .starts_with("llvmpipe")
    {
        // Mesa 26.0 and 26.2 both exceed 1/255 on these explicit-gradient sweeps.
        eprintln!("Native lavapipe anisotropic sampling exceeds the Windows tolerance");
        return;
    }
    let output = SampleOutput::new(&mut r);
    let gradient = (0..64)
        .flat_map(|y| (0..64).map(move |x| 0xff000000 | (x * x * 255 / 3969) | ((y * 4) << 8)))
        .collect();
    sampled(&mut r, 28, [64, 64], &[gradient], 0);
    r.shader(15, 9, SAMPLE_GRAD);
    sampler(&mut r, 0x55, 3, [0., 0., 1000.]);
    output.check(
        &mut r,
        "aniso16-single-level-gradient-x=2^(i/8)/64",
        [0.37, 0.41, 1. / 64., 0.],
        1. / 255.,
    );
    let mips: Vec<_> = (0..7)
        .map(|mip| vec![0xff000000 | (mip * 32 + 16); (64usize >> mip).pow(2)])
        .collect();
    sampled(&mut r, 28, [64, 64], &mips, 0);
    output.check(
        &mut r,
        "aniso16-mips-gradient-x=2^(i/8)/64",
        [0.5, 0.5, 1. / 64., 0.],
        1. / 255.,
    );
}

fn explicit_srv(r: &mut Raster, image: u32, format: i32, dimension: i32, words: [u32; 4]) {
    let desc = adv_abi::types::D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: dimension,
        Anonymous: words.map(u32::to_le_bytes).concat().try_into().unwrap(),
    };
    write_value(&mut r.machine, DESCRIPTOR, desc);
    let view = create_object(&mut r.machine, r.device, 7, &[image, DESCRIPTOR]);
    write_value(&mut r.machine, ARRAY, view);
    r.call(8, &[0, 1, ARRAY]);
}

fn copy_rows(r: &mut Raster, source: u32, staging: u32, row: usize, height: usize) -> Vec<u8> {
    r.call(47, &[staging, source]);
    let mapped = map(&mut r.machine, r.context, staging, 1);
    let mut bytes = vec![0; row * height];
    for (i, output) in bytes.chunks_exact_mut(row).enumerate() {
        r.machine
            .cpu
            .read(mapped.pData + i as u32 * mapped.RowPitch, output)
            .unwrap();
    }
    r.call(15, &[staging, 0]);
    bytes
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn rgb32_typed_views_and_staging_keep_guest_texel_stride() {
    const UINT: &[u8] =
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_rgb_uint.dxbc");
    const BUFFER: &[u8] =
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_rgb_buffer.dxbc");

    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let src = [
        [
            0x3f00_0000u32,
            0x3f80_0000,
            0x4000_0000,
            4,
            5,
            6,
            0xdead_beef,
            0,
        ],
        [7, 8, 9, 10, 11, 12, 0xdead_beef, 0],
    ];
    r.machine
        .cpu
        .write(TEXELS, bytemuck::bytes_of(&src))
        .unwrap();
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: TEXELS,
            SysMemPitch: 32,
            SysMemSlicePitch: 64,
        },
    );
    let rgb = r.texture(2, 2, 5, Bind::SHADER, INIT);
    explicit_srv(&mut r, rgb, 6, 4, [0, 1, 0, 0]);
    r.shader(15, 9, TEXEL_LOAD);
    assert_eq!(
        out.draw(&mut r, [1., 0., 0., 0.], 1),
        [[0.5f32, 1., 2., 1.].map(f32::to_bits)]
    );
    explicit_srv(&mut r, rgb, 7, 4, [0, 1, 0, 0]);
    r.shader(15, 9, UINT);
    assert_eq!(
        out.draw(&mut r, [0.; 4], 2)[1],
        [4.0f32, 5., 6., 1.].map(f32::to_bits)
    );

    let staging = r.texture(2, 2, 7, 0, 0);
    let expected: Vec<_> = src
        .iter()
        .flat_map(|row| bytemuck::cast_slice::<_, u8>(&row[..6]))
        .copied()
        .collect();
    assert_eq!(copy_rows(&mut r, rgb, staging, 24, 2), expected);

    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 24,
            BindFlags: Bind::SHADER,
            ..Default::default()
        },
    );
    let buffer = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, INIT]);
    explicit_srv(&mut r, buffer, 6, 1, [0, 2, 0, 0]);
    r.shader(15, 9, BUFFER);
    assert_eq!(
        out.draw(&mut r, [0.; 4], 1),
        [[0.5f32, 1., 2., 1.].map(f32::to_bits)]
    );
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn narrow_array_view_keeps_its_type_with_one_layer_and_loads_the_selected_mip() {
    use adv_abi::types::D3D11_TEXTURE1D_DESC;
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let colors = [0xffffffffu32, 0xff00ff00, 0xffffffff, 0xff0000ff];
    let init: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, &color)| {
            let address = TEXELS + i as u32 * 16;
            r.machine
                .cpu
                .write(address, bytemuck::cast_slice(&[color; 4]))
                .unwrap();
            D3D11_SUBRESOURCE_DATA {
                pSysMem: address,
                SysMemPitch: 16,
                SysMemSlicePitch: 16,
            }
        })
        .collect();
    r.machine
        .cpu
        .write(INIT, bytemuck::cast_slice(&init))
        .unwrap();
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_TEXTURE1D_DESC {
            Width: 4,
            MipLevels: 2,
            ArraySize: 2,
            Format: 28,
            BindFlags: Bind::SHADER,
            ..Default::default()
        },
    );
    let narrow = create_object(&mut r.machine, r.device, 4, &[DESCRIPTOR, INIT]);
    explicit_srv(&mut r, narrow, 28, 3, [0, 2, 1, 1]);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_narrow_array.dxbc"),
    );
    assert_eq!(
        out.draw(&mut r, [0.; 4], 2),
        [[1.0f32, 0., 2., 18.].map(f32::to_bits); 2]
    );
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn multisampled_array_view_loads_nonzero_layer_and_sample() {
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let ms = r.image(
        D3D11_TEXTURE2D_DESC {
            Width: 4,
            Height: 1,
            MipLevels: 1,
            ArraySize: 2,
            Format: 28,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 4,
                Quality: 0,
            },
            BindFlags: Bind::SHADER | Bind::RENDER_TARGET,
            ..Default::default()
        },
        0,
    );
    let rtv = create_object(&mut r.machine, r.device, 9, &[ms, 0]);
    r.clear(rtv, [1., 0., 0., 1.]);
    let first = adv_abi::types::D3D11_RENDER_TARGET_VIEW_DESC {
        Format: 28,
        ViewDimension: 7,
        Anonymous: [0u32, 1, 0]
            .map(u32::to_le_bytes)
            .concat()
            .try_into()
            .unwrap(),
    };
    write_value(&mut r.machine, DESCRIPTOR, first);
    let rtv = create_object(&mut r.machine, r.device, 9, &[ms, DESCRIPTOR]);
    r.clear(rtv, [0., 0., 1., 1.]);
    explicit_srv(&mut r, ms, 28, 7, [0, 2, 0, 0]);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_ms_array.dxbc"),
    );
    assert_eq!(
        out.draw(&mut r, [0.; 4], 1),
        [[1.0f32, 0., 2., 4.].map(f32::to_bits)]
    );
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn compressed_typeless_copies_keep_blocks_and_decode_the_view() {
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let block = [0x0080_0080u32, 0];
    r.machine
        .cpu
        .write(TEXELS, bytemuck::bytes_of(&block))
        .unwrap();
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: TEXELS,
            SysMemPitch: 8,
            SysMemSlicePitch: 8,
        },
    );
    let source = r.texture(4, 4, 70, Bind::SHADER, INIT);
    let staging = r.texture(4, 4, 71, 0, 0);
    r.shader(15, 9, TEXEL_LOAD);
    explicit_srv(&mut r, source, 71, 4, [0, 1, 0, 0]);
    let linear = out.draw(&mut r, [0.; 4], 1)[0];
    explicit_srv(&mut r, source, 72, 4, [0, 1, 0, 0]);
    let srgb = out.draw(&mut r, [0.; 4], 1)[0];
    assert!(f32::from_bits(linear[1]) > f32::from_bits(srgb[1]));
    assert_eq!(
        copy_rows(&mut r, source, staging, 8, 1),
        bytemuck::bytes_of(&block)
    );
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn d32s8_staging_copies_both_aspects() {
    let mut r = Raster::new();
    let _out = SampleOutput::new(&mut r);
    let texels = [[0.25f32.to_bits(), 17u32], [0.75f32.to_bits(), 218u32]];
    r.machine
        .cpu
        .write(TEXELS, bytemuck::bytes_of(&texels))
        .unwrap();
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: TEXELS,
            SysMemPitch: 16,
            SysMemSlicePitch: 16,
        },
    );
    let source = r.texture(2, 1, 20, Bind::DEPTH_STENCIL, INIT);
    let staging = r.texture(2, 1, 20, 0, 0);
    assert_eq!(
        copy_rows(&mut r, source, staging, 16, 1),
        bytemuck::bytes_of(&texels)
    );
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn packed_color_pairs_sample_and_map_without_losing_green_lanes() {
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    r.shader(15, 9, TEXEL_LOAD);
    for (format, pair) in [(68, [255, 0, 0, 255]), (69, [0, 255, 255, 0])] {
        r.machine.cpu.write(TEXELS, &pair).unwrap();
        write_value(
            &mut r.machine,
            INIT,
            D3D11_SUBRESOURCE_DATA {
                pSysMem: TEXELS,
                SysMemPitch: 4,
                SysMemSlicePitch: 4,
            },
        );
        let source = r.texture(2, 1, format, Bind::SHADER, INIT);
        explicit_srv(&mut r, source, format, 4, [0, 1, 0, 0]);
        assert_eq!(
            out.draw(&mut r, [1., 0., 0., 0.], 2),
            [
                [1.0f32, 0., 0., 1.].map(f32::to_bits),
                [1.0f32, 1., 0., 1.].map(f32::to_bits),
            ]
        );
        let staging = r.texture(2, 1, format, 0, 0);
        assert_eq!(copy_rows(&mut r, source, staging, 4, 1), pair);
    }
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn depth_and_color_views_can_copy_the_same_family_bits() {
    let mut r = Raster::new();
    let _out = SampleOutput::new(&mut r);
    for (format, bytes) in [
        (39, vec![0, 0, 128, 62, 0, 0, 64, 63]),
        (53, vec![0, 64, 0, 192]),
    ] {
        r.machine.cpu.write(TEXELS, &bytes).unwrap();
        write_value(
            &mut r.machine,
            INIT,
            D3D11_SUBRESOURCE_DATA {
                pSysMem: TEXELS,
                SysMemPitch: bytes.len() as u32,
                SysMemSlicePitch: bytes.len() as u32,
            },
        );
        let color = r.texture(2, 1, format, Bind::SHADER, INIT);
        let depth = r.texture(2, 1, format, Bind::DEPTH_STENCIL, 0);
        r.call(47, &[depth, color]);
        let staging = r.texture(2, 1, format, 0, 0);
        assert_eq!(copy_rows(&mut r, depth, staging, bytes.len(), 1), bytes);
    }
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn alpha_only_attachment_samples_alpha_and_maps_one_byte() {
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let image = r.texture(
        2,
        1,
        65,
        Bind::SHADER | Bind::RENDER_TARGET | Bind::UNORDERED,
        0,
    );
    let rtv = create_object(&mut r.machine, r.device, 9, &[image, 0]);
    r.clear(rtv, [0., 0., 0., 1.]);
    let uav = create_object(&mut r.machine, r.device, 8, &[image, 0]);
    write_value(&mut r.machine, ARRAY, uav);
    r.call(68, &[0, 1, ARRAY, 0]);
    r.shader(
        18,
        69,
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_alpha_uav.dxbc"),
    );
    r.call(41, &[1, 1, 1]);
    write_value(&mut r.machine, ARRAY, 0u32);
    r.call(68, &[0, 1, ARRAY, 0]);
    explicit_srv(&mut r, image, 65, 4, [0, 1, 0, 0]);
    r.shader(15, 9, TEXEL_LOAD);
    assert_eq!(
        out.draw(&mut r, [1., 0., 0., 0.], 2),
        [
            [0, 0, 0, 1.0f32.to_bits()],
            [0, 0, 0, (64.0f32 / 255.).to_bits()]
        ]
    );
    let staging = r.texture(2, 1, 65, 0, 0);
    assert_eq!(copy_rows(&mut r, image, staging, 2, 1), [255, 64]);
}

// spec: GFX-CAP-5
#[test]
fn typed_array_uav_atomics_return_previous_texels() {
    let mut r = Raster::new();
    for (format, shader, minimum) in [
        (
            42,
            include_bytes!("../../../../../../tests/fixtures/d3d11/format_atomic_uint.dxbc")
                .as_slice(),
            5,
        ),
        (
            43,
            include_bytes!("../../../../../../tests/fixtures/d3d11/format_atomic_sint.dxbc")
                .as_slice(),
            -2i32 as u32,
        ),
    ] {
        let image = r.image(
            D3D11_TEXTURE2D_DESC {
                Width: 10,
                Height: 2,
                MipLevels: 1,
                ArraySize: 3,
                Format: if format == 42 { 39 } else { format },
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BindFlags: Bind::UNORDERED,
                ..Default::default()
            },
            0,
        );
        use adv_abi::types::D3D11_UNORDERED_ACCESS_VIEW_DESC;
        write_value(
            &mut r.machine,
            DESCRIPTOR,
            D3D11_UNORDERED_ACCESS_VIEW_DESC {
                Format: format,
                ViewDimension: 5,
                Anonymous: bytemuck::cast([0u32, 1, 2]),
            },
        );
        let uav = create_object(&mut r.machine, r.device, 8, &[image, DESCRIPTOR]);
        write_value(&mut r.machine, DESCRIPTOR, [5u32, 0, 0, 0]);
        r.call(51, &[uav, DESCRIPTOR]);
        write_value(&mut r.machine, ARRAY, uav);
        r.call(68, &[0, 1, ARRAY, 0]);
        r.shader(18, 69, shader);
        r.call(41, &[1, 1, 1]);
        let staging = r.texture(10, 2, format, 0, 0);
        r.call(46, &[staging, 0, 0, 0, 0, image, 2, 0]);
        let mapped = map(&mut r.machine, r.context, staging, 1);
        let row: [u32; 10] = GuestPtr::new(mapped.pData + mapped.RowPitch)
            .read(&r.machine.cpu)
            .unwrap();
        assert_eq!(row, [9, 5, 8, 0, 8, 12, minimum, 6, 2, 3]);
        r.call(15, &[staging, 0]);
    }
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn float_uav_exchange_keeps_the_image_and_linear_alias_coherent() {
    let mut r = Raster::new();
    let image = r.texture(2, 2, 41, Bind::UNORDERED, 0);
    let view = create_object(&mut r.machine, r.device, 8, &[image, 0]);
    let staging = r.texture(2, 2, 41, 0, 0);
    write_value(&mut r.machine, DESCRIPTOR, [1.5f32, 0., 0., 0.]);
    r.call(52, &[view, DESCRIPTOR]);
    write_value(&mut r.machine, ARRAY, view);
    r.call(68, &[0, 1, ARRAY, 0]);
    r.shader(
        18,
        69,
        include_bytes!("../../../../../../tests/fixtures/d3d11/format_atomic_float.dxbc"),
    );
    for previous in [1.5f32, 2.5] {
        r.call(41, &[1, 1, 1]);
        let bytes = copy_rows(&mut r, image, staging, 8, 2);
        let expected = [1.5f32, 1.5, 2.5, previous].map(f32::to_bits);
        assert_eq!(bytes, bytemuck::bytes_of(&expected));
    }
}

// spec: GFX-CAP-5 GFX-RES-1
#[test]
fn bgra4_attachment_keeps_guest_nibble_order() {
    let mut r = Raster::new();
    let out = SampleOutput::new(&mut r);
    let caps = gpu::caps(&r.machine.ext).unwrap();
    let color =
        crate::d3d11::resource::backing_format(caps, 115) != Some(adv_gpu::Format::Bgra8Unorm);
    let packed = [0, 255, 0, 255];
    r.machine.cpu.write(TEXELS, &packed).unwrap();
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: TEXELS,
            SysMemPitch: 4,
            SysMemSlicePitch: 4,
        },
    );
    let bind = Bind::SHADER | if color { Bind::RENDER_TARGET } else { 0 };
    let image = r.texture(2, 1, 115, bind, INIT);
    if color {
        let rtv = create_object(&mut r.machine, r.device, 9, &[image, 0]);
        r.clear(rtv, [1., 0., 0., 1.]);
    }
    explicit_srv(&mut r, image, 115, 4, [0, 1, 0, 0]);
    r.shader(15, 9, TEXEL_LOAD);
    assert_eq!(
        out.draw(&mut r, [1., 0., 0., 0.], 2),
        [[1.0f32, 0., 0., 1.].map(f32::to_bits); 2]
    );
    let staging = r.texture(2, 1, 115, 0, 0);
    assert_eq!(copy_rows(&mut r, image, staging, 4, 1), [0, 255, 0, 255]);
}
