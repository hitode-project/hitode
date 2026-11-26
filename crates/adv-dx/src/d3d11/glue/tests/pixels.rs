use super::*;
use crate::d3d11::desc::Misc;
use adv_abi::types::{D3D11_INPUT_ELEMENT_DESC, D3D11_SUBRESOURCE_DATA};
use adv_device::gpu::Gpu;

const CODE_BYTES: u32 = 0x420000;
const DESCRIPTOR: u32 = DATA + 0x700;
const INIT: u32 = DATA + 0x800;
const ARRAY: u32 = DATA + 0x900;

fn write_value<T: Pod>(machine: &mut Machine<JitCpu>, address: u32, value: T) {
    GuestPtr::new(address)
        .write(&mut machine.cpu, value)
        .unwrap();
}

fn create_object(machine: &mut Machine<JitCpu>, device: u32, slot: u32, arguments: &[u32]) -> u32 {
    let arguments = [arguments, &[OUT]].concat();
    assert_eq!(
        method(machine, device, slot, &arguments),
        0,
        "creation slot {slot}"
    );
    machine.cpu.read_u32(OUT).unwrap()
}

fn buffer(machine: &mut Machine<JitCpu>, device: u32, flags: u32, size: u32) -> u32 {
    write_value(
        machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: size,
            Usage: 2,
            BindFlags: flags,
            CPUAccessFlags: 0x10000,
            ..Default::default()
        },
    );
    create_object(machine, device, 3, &[DESCRIPTOR, 0])
}

fn texture(
    machine: &mut Machine<JitCpu>,
    device: u32,
    usage: i32,
    bind: u32,
    cpu: u32,
    initial: u32,
    size: u32,
) -> u32 {
    write_value(
        machine,
        DESCRIPTOR,
        D3D11_TEXTURE2D_DESC {
            Width: size,
            Height: size,
            MipLevels: 1,
            ArraySize: 1,
            Format: 28,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind,
            CPUAccessFlags: cpu,
            ..Default::default()
        },
    );
    create_object(machine, device, 5, &[DESCRIPTOR, initial])
}

fn map(
    machine: &mut Machine<JitCpu>,
    context: u32,
    resource: u32,
    access: u32,
) -> D3D11_MAPPED_SUBRESOURCE {
    assert_eq!(
        method(machine, context, 14, &[resource, 0, access, 0, INIT]),
        0
    );
    GuestPtr::new(INIT).read(&machine.cpu).unwrap()
}

fn update_dynamic<T: Pod>(
    machine: &mut Machine<JitCpu>,
    context: u32,
    resource: u32,
    values: &[T],
) {
    let mapping = map(machine, context, resource, 4);
    machine
        .cpu
        .write(mapping.pData, bytemuck::cast_slice(values))
        .unwrap();
    method(machine, context, 15, &[resource, 0]);
}

// spec: GFX-INPUT-1 GFX-REC-1 GFX-RES-1
#[test]
fn windows_fill_and_sampling_rows_through_guest_vtables() {
    let mut machine = machine();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    machine.cpu.map(CODE_BYTES, 4096, Prot::RW).unwrap();
    let (device, context) = create(&mut machine, 1, 0, &[0xb000]);

    let vs_bytes = include_bytes!("../../../../tests/fixtures/shader-core/vs_5_0.dxbc");
    let ps_bytes = include_bytes!("../../../../tests/fixtures/shader-core/ps_4_0_level_9_1.dxbc");
    machine.cpu.write(CODE_BYTES, vs_bytes).unwrap();
    let vs = create_object(
        &mut machine,
        device,
        12,
        &[CODE_BYTES, vs_bytes.len() as u32, 0],
    );
    machine.cpu.write(CODE_BYTES + 2048, ps_bytes).unwrap();
    let ps = create_object(
        &mut machine,
        device,
        15,
        &[CODE_BYTES + 2048, ps_bytes.len() as u32, 0],
    );

    machine.cpu.write(ARRAY, b"POSITION\0TEXCOORD\0").unwrap();
    let elements = [
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: ARRAY,
            Format: 2,
            ..Default::default()
        },
        D3D11_INPUT_ELEMENT_DESC {
            SemanticName: ARRAY + 9,
            Format: 16,
            AlignedByteOffset: u32::MAX,
            ..Default::default()
        },
    ];
    machine
        .cpu
        .write(DESCRIPTOR, bytemuck::cast_slice(&elements))
        .unwrap();
    let layout = create_object(
        &mut machine,
        device,
        11,
        &[DESCRIPTOR, 2, CODE_BYTES, vs_bytes.len() as u32],
    );
    let vb = buffer(&mut machine, device, Bind::VERTEX, 144);
    let vs_constant = buffer(&mut machine, device, Bind::CONSTANT, 32);
    let ps_constant = buffer(&mut machine, device, Bind::CONSTANT, 32);
    update_dynamic(
        &mut machine,
        context,
        vs_constant,
        &[2.0f32 / 7., -2.0 / 7., 1., 1., -1., 1., 0., 0.],
    );
    update_dynamic(
        &mut machine,
        context,
        ps_constant,
        &[0.0f32, 0., 0., 0., 1., 1., 1., 1.],
    );

    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_RASTERIZER_DESC {
            FillMode: 3,
            CullMode: 1,
            DepthClipEnable: 1,
            ..Default::default()
        },
    );
    let raster = create_object(&mut machine, device, 22, &[DESCRIPTOR]);
    let rt = texture(&mut machine, device, 0, Bind::RENDER_TARGET, 0, 0, 7);
    let rtv = create_object(&mut machine, device, 9, &[rt, 0]);
    let staging = texture(&mut machine, device, 3, 0, 0x20000, 0, 7);
    write_value(&mut machine, ARRAY + 32, u32::MAX);
    write_value(
        &mut machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: ARRAY + 32,
            SysMemPitch: 4,
            SysMemSlicePitch: 4,
        },
    );
    let white = texture(&mut machine, device, 1, Bind::SHADER, 0, INIT, 1);
    let srv = create_object(&mut machine, device, 7, &[white, 0]);

    write_value(&mut machine, ARRAY, vb);
    write_value(&mut machine, ARRAY + 4, 24u32);
    write_value(&mut machine, ARRAY + 8, 0u32);
    write_value(&mut machine, ARRAY + 12, vs_constant);
    write_value(&mut machine, ARRAY + 16, ps_constant);
    write_value(&mut machine, ARRAY + 20, rtv);
    write_value(&mut machine, ARRAY + 24, srv);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_VIEWPORT {
            Width: 7.,
            Height: 7.,
            MaxDepth: 1.,
            ..Default::default()
        },
    );
    for (slot, arguments) in [
        (7, vec![0, 1, ARRAY + 12]),
        (8, vec![0, 1, ARRAY + 24]),
        (9, vec![ps, 0, 0]),
        (11, vec![vs, 0, 0]),
        (16, vec![0, 1, ARRAY + 16]),
        (17, vec![layout]),
        (18, vec![0, 1, ARRAY, ARRAY + 4, ARRAY + 8]),
        (24, vec![4]),
        (33, vec![1, ARRAY + 20, 0]),
        (43, vec![raster]),
        (44, vec![1, DESCRIPTOR]),
    ] {
        assert_eq!(
            method(&mut machine, context, slot, &arguments),
            0,
            "binding slot {slot}"
        );
    }

    for (name, [left, top, right, bottom]) in [
        ("quad-1-1-5-5", [1., 1., 5., 5.]),
        ("quad-1.5-1.5-5.5-5.5", [1.5, 1.5, 5.5, 5.5]),
        ("quad-1.49-1.49-5.49-5.49", [1.49, 1.49, 5.49, 5.49]),
        ("quad-1.51-1.51-5.51-5.51", [1.51, 1.51, 5.51, 5.51]),
        ("quad-thin-y-1.5-1.9", [1., 1.5, 5., 1.9]),
        ("quad-thin-y-1.4-1.6", [1., 1.4, 5., 1.6]),
    ] {
        let vertex = |x, y| [x, y, 0.5f32, 1., 0., 0.];
        let vertices = [
            vertex(left, top),
            vertex(right, top),
            vertex(left, bottom),
            vertex(left, bottom),
            vertex(right, top),
            vertex(right, bottom),
        ];
        update_dynamic(&mut machine, context, vb, &vertices);
        write_value(&mut machine, DESCRIPTOR, [0.0f32; 4]);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(method(&mut machine, context, 13, &[6, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, rt]), 0);
        assert_rows(&mut machine, context, staging, name, 0);
    }

    let vb = buffer(&mut machine, device, Bind::VERTEX, 168);
    let p = |x, y| [x, y, 0.5f32, 1., 0., 0.];
    let vertices = [
        p(0., 0.),
        p(1., 1.),
        p(5., 1.),
        p(1., 5.),
        p(1., 5.),
        p(5., 1.),
        p(5., 5.),
    ];
    update_dynamic(&mut machine, context, vb, &vertices);
    write_value(&mut machine, ARRAY, vb);
    write_value(&mut machine, ARRAY + 8, 24u32);
    method(
        &mut machine,
        context,
        18,
        &[0, 1, ARRAY, ARRAY + 4, ARRAY + 8],
    );
    for (format, offset) in [(57, 2), (42, 4)] {
        let indices = [0u32, 0, 1, 2, 3, 4, 5, 6];
        let index = buffer(&mut machine, device, Bind::INDEX, 32);
        if format == 57 {
            update_dynamic(
                &mut machine,
                context,
                index,
                &indices.map(|index| index as u16),
            );
        } else {
            update_dynamic(&mut machine, context, index, &indices);
        }
        assert_eq!(
            method(&mut machine, context, 19, &[index, format, offset]),
            0
        );
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(
            method(&mut machine, context, 20, &[6, 2, 1, u32::MAX, 3]),
            0
        );
        assert_eq!(method(&mut machine, context, 47, &[staging, rt]), 0);
        assert_rows(&mut machine, context, staging, "quad-1-1-5-5", 0);
    }

    write_value(&mut machine, ARRAY + 8, 0u32);
    method(
        &mut machine,
        context,
        18,
        &[0, 1, ARRAY, ARRAY + 4, ARRAY + 8],
    );
    let target = texture(&mut machine, device, 0, Bind::RENDER_TARGET, 0, 0, 32);
    let rtv = create_object(&mut machine, device, 9, &[target, 0]);
    let staging = texture(&mut machine, device, 3, 0, 0x20000, 0, 32);
    let texels: [u32; 16] = std::array::from_fn(|index| {
        let (x, y) = (index as u32 % 4, index as u32 / 4);
        0xff00_0000 | ((y * 0x40 + 0x10) << 8) | (x * 0x40 + 0x10)
    });
    let sampled = texture(&mut machine, device, 2, Bind::SHADER, 0x10000, 0, 4);
    let mapped = map(&mut machine, context, sampled, 4);
    for (y, texels) in texels.as_chunks::<4>().0.iter().enumerate() {
        write_value(
            &mut machine,
            mapped.pData + y as u32 * mapped.RowPitch,
            *texels,
        );
    }
    assert_eq!(method(&mut machine, context, 15, &[sampled, 0]), 0);
    let srv = create_object(&mut machine, device, 7, &[sampled, 0]);
    write_value(&mut machine, ARRAY + 20, rtv);
    write_value(&mut machine, ARRAY + 24, srv);
    assert_eq!(method(&mut machine, context, 8, &[0, 1, ARRAY + 24]), 0);
    assert_eq!(method(&mut machine, context, 33, &[1, ARRAY + 20, 0]), 0);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_VIEWPORT {
            Width: 32.,
            Height: 32.,
            MaxDepth: 1.,
            ..Default::default()
        },
    );
    assert_eq!(method(&mut machine, context, 44, &[1, DESCRIPTOR]), 0);
    update_dynamic(
        &mut machine,
        context,
        vs_constant,
        &[1.0f32 / 16., -1.0 / 16., 1., 1., -1., 1., 0., 0.],
    );
    update_dynamic(
        &mut machine,
        context,
        ps_constant,
        &[1.0f32, 1., 1., 1., 0., 0., 0., 0.],
    );
    for (name, [left, top, right, bottom, uv0, uv1], filter, address) in [
        ("tex-1to1-int-point", [0., 0., 4., 4., 0., 1.], 0, 3),
        (
            "tex-1to1-plus-half-point",
            [0.5, 0.5, 4.5, 4.5, 0., 1.],
            0,
            3,
        ),
        ("tex-1to1-int-linear", [0., 0., 4., 4., 0., 1.], 0x15, 3),
        (
            "tex-1to1-plus-half-linear",
            [0.5, 0.5, 4.5, 4.5, 0., 1.],
            0x15,
            3,
        ),
        ("tex-2x-linear", [0., 0., 8., 8., 0., 1.], 0x15, 3),
        (
            "tex-wrap-uv-minus-0.5-1.5",
            [0., 0., 8., 8., -0.5, 1.5],
            0x15,
            1,
        ),
    ] {
        let vertex = |x, y, u, v| [x, y, 0.5f32, 1., u, v];
        update_dynamic(
            &mut machine,
            context,
            vb,
            &[
                vertex(left, top, uv0, uv0),
                vertex(right, top, uv1, uv0),
                vertex(left, bottom, uv0, uv1),
                vertex(left, bottom, uv0, uv1),
                vertex(right, top, uv1, uv0),
                vertex(right, bottom, uv1, uv1),
            ],
        );
        write_value(
            &mut machine,
            DESCRIPTOR,
            D3D11_SAMPLER_DESC {
                Filter: filter,
                AddressU: address,
                AddressV: address,
                AddressW: address,
                ComparisonFunc: 1,
                MaxLOD: f32::MAX,
                ..Default::default()
            },
        );
        let sampler = create_object(&mut machine, device, 23, &[DESCRIPTOR]);
        write_value(&mut machine, ARRAY + 28, sampler);
        assert_eq!(method(&mut machine, context, 10, &[0, 1, ARRAY + 28]), 0);
        write_value(&mut machine, DESCRIPTOR, [0.0f32; 4]);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(method(&mut machine, context, 13, &[6, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
        // The D3D11 filtering precision allowance covers one 8-bit output code.
        assert_rows(&mut machine, context, staging, name, u8::from(filter != 0));
    }

    machine
        .cpu
        .write(ARRAY + 64, b"POSITION\0TEXCOORD\0")
        .unwrap();
    let mut elements = elements;
    elements[0].SemanticName = ARRAY + 64;
    elements[1].SemanticName = ARRAY + 73;
    elements[1].InputSlot = 1;
    elements[1].AlignedByteOffset = 0;
    elements[1].InputSlotClass = 1;
    elements[1].InstanceDataStepRate = 2;
    machine
        .cpu
        .write(DESCRIPTOR, bytemuck::cast_slice(&elements))
        .unwrap();
    let instance_layout = create_object(
        &mut machine,
        device,
        11,
        &[DESCRIPTOR, 2, CODE_BYTES, vs_bytes.len() as u32],
    );
    assert_eq!(method(&mut machine, context, 17, &[instance_layout]), 0);
    let uv = buffer(&mut machine, device, Bind::VERTEX, 32);
    update_dynamic(
        &mut machine,
        context,
        uv,
        &[
            [0.125f32, 0.125],
            [0.375, 0.125],
            [0.625, 0.125],
            [0.875, 0.125],
        ],
    );
    update_dynamic(&mut machine, context, vb, &vertices);
    write_value(&mut machine, ARRAY + 80, [vb, uv, 24, 8, 0, 0]);
    assert_eq!(
        method(
            &mut machine,
            context,
            18,
            &[0, 2, ARRAY + 80, ARRAY + 88, ARRAY + 96]
        ),
        0
    );

    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_SAMPLER_DESC {
            AddressU: 3,
            AddressV: 3,
            AddressW: 3,
            ComparisonFunc: 1,
            MaxLOD: f32::MAX,
            ..Default::default()
        },
    );
    let sampler = create_object(&mut machine, device, 23, &[DESCRIPTOR]);
    write_value(&mut machine, ARRAY + 28, sampler);
    assert_eq!(method(&mut machine, context, 10, &[0, 1, ARRAY + 28]), 0);

    for (slot, args) in [(40, [0, 6, 2, 1, 3, 0]), (39, [0, 6, 2, 1, 0, 3])] {
        write_value(&mut machine, ARRAY + 104, args);
        write_value(
            &mut machine,
            INIT,
            D3D11_SUBRESOURCE_DATA {
                pSysMem: ARRAY + 104,
                ..Default::default()
            },
        );
        write_value(
            &mut machine,
            DESCRIPTOR,
            D3D11_BUFFER_DESC {
                ByteWidth: 24,
                MiscFlags: Misc::INDIRECT,
                ..Default::default()
            },
        );
        let indirect = create_object(&mut machine, device, 3, &[DESCRIPTOR, INIT]);
        write_value(&mut machine, DESCRIPTOR, [0.0f32; 4]);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(method(&mut machine, context, slot, &[indirect, 4]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
        let mapped = map(&mut machine, context, staging, 1);
        assert_eq!(
            machine
                .cpu
                .read_u32(mapped.pData + mapped.RowPitch + 4)
                .unwrap(),
            texels[3],
            "slot {slot}"
        );
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
    }

    assert_eq!(method(&mut machine, context, 17, &[layout]), 0);
    let srv = create_object(&mut machine, device, 7, &[white, 0]);
    write_value(&mut machine, ARRAY + 24, srv);
    assert_eq!(method(&mut machine, context, 8, &[0, 1, ARRAY + 24]), 0);
    update_dynamic(
        &mut machine,
        context,
        ps_constant,
        &[0.0f32, 0., 0., 0., 1., 1., 1., 1.],
    );
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_RASTERIZER_DESC {
            FillMode: 3,
            CullMode: 1,
            DepthClipEnable: 1,
            ScissorEnable: 1,
            ..Default::default()
        },
    );
    let raster_scissor = create_object(&mut machine, device, 22, &[DESCRIPTOR]);

    for (name, [x, y, w, h], scissor) in [
        ("viewport-0.5-0.5-8x8", [0.5, 0.5, 8., 8.], false),
        ("viewport-2.25-2.75-4x4", [2.25, 2.75, 4., 4.], false),
        ("viewport-1-1-4.5x4.5", [1., 1., 4.5, 4.5], false),
        ("viewport-full-scissor-1-2-5-4", [0., 0., 32., 32.], true),
    ] {
        write_value(
            &mut machine,
            DESCRIPTOR,
            D3D11_VIEWPORT {
                TopLeftX: x,
                TopLeftY: y,
                Width: w,
                Height: h,
                MaxDepth: 1.,
                ..Default::default()
            },
        );
        assert_eq!(method(&mut machine, context, 44, &[1, DESCRIPTOR]), 0);
        write_value(&mut machine, DESCRIPTOR, [1i32, 2, 5, 4]);
        assert_eq!(method(&mut machine, context, 45, &[1, DESCRIPTOR]), 0);
        assert_eq!(
            method(
                &mut machine,
                context,
                43,
                &[if scissor { raster_scissor } else { raster }]
            ),
            0
        );
        update_dynamic(
            &mut machine,
            context,
            vs_constant,
            &[2.0 / w, -2.0 / h, 1., 1., -1., 1., 0., 0.],
        );
        update_dynamic(
            &mut machine,
            context,
            vb,
            &[p(0., 0.), p(w, 0.), p(0., h), p(0., h), p(w, 0.), p(w, h)],
        );
        write_value(&mut machine, DESCRIPTOR, [0.0f32; 4]);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(method(&mut machine, context, 13, &[6, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
        assert_rows(&mut machine, context, staging, name, 0);
    }

    assert_eq!(method(&mut machine, context, 43, &[raster]), 0);
    let mut blend = D3D11_BLEND_DESC::default();
    blend.RenderTarget[0] = adv_abi::types::D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: 1,
        SrcBlend: 2,
        DestBlend: 2,
        BlendOp: 1,
        SrcBlendAlpha: 2,
        DestBlendAlpha: 2,
        BlendOpAlpha: 1,
        RenderTargetWriteMask: 0xf,
        ..Default::default()
    };
    write_value(&mut machine, DESCRIPTOR, blend);
    let additive = create_object(&mut machine, device, 20, &[DESCRIPTOR]);
    assert_eq!(
        method(&mut machine, context, 35, &[additive, 0, u32::MAX]),
        0
    );
    assert_eq!(method(&mut machine, context, 24, &[4]), 0);
    update_dynamic(
        &mut machine,
        context,
        ps_constant,
        &[0.0f32, 0., 0., 0., 0.25, 0.25, 0.25, 0.25],
    );
    write_value(&mut machine, DESCRIPTOR, [0.0f32; 4]);
    assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
    for [l, t, r, b] in [[0., 0., 8., 8.], [2.3, 1.7, 6.1, 6.6]] {
        update_dynamic(
            &mut machine,
            context,
            vb,
            &[p(l, t), p(r, t), p(l, b), p(l, b), p(r, t), p(r, b)],
        );
        assert_eq!(method(&mut machine, context, 13, &[6, 0]), 0);
    }
    assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
    assert_rows(
        &mut machine,
        context,
        staging,
        "shared-edges-additive-quarter",
        0,
    );
    assert_eq!(method(&mut machine, context, 35, &[0, 0, u32::MAX]), 0);
    update_dynamic(
        &mut machine,
        context,
        ps_constant,
        &[0.0f32, 0., 0., 0., 1., 1., 1., 1.],
    );
    for (name, topology, vertices) in [
        (
            "points-1.5,1.5-3,3-5.25,5.75",
            1,
            vec![p(1.5, 1.5), p(3., 3.), p(5.25, 5.75)],
        ),
        (
            "lines-1,1-6,4-and-1.5,6.5-6.5,1.5",
            2,
            vec![p(1., 1.), p(6., 4.), p(1.5, 6.5), p(6.5, 1.5)],
        ),
    ] {
        update_dynamic(&mut machine, context, vb, &vertices);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(method(&mut machine, context, 24, &[topology]), 0);
        assert_eq!(
            method(&mut machine, context, 13, &[vertices.len() as u32, 0]),
            0
        );
        assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
        assert_rows(&mut machine, context, staging, name, 0);
    }
}

fn assert_rows(
    machine: &mut Machine<JitCpu>,
    context: u32,
    staging: u32,
    name: &str,
    tolerance: u8,
) {
    let mapped = map(machine, context, staging, 1);
    let mut matched = 0;
    for row in include_str!("pixels.tsv").lines() {
        let mut fields = row.split('\t');
        if fields.next() != Some(name) {
            continue;
        }
        let y: u32 = fields.next().unwrap().parse().unwrap();
        for (x, expected) in fields.enumerate() {
            let actual = machine
                .cpu
                .read_u32(mapped.pData + y * mapped.RowPitch + x as u32 * 4)
                .unwrap();
            let expected = u32::from_str_radix(expected, 16).unwrap().to_be_bytes();
            assert!(
                actual
                    .to_le_bytes()
                    .into_iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(expected) <= tolerance),
                "{name} ({x}, {y}): {actual:08x} != {expected:02x?}",
            );
        }
        matched += 1;
    }
    assert!(matched != 0, "missing {name}");
    method(machine, context, 15, &[staging, 0]);
}
// spec: GFX-DXGI-2 GFX-DXGI-5 GFX-PACE-1
#[test]
fn guest_swap_chain_presents_the_back_buffer_and_resizes_after_release() {
    use crate::dxgi::swap::Window;
    use adv_abi::types::{DXGI_MODE_DESC, DXGI_SWAP_CHAIN_DESC};
    use adv_device::frame::test_support::CapturePresenter;

    let mut machine = machine();
    let (presenter, frames) = CapturePresenter::new();
    let (hardware, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
        software: !cfg!(target_vendor = "apple"),
        validation: true,
        ..Default::default()
    })
    .unwrap();
    let gpu = Gpu::request_with_presenter(hardware, queue, Box::new(presenter)).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    configure_swap_chains(
        &mut machine,
        9600,
        |_, _, window| {
            (window == 1).then_some(Window {
                client: [8, 6],
                minimized: false,
            })
        },
        |_, _, _| Ok(()),
    );

    let swap_output = OUT + 32;
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Format: 28,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: 32,
        BufferCount: 2,
        OutputWindow: 1,
        Windowed: 1,
        SwapEffect: 4,
        Flags: 64,
    };
    write_value(&mut machine, DESCRIPTOR, desc);
    let create = machine
        .resolve_import(DLL, "D3D11CreateDeviceAndSwapChain")
        .unwrap();
    assert_eq!(
        guest_call(
            &mut machine,
            create,
            &[
                0,
                1,
                0,
                0,
                0,
                0,
                7,
                DESCRIPTOR,
                swap_output,
                OUT,
                LEVEL,
                CONTEXT
            ]
        ),
        0
    );
    let chain = machine.cpu.read_u32(swap_output).unwrap();
    let device = machine.cpu.read_u32(OUT).unwrap();
    let context = machine.cpu.read_u32(CONTEXT).unwrap();
    machine.cpu.write(IID, &IID_IDXGISWAPCHAIN2).unwrap();
    assert_eq!(method(&mut machine, chain, 0, &[IID, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), chain);
    assert_eq!(method(&mut machine, chain, 32, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 1);
    assert_ne!(method(&mut machine, chain, 33, &[]), 0);

    machine.cpu.write(IID, &IID_ID3D11TEXTURE2D).unwrap();
    assert_eq!(method(&mut machine, chain, 9, &[1, IID, OUT]), 0x887a0001);
    assert_eq!(method(&mut machine, chain, 9, &[0, IID, OUT]), 0);
    let resource = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(
        method(&mut machine, chain, 13, &[0, 10, 9, 0, 64]),
        0x887a0001
    );
    let rtv = create_object(&mut machine, device, 9, &[resource, 0]);
    assert_eq!(method(&mut machine, resource, 2, &[]), 0);
    assert_eq!(
        method(&mut machine, chain, 13, &[0, 10, 9, 0, 64]),
        0x887a0001
    );
    write_value(&mut machine, DESCRIPTOR, [1.0f32, 0., 0., 1.]);
    assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
    assert_eq!(method(&mut machine, rtv, 2, &[]), 0);
    assert_eq!(method(&mut machine, chain, 8, &[0, 1]), 0);
    assert_eq!(method(&mut machine, chain, 17, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(method(&mut machine, chain, 8, &[0, 0]), 0);
    let frame = frames
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    assert_eq!((frame.pixels.width, frame.pixels.height), (8, 6));
    assert!(
        frame
            .pixels
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [0, 0, 255, 255])
    );
    assert_eq!(method(&mut machine, chain, 13, &[0, 10, 9, 0, 64]), 0);
    assert_eq!(method(&mut machine, chain, 9, &[0, IID, OUT]), 0);
    let resource = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, resource, 10, &[DESCRIPTOR]), 0);
    let replacement: D3D11_TEXTURE2D_DESC = GuestPtr::new(DESCRIPTOR).read(&machine.cpu).unwrap();
    assert_eq!((replacement.Width, replacement.Height), (10, 9));
    assert_eq!(method(&mut machine, resource, 2, &[]), 0);
    assert_eq!(method(&mut machine, chain, 2, &[]), 1);
    assert_eq!(method(&mut machine, chain, 2, &[]), 0);
    assert_eq!(method(&mut machine, context, 2, &[]), 0);
    assert_eq!(method(&mut machine, device, 2, &[]), 0);
}

// spec: GFX-RES-1 GFX-DXGI-5
#[test]
fn gdi_surface_dc_reads_gpu_output_and_uploads_gdi_writes() {
    use crate::interop::MemoryDc;

    let mut machine = machine();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    configure_surface_dc(
        &mut machine,
        MemoryDc {
            create: |env, surface, done| {
                assert_eq!(env.cpu.read_u32(surface.pixels).unwrap(), 0xffff0000);
                done(env, Ok(7))
            },
            flush: |env, dc, surface, done| {
                assert_eq!(dc, 7);
                env.cpu.write_u32(surface.pixels, 0xff0000ff).unwrap();
                done(env, Ok(()))
            },
            release: |env, dc, done| {
                assert_eq!(dc, 7);
                done(env, Ok(()))
            },
            discard: |_, _| (),
        },
    );
    let (device, context) = create(&mut machine, 1, 0, &[0xb000]);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: 2,
        Height: 2,
        MipLevels: 1,
        ArraySize: 1,
        Format: 87,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BindFlags: Bind::RENDER_TARGET,
        MiscFlags: crate::d3d11::desc::Misc::GDI_COMPATIBLE,
        ..Default::default()
    };
    write_value(&mut machine, DESCRIPTOR, desc);
    let target = create_object(&mut machine, device, 5, &[DESCRIPTOR, 0]);
    let rtv = create_object(&mut machine, device, 9, &[target, 0]);
    write_value(&mut machine, DESCRIPTOR, [1.0f32, 0., 0., 1.]);
    assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
    machine.cpu.write(IID, &IID_IDXGISURFACE1).unwrap();
    assert_eq!(method(&mut machine, target, 0, &[IID, OUT]), 0);
    let surface = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, surface, 11, &[0, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 7);
    assert_eq!(method(&mut machine, surface, 12, &[0]), 0);

    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_TEXTURE2D_DESC {
            Usage: 3,
            CPUAccessFlags: 0x20000,
            BindFlags: 0,
            MiscFlags: 0,
            ..desc
        },
    );
    let staging = create_object(&mut machine, device, 5, &[DESCRIPTOR, 0]);
    assert_eq!(method(&mut machine, context, 47, &[staging, target]), 0);
    let mapped = map(&mut machine, context, staging, 1);
    assert_eq!(machine.cpu.read_u32(mapped.pData).unwrap(), 0xff0000ff);
    assert_eq!(machine.cpu.read_u32(mapped.pData + 4).unwrap(), 0xffff0000);
}
