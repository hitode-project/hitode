use super::*;

mod raster;
use crate::d3d11::desc::Misc;
use adv_abi::types::{
    D3D11_BOX, D3D11_INPUT_ELEMENT_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE1D_DESC,
    D3D11_TEXTURE3D_DESC,
};
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

// spec: GFX-INPUT-1 GFX-REC-1 GFX-REC-7 GFX-RES-1
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
        let restore_indices = |machine: &mut Machine<JitCpu>| {
            if format == 57 {
                update_dynamic(machine, context, index, &indices.map(|index| index as u16));
            } else {
                update_dynamic(machine, context, index, &indices);
            }
        };
        restore_indices(&mut machine);
        assert_eq!(
            method(&mut machine, context, 19, &[index, format, offset]),
            0
        );
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(
            method(&mut machine, context, 20, &[6, 2, 1, u32::MAX, 3]),
            0
        );
        assert_eq!(method(&mut machine, context, 111, &[]), 0);
        #[cfg(feature = "d3d9-counters")]
        let before = gpu::capture_counters(&machine.ext).unwrap();
        assert_eq!(
            method(&mut machine, context, 20, &[6, 2, 1, u32::MAX, 3]),
            0
        );
        #[cfg(feature = "d3d9-counters")]
        {
            let after = gpu::capture_counters(&machine.ext).unwrap();
            assert_eq!(after.captures_reused - before.captures_reused, 2);
            assert_eq!(after.ring_bytes, before.ring_bytes);
        }
        assert_eq!(method(&mut machine, context, 47, &[staging, rt]), 0);
        assert_rows(&mut machine, context, staging, "quad-1-1-5-5", 0);

        let mapped = map(&mut machine, context, index, 5);
        if format == 57 {
            write_value(&mut machine, mapped.pData + 2 * offset, [1u16; 6]);
        } else {
            write_value(&mut machine, mapped.pData + 2 * offset, [1u32; 6]);
        }
        assert_eq!(method(&mut machine, context, 15, &[index, 0]), 0);
        assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
        assert_eq!(
            method(&mut machine, context, 20, &[6, 2, 1, u32::MAX, 3]),
            0
        );
        assert_eq!(method(&mut machine, context, 47, &[staging, rt]), 0);
        let mapped = map(&mut machine, context, staging, 1);
        assert_eq!(
            machine
                .cpu
                .read_u32(mapped.pData + 2 * mapped.RowPitch + 8)
                .unwrap(),
            0
        );
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
        restore_indices(&mut machine);
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
    use adv_abi::types::{
        DXGI_FRAME_STATISTICS, DXGI_MODE_DESC, DXGI_PRESENT_PARAMETERS, DXGI_RATIONAL,
        DXGI_SWAP_CHAIN_DESC,
    };
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
        |env, _, _, then| then(env, adv_machine::Resume::Returned(1)),
        None,
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
    check_vtable(&mut machine, chain, "dxgi.dll", "IDXGISwapChain2");
    let device = machine.cpu.read_u32(OUT).unwrap();
    let context = machine.cpu.read_u32(CONTEXT).unwrap();
    machine.cpu.write(IID, &IID_IDXGISWAPCHAIN2).unwrap();
    assert_eq!(method(&mut machine, chain, 0, &[IID, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), chain);
    assert_eq!(method(&mut machine, chain, 32, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 1);
    assert_ne!(method(&mut machine, chain, 33, &[]), 0);
    assert_eq!(method(&mut machine, chain, 10, &[1, 0]), 0x887a0001);
    write_value(
        &mut machine,
        DESCRIPTOR,
        DXGI_MODE_DESC {
            Width: 12,
            Height: 10,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 75,
                Denominator: 1,
            },
            Format: 28,
            ..Default::default()
        },
    );
    assert_eq!(method(&mut machine, chain, 14, &[DESCRIPTOR]), 0);
    assert_eq!(method(&mut machine, chain, 12, &[DESCRIPTOR]), 0);
    let resized: DXGI_SWAP_CHAIN_DESC = GuestPtr::new(DESCRIPTOR).read(&machine.cpu).unwrap();
    assert_eq!(resized.BufferDesc.RefreshRate.Numerator, 75);
    assert_eq!(
        (resized.BufferDesc.Width, resized.BufferDesc.Height),
        (8, 6)
    );

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
    write_value(&mut machine, DESCRIPTOR, DXGI_PRESENT_PARAMETERS::default());
    assert_eq!(method(&mut machine, chain, 22, &[0, 0, DESCRIPTOR]), 0);
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
    assert_eq!(method(&mut machine, chain, 16, &[DESCRIPTOR]), 0);
    let stats: DXGI_FRAME_STATISTICS = GuestPtr::new(DESCRIPTOR).read(&machine.cpu).unwrap();
    assert_eq!(stats.PresentCount, 1);
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

// spec: GFX-RES-1 GFX-REC-8
#[test]
fn guest_staging_write_uploads_before_the_recorded_copy() {
    let mut machine = machine();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    let (device, context) = create(&mut machine, 1, 0, &[0xb000]);
    let target = texture(&mut machine, device, 0, Bind::RENDER_TARGET, 0, 0, 2);
    let upload = texture(&mut machine, device, 3, 0, 0x10000, 0, 2);
    let readback = texture(&mut machine, device, 3, 0, 0x20000, 0, 2);
    #[cfg(feature = "d3d9-counters")]
    let before = gpu::traffic(&machine.ext).unwrap();
    let mapped = map(&mut machine, context, upload, 2);
    write_value(&mut machine, mapped.pData, [0xff12_3456u32, 0xffab_cdef]);
    write_value(
        &mut machine,
        mapped.pData + mapped.RowPitch,
        [0xffde_bc9au32, 0xff78_5634],
    );
    assert_eq!(method(&mut machine, context, 15, &[upload, 0]), 0);
    assert_eq!(method(&mut machine, context, 47, &[target, upload]), 0);
    assert_eq!(method(&mut machine, context, 47, &[readback, target]), 0);
    let mapped = map(&mut machine, context, readback, 1);
    let row = |y| {
        GuestPtr::<[u32; 2]>::new(mapped.pData + y * mapped.RowPitch)
            .read(&machine.cpu)
            .unwrap()
    };
    assert_eq!(row(0), [0xff12_3456, 0xffab_cdef]);
    assert_eq!(row(1), [0xffde_bc9a, 0xff78_5634]);
    assert_eq!(method(&mut machine, context, 15, &[readback, 0]), 0);
    #[cfg(feature = "d3d9-counters")]
    {
        let traffic = gpu::traffic(&machine.ext).unwrap();
        assert_eq!(traffic.upload_bytes - before.upload_bytes, 16);
        assert_eq!(traffic.readback_bytes - before.readback_bytes, 16);
        assert!(traffic.submissions > before.submissions);
        let counters = gpu::frame_counters(&machine.ext).unwrap();
        assert_eq!((counters.maps, counters.constant_time_maps), (2, 0));
    }
}

// spec: GFX-REC-7
#[test]
fn gpu_copies_preserve_dynamic_buffer_bytes_across_no_overwrite() {
    let mut machine = machine();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    let (device, context) = create(&mut machine, 1, 0, &[0xb000]);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 16,
            ..Default::default()
        },
    );
    let source = create_object(&mut machine, device, 3, &[DESCRIPTOR, 0]);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut machine, device, 3, &[DESCRIPTOR, 0]);
    write_value(&mut machine, ARRAY, [11u32, 22, 33, 44]);
    assert_eq!(
        method(&mut machine, context, 48, &[source, 0, 0, ARRAY, 0, 0]),
        0
    );
    let box_address = ARRAY + 16;
    write_value(
        &mut machine,
        box_address,
        D3D11_BOX {
            left: 4,
            right: 12,
            bottom: 1,
            back: 1,
            ..Default::default()
        },
    );
    for bind in [Bind::CONSTANT, Bind::VERTEX, Bind::INDEX, Bind::SHADER] {
        let dynamic = buffer(&mut machine, device, bind, 16);
        update_dynamic(&mut machine, context, dynamic, &[1u32, 2, 3, 4]);
        assert_eq!(
            method(
                &mut machine,
                context,
                46,
                &[dynamic, 0, 4, 0, 0, source, 0, box_address]
            ),
            0
        );
        #[cfg(feature = "d3d9-counters")]
        let before = gpu::traffic(&machine.ext).unwrap();
        #[cfg(feature = "d3d9-counters")]
        let before_maps = gpu::frame_counters(&machine.ext).unwrap();
        let mapped = map(&mut machine, context, dynamic, 5);
        let read = |machine: &Machine<JitCpu>, address| {
            GuestPtr::<[u32; 4]>::new(address)
                .read(&machine.cpu)
                .unwrap()
        };
        assert_eq!(
            read(&machine, mapped.pData),
            [1, 22, 33, 4],
            "bind {bind:#x}"
        );
        write_value(&mut machine, mapped.pData + 12, 55u32);
        assert_eq!(method(&mut machine, context, 15, &[dynamic, 0]), 0);
        assert_eq!(map(&mut machine, context, dynamic, 5).pData, mapped.pData);
        assert_eq!(method(&mut machine, context, 15, &[dynamic, 0]), 0);
        #[cfg(feature = "d3d9-counters")]
        {
            let traffic = gpu::traffic(&machine.ext).unwrap();
            let counters = gpu::frame_counters(&machine.ext).unwrap();
            assert_eq!(traffic.readback_bytes - before.readback_bytes, 16);
            assert_eq!(counters.maps - before_maps.maps, 2);
            assert_eq!(
                counters.constant_time_maps - before_maps.constant_time_maps,
                1
            );
        }
        assert_eq!(method(&mut machine, context, 47, &[staging, dynamic]), 0);
        let staged = map(&mut machine, context, staging, 1);
        assert_eq!(
            read(&machine, staged.pData),
            [1, 22, 33, 55],
            "bind {bind:#x}"
        );
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[dynamic, source]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, dynamic]), 0);
        let staged = map(&mut machine, context, staging, 1);
        assert_eq!(read(&machine, staged.pData), [11, 22, 33, 44]);
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
        let mapped = map(&mut machine, context, dynamic, 5);
        assert_eq!(
            read(&machine, mapped.pData),
            [11, 22, 33, 44],
            "bind {bind:#x}"
        );
        write_value(&mut machine, mapped.pData + 12, 55u32);
        assert_eq!(method(&mut machine, context, 15, &[dynamic, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, dynamic]), 0);
        let staged = map(&mut machine, context, staging, 1);
        assert_eq!(read(&machine, staged.pData), [11, 22, 33, 55]);
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
    }
}

// spec: GFX-REC-7
#[test]
fn gpu_copies_preserve_dynamic_texture_bytes_across_discard() {
    let mut machine = machine();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    let (device, context) = create(&mut machine, 1, 0, &[0xb000]);
    let pixels = [11u32, 22, 33, 44, 55, 66, 77, 88];
    write_value(&mut machine, ARRAY, pixels);
    let initial = ARRAY + 32;
    for (slot, height, depth) in [(4, 1, 1), (5, 2, 1), (6, 2, 2)] {
        write_value(
            &mut machine,
            initial,
            D3D11_SUBRESOURCE_DATA {
                pSysMem: ARRAY,
                SysMemPitch: 8,
                SysMemSlicePitch: 8 * height,
            },
        );
        let mut make = |usage, bind, cpu, data| {
            match slot {
                4 => write_value(
                    &mut machine,
                    DESCRIPTOR,
                    D3D11_TEXTURE1D_DESC {
                        Width: 2,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: 28,
                        Usage: usage,
                        BindFlags: bind,
                        CPUAccessFlags: cpu,
                        ..Default::default()
                    },
                ),
                5 => return texture(&mut machine, device, usage, bind, cpu, data, 2),
                6 => write_value(
                    &mut machine,
                    DESCRIPTOR,
                    D3D11_TEXTURE3D_DESC {
                        Width: 2,
                        Height: height,
                        Depth: depth,
                        MipLevels: 1,
                        Format: 28,
                        Usage: usage,
                        BindFlags: bind,
                        CPUAccessFlags: cpu,
                        ..Default::default()
                    },
                ),
                _ => unreachable!(),
            }
            create_object(&mut machine, device, slot, &[DESCRIPTOR, data])
        };
        let source = make(0, Bind::SHADER, 0, initial);
        let dynamic = make(2, Bind::SHADER, 0x10000, 0);
        let staging = make(3, 0, 0x20000, 0);
        assert_eq!(method(&mut machine, context, 47, &[dynamic, source]), 0);
        #[cfg(feature = "d3d9-counters")]
        let before = gpu::traffic(&machine.ext).unwrap();
        let mapped = map(&mut machine, context, dynamic, 4);
        let addresses = |mapped: D3D11_MAPPED_SUBRESOURCE| {
            (0..depth).flat_map(move |z| {
                (0..height).flat_map(move |y| {
                    let row = mapped.pData + z * mapped.DepthPitch + y * mapped.RowPitch;
                    [row, row + 4]
                })
            })
        };
        for (address, value) in addresses(mapped).zip(pixels) {
            assert_eq!(
                machine.cpu.read_u32(address).unwrap(),
                value,
                "texture slot {slot}"
            );
        }
        #[cfg(feature = "d3d9-counters")]
        assert_eq!(
            gpu::traffic(&machine.ext).unwrap().readback_bytes - before.readback_bytes,
            u64::from(8 * height * depth)
        );
        write_value(&mut machine, mapped.pData, 99u32);
        assert_eq!(method(&mut machine, context, 15, &[dynamic, 0]), 0);
        assert_eq!(method(&mut machine, context, 47, &[staging, dynamic]), 0);
        let staged = map(&mut machine, context, staging, 1);
        for (address, value) in
            addresses(staged).zip([99].into_iter().chain(pixels[1..].iter().copied()))
        {
            assert_eq!(
                machine.cpu.read_u32(address).unwrap(),
                value,
                "texture slot {slot}"
            );
        }
        assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
    }
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

// spec: GFX-DXGI-5
#[test]
fn alt_enter_respects_window_association_and_toggles_exclusive_mode() {
    use adv_abi::types::{DXGI_MODE_DESC, DXGI_SWAP_CHAIN_DESC};
    use adv_device::surface::{Snapshot, SurfaceStore};
    let mut machine = machine();
    machine
        .ext
        .insert(SurfaceStore::new(8, 6, 0x500000, 65536, Snapshot::default()).unwrap());
    crate::ddraw::install(
        &mut machine,
        crate::ddraw::Provider {
            dc: crate::interop::MemoryDc {
                create: |_, _, _| unreachable!(),
                flush: |_, _, _, _| unreachable!(),
                release: |_, _, _| unreachable!(),
                discard: |_, _| unreachable!(),
            },
            memory: |_| unreachable!(),
            window: |_, _, _| None,
            submitted: |_, _, _, _| (),
            completed: |_, _| None,
            released: |_, _| (),
            vsync: |_| None,
        },
    )
    .unwrap();
    let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    configure_swap_chains(
        &mut machine,
        9600,
        |_, _, hwnd| {
            (hwnd == 1).then_some(crate::dxgi::swap::Window {
                client: [8, 6],
                minimized: false,
            })
        },
        |env, _, _, then| then(env, adv_machine::Resume::Returned(1)),
        None,
    );
    write_value(
        &mut machine,
        DESCRIPTOR,
        DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Format: 28,
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferCount: 1,
            BufferUsage: 32,
            OutputWindow: 1,
            Windowed: 1,
            ..Default::default()
        },
    );
    let create = machine
        .resolve_import(DLL, "D3D11CreateDeviceAndSwapChain")
        .unwrap();
    let chain_out = OUT + 4;
    let mut creation = [
        0, 1, 0, 0, 0, 0, 7, DESCRIPTOR, chain_out, OUT, LEVEL, CONTEXT,
    ];
    assert_eq!(guest_call(&mut machine, create, &creation), 0);
    let chain = machine.cpu.read_u32(chain_out).unwrap();
    let alt = machine.resolve_import("fixture", "alt_enter").unwrap();
    assert_eq!(guest_call(&mut machine, alt, &[1]), 1);
    assert_eq!(method(&mut machine, chain, 11, &[OUT, 0]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 1);
    assert_eq!(guest_call(&mut machine, alt, &[1]), 1);
    assert_eq!(method(&mut machine, chain, 11, &[OUT, 0]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);

    machine.cpu.write(IID, &IID_IDXGIFACTORY1).unwrap();
    assert_eq!(method(&mut machine, chain, 6, &[IID, OUT]), 0);
    let factory = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, factory, 8, &[0, 2]), 0);
    for flags in [1, 2] {
        assert_eq!(method(&mut machine, factory, 8, &[1, flags]), 0);
        assert_eq!(guest_call(&mut machine, alt, &[1]), 0);
    }
    assert_eq!(method(&mut machine, factory, 8, &[0, 0]), 0);
    assert_eq!(guest_call(&mut machine, alt, &[1]), 1);
    assert_eq!(method(&mut machine, chain, 15, &[OUT]), 0);
    let output = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, chain, 2, &[]), 0);
    assert!(!crate::dxgi::has_swap_chain(&machine.ext, 1));

    creation[1] = 5;
    assert_eq!(guest_call(&mut machine, create, &creation), 0);
    let software_chain = machine.cpu.read_u32(chain_out).unwrap();
    assert_eq!(
        method(&mut machine, software_chain, 10, &[1, 0]),
        0x887a0004
    );
    assert_eq!(
        method(&mut machine, software_chain, 10, &[1, output]),
        0x887a0001
    );
    assert_eq!(guest_call(&mut machine, alt, &[1]), 0);
}

fn presented_device(
    machine: &mut Machine<JitCpu>,
) -> (
    u32,
    u32,
    u32,
    std::sync::mpsc::Receiver<adv_device::frame::test_support::CapturedFrame>,
) {
    let (presenter, frames) = adv_device::frame::test_support::CapturePresenter::new();
    let (chain, device, context) =
        presented_device_with(machine, Box::new(presenter), adv_gpu::caps::Modern::empty());
    (chain, device, context, frames)
}

fn presented_device_with(
    machine: &mut Machine<JitCpu>,
    presenter: Box<dyn adv_device::frame::Presenter>,
    disable: adv_gpu::caps::Modern,
) -> (u32, u32, u32) {
    use adv_abi::types::{DXGI_MODE_DESC, DXGI_SWAP_CHAIN_DESC};
    let (hardware, queue) = adv_gpu::Device::open(&adv_gpu::queue::DeviceOptions {
        software: !cfg!(target_vendor = "apple"),
        validation: true,
        disable,
        ..Default::default()
    })
    .unwrap();
    let gpu = Gpu::request_with_presenter(hardware, queue, presenter).unwrap();
    machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
    configure_swap_chains(
        machine,
        19045,
        |_, _, window| {
            (window == 1).then_some(crate::dxgi::swap::Window {
                client: [32, 24],
                minimized: false,
            })
        },
        |env, _, _, then| then(env, adv_machine::Resume::Returned(1)),
        None,
    );
    write_value(
        machine,
        DESCRIPTOR,
        DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC {
                Width: 32,
                Height: 24,
                Format: 28,
                ..Default::default()
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: 32,
            BufferCount: 1,
            OutputWindow: 1,
            Windowed: 1,
            ..Default::default()
        },
    );
    let create = machine
        .resolve_import(DLL, "D3D11CreateDeviceAndSwapChain")
        .unwrap();
    assert_eq!(
        guest_call(
            machine,
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
                OUT + 4,
                OUT,
                LEVEL,
                CONTEXT
            ]
        ),
        0
    );
    (
        machine.cpu.read_u32(OUT + 4).unwrap(),
        machine.cpu.read_u32(OUT).unwrap(),
        machine.cpu.read_u32(CONTEXT).unwrap(),
    )
}

// spec: GFX-REC-7 GFX-REC-8 GFX-SAMPLER-4
#[cfg(feature = "d3d9-counters")]
#[test]
fn guest_draw_uploads_reuse_gpu_objects() {
    draw_uploads(adv_gpu::caps::Modern::empty());
}

// spec: GFX-REC-7 GFX-REC-8
#[cfg(all(feature = "d3d9-counters", not(target_vendor = "apple")))]
#[test]
fn guest_draw_uploads_reuse_cached_descriptor_sets() {
    use adv_gpu::caps::Modern;
    draw_uploads(Modern::BINDLESS | Modern::PUSH_DESCRIPTORS);
}

#[cfg(feature = "d3d9-counters")]
fn draw_uploads(disable: adv_gpu::caps::Modern) {
    use adv_device::frame::{Base, Frame, Presenter, Waker};
    use std::{sync::mpsc, time::Duration};

    struct Frames {
        sender: mpsc::Sender<u64>,
        serial: Option<u64>,
    }
    impl Presenter for Frames {
        fn presentable(&self, _: u32, _: u32) -> bool {
            false
        }

        fn present(&mut self, queue: &mut adv_gpu::Queue, frame: Frame) {
            let Base::Gpu { ready, .. } = frame.base else {
                panic!("expected a GPU frame");
            };
            assert!(queue.wait(ready, Duration::from_secs(20)).unwrap());
            self.serial = Some(frame.serial);
        }

        fn service(&mut self, _: &mut adv_gpu::Queue, _: Option<&Waker>) {
            // The owner has polled completion and released this frame's uploads.
            if let Some(serial) = self.serial.take() {
                self.sender.send(serial).unwrap();
            }
        }
    }

    let mut machine = machine();
    let (sender, frames) = mpsc::channel();
    let presenter = Frames {
        sender,
        serial: None,
    };
    let (chain, device, context) =
        presented_device_with(&mut machine, Box::new(presenter), disable);
    machine.cpu.map(CODE_BYTES, 4096, Prot::RW).unwrap();
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
            AlignedByteOffset: 16,
            ..Default::default()
        },
    ];
    let vs_bytes = include_bytes!("../../../../tests/fixtures/shader-core/vs_5_0.dxbc");
    machine.cpu.write(CODE_BYTES, vs_bytes).unwrap();
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
    let vs = create_object(
        &mut machine,
        device,
        12,
        &[CODE_BYTES, vs_bytes.len() as u32, 0],
    );
    let ps_bytes = include_bytes!("../../../../tests/fixtures/d3d11/guest_texel_load.dxbc");
    machine.cpu.write(CODE_BYTES, ps_bytes).unwrap();
    let ps = create_object(
        &mut machine,
        device,
        15,
        &[CODE_BYTES, ps_bytes.len() as u32, 0],
    );
    let vb = buffer(&mut machine, device, Bind::VERTEX, 192);
    let ib = buffer(&mut machine, device, Bind::INDEX, 24);
    let vs_constant = buffer(&mut machine, device, Bind::CONSTANT, 32);
    let ps_constant = buffer(&mut machine, device, Bind::CONSTANT, 16);
    update_dynamic(
        &mut machine,
        context,
        vs_constant,
        &[2.0f32 / 32., -2. / 24., 1., 1., -1., 1., 0., 0.],
    );
    update_dynamic(&mut machine, context, ps_constant, &[0.0f32; 4]);
    let default = texture(&mut machine, device, 0, Bind::SHADER, 0, 0, 1);
    let dynamic = texture(&mut machine, device, 2, Bind::SHADER, 0x10000, 0, 1);
    let views =
        [default, dynamic].map(|texture| create_object(&mut machine, device, 7, &[texture, 0]));

    machine.cpu.write(IID, &IID_ID3D11TEXTURE2D).unwrap();
    assert_eq!(method(&mut machine, chain, 9, &[0, IID, OUT]), 0);
    let back = machine.cpu.read_u32(OUT).unwrap();
    let target = create_object(&mut machine, device, 9, &[back, 0]);
    let raster = D3D11_RASTERIZER_DESC {
        FillMode: 3,
        CullMode: 1,
        DepthClipEnable: 1,
        ..Default::default()
    };
    write_value(&mut machine, DESCRIPTOR, raster);
    let raster = create_object(&mut machine, device, 22, &[DESCRIPTOR]);
    let call = |machine: &mut Machine<JitCpu>, slot, args: &[u32]| {
        assert_eq!(
            method(machine, context, slot, args),
            0,
            "context slot {slot}"
        );
    };
    write_value(
        &mut machine,
        ARRAY,
        [vb, 24, 0, vs_constant, ps_constant, target],
    );
    call(&mut machine, 7, &[0, 1, ARRAY + 12]);
    call(&mut machine, 9, &[ps, 0, 0]);
    call(&mut machine, 11, &[vs, 0, 0]);
    call(&mut machine, 16, &[0, 1, ARRAY + 16]);
    call(&mut machine, 17, &[layout]);
    call(&mut machine, 18, &[0, 1, ARRAY, ARRAY + 4, ARRAY + 8]);
    call(&mut machine, 19, &[ib, 57, 0]);
    call(&mut machine, 24, &[4]);
    call(&mut machine, 33, &[1, ARRAY + 20, 0]);
    call(&mut machine, 43, &[raster]);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_VIEWPORT {
            Width: 32.,
            Height: 24.,
            MaxDepth: 1.,
            ..Default::default()
        },
    );
    call(&mut machine, 44, &[1, DESCRIPTOR]);

    const WARMUP: u64 = 16;
    const FRAMES: u64 = 240;
    const DRAWS: u64 = 64;
    let render = |machine: &mut Machine<JitCpu>, frame: u64| {
        write_value(machine, ARRAY, [frame as u8, 128, 0, 255]);
        call(machine, 48, &[default, 0, 0, ARRAY, 4, 4]);
        update_dynamic(machine, context, dynamic, &[[0u8, frame as u8, 128, 255]]);
        for (side, view) in views.into_iter().enumerate() {
            let left = 16. * side as f32;
            let v = |x, y| [x, y, 0.5, 1., 0., 0.];
            let vertices = [
                v(left, 0.),
                v(left + 16., 0.),
                v(left, 24.),
                v(left + 16., 24.),
            ];
            let access = 4 + side as u32;
            let mapped = map(machine, context, vb, access);
            write_value(machine, mapped.pData + 96 * side as u32, vertices);
            call(machine, 15, &[vb, 0]);
            let mapped = map(machine, context, ib, access);
            write_value(
                machine,
                mapped.pData + 12 * side as u32,
                [0u16, 1, 2, 2, 1, 3],
            );
            call(machine, 15, &[ib, 0]);
            write_value(machine, ARRAY, view);
            call(machine, 8, &[0, 1, ARRAY]);
            for _ in 0..DRAWS / 2 {
                call(machine, 12, &[6, 6 * side as u32, 4 * side as u32]);
            }
        }
        assert_eq!(method(machine, chain, 8, &[0, 0]), 0);
        assert_eq!(frames.recv_timeout(Duration::from_secs(20)).unwrap(), frame);
    };
    for frame in 1..=WARMUP {
        render(&mut machine, frame);
    }
    let before = gpu::capture_counters(&machine.ext).unwrap();
    for frame in WARMUP + 1..=WARMUP + FRAMES {
        render(&mut machine, frame);
    }
    let after = gpu::capture_counters(&machine.ext).unwrap();
    let creations = [
        after.buffers_created - before.buffers_created,
        after.device_memory_created - before.device_memory_created,
        after.image_views_created - before.image_views_created,
    ];
    let sets = after.descriptor_sets_allocated - before.descriptor_sets_allocated;
    let updates = after.descriptor_set_updates - before.descriptor_set_updates;
    let pushes = after.descriptor_pushes - before.descriptor_pushes;
    eprintln!(
        "{FRAMES} frames, {} draws, buffer/memory/view creations {creations:?}, descriptor allocations {sets}, updates {updates}, pushes {pushes}",
        FRAMES * DRAWS
    );
    assert_eq!(creations, [0; 3]);
    assert!(sets <= 3 * FRAMES && updates <= 3 * FRAMES);

    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_TEXTURE2D_DESC {
            Width: 32,
            Height: 24,
            MipLevels: 1,
            ArraySize: 1,
            Format: 28,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut machine, device, 5, &[DESCRIPTOR, 0]);
    call(&mut machine, 47, &[staging, back]);
    let mapped = map(&mut machine, context, staging, 1);
    let pixel = |x: u32| {
        GuestPtr::<[u8; 4]>::new(mapped.pData + 12 * mapped.RowPitch + 4 * x)
            .read(&machine.cpu)
            .unwrap()
    };
    let final_color = (WARMUP + FRAMES) as u8;
    assert_eq!(pixel(8), [final_color, 128, 0, 255]);
    assert_eq!(pixel(24), [0, final_color, 128, 255]);
}

// spec: GFX-REC-2 GFX-REC-3 GFX-REC-5 GFX-PACE-1
#[test]
fn guest_present_copies_damage_and_counts_unwritten_frames_without_publication() {
    let mut machine = machine();
    let (chain, device, context, frames) = presented_device(&mut machine);
    let deadline = std::time::Duration::from_secs(20);
    machine.cpu.write(IID, &IID_ID3D11TEXTURE2D).unwrap();
    assert_eq!(method(&mut machine, chain, 9, &[0, IID, OUT]), 0);
    let buffer = machine.cpu.read_u32(OUT).unwrap();
    let rtv = create_object(&mut machine, device, 9, &[buffer, 0]);
    write_value(&mut machine, DESCRIPTOR, [1.0f32, 0.0, 0.0, 1.0]);
    assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
    assert_eq!(method(&mut machine, chain, 8, &[0, 0]), 0);
    let first = frames.recv_timeout(deadline).unwrap();
    assert!(first.damage.is_whole());

    assert_eq!(method(&mut machine, chain, 8, &[0, 0]), 0);
    assert_eq!(method(&mut machine, chain, 17, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 2);

    // An explicit Flush must preserve the recorded writes' contribution to damage.
    write_value(
        &mut machine,
        DESCRIPTOR,
        adv_abi::types::D3D11_BOX {
            left: 10,
            top: 11,
            front: 0,
            right: 12,
            bottom: 13,
            back: 1,
        },
    );
    write_value(&mut machine, ARRAY, [[0u8, 255, 0, 255]; 4]);
    assert_eq!(
        method(
            &mut machine,
            context,
            48,
            &[buffer, 0, DESCRIPTOR, ARRAY, 8, 16]
        ),
        0
    );
    assert_eq!(method(&mut machine, context, 111, &[]), 0);
    assert_eq!(method(&mut machine, chain, 8, &[0, 0]), 0);
    let other = frames.recv_timeout(deadline).unwrap();
    assert_eq!(other.pixels.frames, first.pixels.frames + 1);
    assert_eq!(other.damage.area(), 64);
    for y in 0..24 {
        for x in 0..32 {
            let n = 4 * (y * 32 + x);
            let expected = if (10..12).contains(&x) && (11..13).contains(&y) {
                [0, 255, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            assert_eq!(other.pixels.bgra[n..n + 4], expected);
        }
    }
}

// spec: GFX-DXGI-4 GFX-DXGI-5
#[test]
fn guest_output_ownership_gamma_surface_copy_and_duplication_share_the_display() {
    use adv_abi::types::{
        DXGI_GAMMA_CONTROL, DXGI_GAMMA_CONTROL_CAPABILITIES, DXGI_OUTDUPL_DESC,
        DXGI_OUTDUPL_FRAME_INFO, DXGI_RGB,
    };
    use adv_device::surface::{Snapshot, SurfaceStore};
    let mut machine = machine();
    crate::ddraw::install(
        &mut machine,
        crate::ddraw::Provider {
            dc: crate::interop::MemoryDc {
                create: |_, _, _| unreachable!(),
                flush: |_, _, _, _| unreachable!(),
                release: |_, _, _| unreachable!(),
                discard: |_, _| unreachable!(),
            },
            memory: |_| unreachable!(),
            window: |_, _, _| None,
            submitted: |_, _, _, _| (),
            completed: |_, _| None,
            released: |_, _| (),
            vsync: |_| None,
        },
    )
    .unwrap();
    let (chain, device, context, frames) = presented_device(&mut machine);
    machine
        .ext
        .insert(SurfaceStore::new(32, 24, 0x500000, 65536, Snapshot::default()).unwrap());
    let gamma = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 16384, Prot::RW)
        .unwrap();
    assert_eq!(method(&mut machine, chain, 15, &[OUT]), 0);
    let output = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, output, 13, &[gamma]), 0x887a0001);
    assert_eq!(method(&mut machine, output, 11, &[0, 1]), 0);
    assert_eq!(method(&mut machine, output, 11, &[device, 0]), 0);
    assert_eq!(method(&mut machine, output, 11, &[device, 1]), 0);
    assert_eq!(method(&mut machine, output, 13, &[gamma]), 0x887a0001);
    assert_eq!(method(&mut machine, output, 11, &[device, 0]), 0);
    assert_eq!(method(&mut machine, chain, 10, &[1, output]), 0x887a0022);
    assert_eq!(method(&mut machine, output, 12, &[]), 0);
    assert_eq!(method(&mut machine, chain, 10, &[1, output]), 0);

    machine
        .cpu
        .write(gamma, &[0xcc; size_of::<DXGI_GAMMA_CONTROL>()])
        .unwrap();
    assert_eq!(method(&mut machine, output, 13, &[gamma]), 0);
    let caps: DXGI_GAMMA_CONTROL_CAPABILITIES = GuestPtr::new(gamma).read(&machine.cpu).unwrap();
    assert_eq!(caps.ScaleAndOffsetSupported, 0);
    assert_eq!((caps.MinConvertedValue, caps.MaxConvertedValue), (0., 1.));
    assert_eq!(caps.NumGammaControlPoints, 256);
    for (i, &position) in caps.ControlPointPositions[..256].iter().enumerate() {
        assert_eq!(position, i as f32 / 255.0);
    }
    assert!(
        caps.ControlPointPositions[256..]
            .iter()
            .all(|point| point.to_bits() == 0xcccc_cccc)
    );
    assert_eq!(method(&mut machine, output, 15, &[gamma]), 0x887a0001);
    let mut programmed = DXGI_GAMMA_CONTROL {
        Scale: DXGI_RGB {
            Red: 2.,
            Green: 3.,
            Blue: 4.,
        },
        Offset: DXGI_RGB {
            Red: 0.25,
            Green: 0.5,
            Blue: 0.75,
        },
        ..Default::default()
    };
    for (i, point) in programmed.GammaCurve[..256].iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let term = t * (1.0 - t) / 8.0;
        *point = DXGI_RGB {
            Red: t + term,
            Green: t,
            Blue: t - term,
        };
    }
    let tail = DXGI_RGB {
        Red: 0.125,
        Green: 0.25,
        Blue: 0.5,
    };
    programmed.GammaCurve[256] = tail;
    programmed.GammaCurve[1024] = tail;
    write_value(&mut machine, gamma, programmed);
    assert_eq!(method(&mut machine, output, 14, &[gamma]), 0);
    machine
        .cpu
        .write(gamma, &[0xcc; size_of::<DXGI_GAMMA_CONTROL>()])
        .unwrap();
    assert_eq!(method(&mut machine, output, 15, &[gamma]), 0);
    let stored: DXGI_GAMMA_CONTROL = GuestPtr::new(gamma).read(&machine.cpu).unwrap();
    assert_eq!(bytemuck::bytes_of(&stored), bytemuck::bytes_of(&programmed));
    let ramp = DXGI_GAMMA_CONTROL {
        GammaCurve: std::array::from_fn(|i| DXGI_RGB {
            Red: 0.0,
            Green: if i == 255 { 1.0 } else { 0.0 },
            Blue: 0.0,
        }),
        ..Default::default()
    };
    write_value(&mut machine, gamma, ramp);
    assert_eq!(method(&mut machine, output, 14, &[gamma]), 0);
    assert_eq!(method(&mut machine, output, 15, &[gamma]), 0);
    let stored: DXGI_GAMMA_CONTROL = GuestPtr::new(gamma).read(&machine.cpu).unwrap();
    assert_eq!(stored.GammaCurve[255].Green, 1.0);

    machine.cpu.write(IID, &IID_ID3D11TEXTURE2D).unwrap();
    assert_eq!(method(&mut machine, chain, 9, &[0, IID, OUT]), 0);
    let buffer = machine.cpu.read_u32(OUT).unwrap();
    let rtv = create_object(&mut machine, device, 9, &[buffer, 0]);
    write_value(&mut machine, DESCRIPTOR, [1.0f32, 1.0, 0.0, 1.0]);
    assert_eq!(method(&mut machine, context, 50, &[rtv, DESCRIPTOR]), 0);
    machine.cpu.write(IID, &IID_IDXGISURFACE).unwrap();
    assert_eq!(method(&mut machine, buffer, 0, &[IID, OUT]), 0);
    let surface = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, output, 16, &[surface]), 0);
    let frame = frames
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    assert_eq!(frame.pixels.bgra[0..4], [0, 255, 0, 255]);

    let staging = texture(&mut machine, device, 3, 0, 0x30000, 0, 2);
    machine.cpu.write(IID, &IID_IDXGISURFACE).unwrap();
    assert_eq!(method(&mut machine, staging, 0, &[IID, OUT]), 0);
    let surface = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut machine, output, 17, &[surface]), 0);
    let mapped = map(&mut machine, context, staging, 1);
    assert_eq!(machine.cpu.read_u32(mapped.pData).unwrap(), 0xff00ff00);
    assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);

    assert_eq!(method(&mut machine, output, 22, &[device, OUT]), 0);
    let duplicate = machine.cpu.read_u32(OUT).unwrap();
    check_vtable(
        &mut machine,
        duplicate,
        "dxgi.dll",
        "IDXGIOutputDuplication",
    );
    machine.cpu.write(IID, &IID_IDXGIOUTPUTDUPLICATION).unwrap();
    assert_eq!(method(&mut machine, duplicate, 0, &[IID, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), duplicate);
    assert_eq!(method(&mut machine, duplicate, 2, &[]), 1);
    assert_eq!(method(&mut machine, output, 22, &[device, OUT]), 0x80070057);
    assert_eq!(method(&mut machine, duplicate, 7, &[DESCRIPTOR]), 0);
    let desc: DXGI_OUTDUPL_DESC = GuestPtr::new(DESCRIPTOR).read(&machine.cpu).unwrap();
    assert_eq!(
        (
            desc.ModeDesc.Width,
            desc.ModeDesc.Height,
            desc.ModeDesc.Format
        ),
        (32, 24, 87)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let status = method(&mut machine, duplicate, 8, &[0, DESCRIPTOR, OUT]);
        if status == 0 {
            break;
        }
        assert_eq!(status, 0x887a0027);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let resource = machine.cpu.read_u32(OUT).unwrap();
    let info: DXGI_OUTDUPL_FRAME_INFO = GuestPtr::new(DESCRIPTOR).read(&machine.cpu).unwrap();
    assert_eq!(info.TotalMetadataBufferSize, 16);
    assert_eq!(
        method(&mut machine, duplicate, 8, &[0, DESCRIPTOR, OUT]),
        0x887a0001
    );
    machine.cpu.write_u32(OUT, 0x1234).unwrap();
    for slot in [9, 10, 11] {
        let mut args = vec![0, 0, OUT];
        if slot == 11 {
            args.push(DESCRIPTOR);
        }
        assert_eq!(method(&mut machine, duplicate, slot, &args), 0x80070057);
        assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0x1234);
    }
    machine.cpu.write_u32(ARRAY, 0xcccc_cccc).unwrap();
    assert_eq!(
        method(&mut machine, duplicate, 9, &[0, ARRAY, OUT]),
        0x887a0003
    );
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 16);
    assert_eq!(machine.cpu.read_u32(ARRAY).unwrap(), 0xcccc_cccc);
    assert_eq!(method(&mut machine, duplicate, 10, &[0, ARRAY, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(
        method(&mut machine, duplicate, 11, &[0, ARRAY, OUT, DESCRIPTOR]),
        0
    );
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(ARRAY).unwrap(), 0xcccc_cccc);
    assert_eq!(method(&mut machine, duplicate, 14, &[]), 0);
    assert_eq!(
        method(&mut machine, duplicate, 8, &[0, DESCRIPTOR, OUT]),
        0x887a0027
    );
    assert_eq!(method(&mut machine, chain, 10, &[0, 0]), 0);
    assert_eq!(method(&mut machine, output, 15, &[gamma]), 0x887a0001);
    assert_eq!(
        method(&mut machine, duplicate, 8, &[0, DESCRIPTOR, OUT]),
        0x887a0026
    );
    assert_eq!(method(&mut machine, duplicate, 2, &[]), 0);
    write_value(
        &mut machine,
        DESCRIPTOR,
        D3D11_TEXTURE2D_DESC {
            Width: 32,
            Height: 24,
            ArraySize: 1,
            MipLevels: 1,
            Format: 87,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut machine, device, 5, &[DESCRIPTOR, 0]);
    assert_eq!(method(&mut machine, context, 47, &[staging, resource]), 0);
    let mapped = map(&mut machine, context, staging, 1);
    assert_eq!(machine.cpu.read_u32(mapped.pData).unwrap(), 0xff00ff00);
    assert_eq!(method(&mut machine, context, 15, &[staging, 0]), 0);
    assert_eq!(method(&mut machine, resource, 2, &[]), 0);
    write_value(&mut machine, ARRAY, [87i32]);
    assert_eq!(
        method(&mut machine, output, 26, &[device, 0, 1, ARRAY, OUT]),
        0
    );
}

// spec: GFX-DXGI-2 GFX-DXGI-5
#[test]
fn guest_win32_factory_creates_a_windowless_composition_chain() {
    use adv_abi::types::DXGI_SWAP_CHAIN_DESC1;
    let mut machine = machine();
    let (chain, device, _, frames) = presented_device(&mut machine);
    let (software, _) = create(&mut machine, 5, 0, &[0xb000]);
    machine.cpu.write(IID, &IID_IDXGIFACTORY2).unwrap();
    assert_eq!(method(&mut machine, chain, 6, &[IID, OUT]), 0);
    let factory = machine.cpu.read_u32(OUT).unwrap();
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: 32,
        Height: 24,
        Format: 28,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: 32,
        BufferCount: 2,
        SwapEffect: 3,
        ..Default::default()
    };
    write_value(&mut machine, DESCRIPTOR, desc);
    assert_eq!(
        method(
            &mut machine,
            factory,
            16,
            &[device, device, DESCRIPTOR, 0, OUT]
        ),
        0x887a0001
    );
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 0);
    for adapter in [device, software] {
        assert_eq!(
            method(&mut machine, factory, 24, &[adapter, DESCRIPTOR, 0, OUT]),
            0
        );
        let composition = machine.cpu.read_u32(OUT).unwrap();
        assert_eq!(method(&mut machine, composition, 20, &[OUT]), 0x887a0001);
        assert_eq!(method(&mut machine, composition, 15, &[OUT]), 0x887a0001);
        assert_eq!(method(&mut machine, composition, 8, &[0, 0]), 0);
    }
    assert!(frames.try_recv().is_err());
}
