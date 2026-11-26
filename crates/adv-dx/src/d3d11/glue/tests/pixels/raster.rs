use super::*;

mod sampling;

const FIXTURE: &str = include_str!("../pixels.tsv");
const CONVERSION: [f32; 10] = [
    0.5,
    0.49999,
    127.5 / 255.,
    127.49 / 255.,
    127.51 / 255.,
    0.5 / 255.,
    -0.25,
    2.,
    f32::NAN,
    1e-9,
];

struct Raster {
    machine: Machine<JitCpu>,
    device: u32,
    context: u32,
    constants: u32,
}

impl Raster {
    fn new() -> Self {
        let mut machine = machine();
        let gpu = Gpu::request(!cfg!(target_vendor = "apple")).unwrap();
        let samples = gpu
            .device()
            .caps()
            .format_sample_counts(adv_gpu::Format::Rgba8Unorm);
        let level = FeatureLevel::L11_0;
        machine.ext.get_mut::<State>().devices.reports[level.column()] = Some(Arc::new(
            crate::d3d11::caps::tests::fixture_report_samples(level, samples | 7),
        ));
        machine.ext.get_mut::<State>().execution = Some(gpu::Execution::new(gpu));
        machine.cpu.map(CODE_BYTES, 4096, Prot::RW).unwrap();
        let (device, context) = create(&mut machine, 1, 0, &[0xb000]);
        let constants = buffer(&mut machine, device, Bind::CONSTANT, 16);
        let mut raster = Self {
            machine,
            device,
            context,
            constants,
        };
        raster.shader(
            12,
            11,
            include_bytes!("../../../../../tests/fixtures/d3d11/guest_fullscreen.dxbc"),
        );
        let state = raster.object(
            22,
            D3D11_RASTERIZER_DESC {
                FillMode: 3,
                CullMode: 1,
                DepthClipEnable: 1,
                ScissorEnable: 1,
                ..Default::default()
            },
        );
        write_value(&mut raster.machine, ARRAY, constants);
        raster.call(16, &[0, 1, ARRAY]);
        raster.call(24, &[4]);
        raster.call(43, &[state]);
        raster
    }

    fn call(&mut self, slot: u32, args: &[u32]) {
        assert_eq!(
            method(&mut self.machine, self.context, slot, args),
            0,
            "context slot {slot}"
        );
    }

    fn object<T: Pod>(&mut self, slot: u32, desc: T) -> u32 {
        write_value(&mut self.machine, DESCRIPTOR, desc);
        create_object(&mut self.machine, self.device, slot, &[DESCRIPTOR])
    }

    fn shader(&mut self, create: u32, bind: u32, bytes: &[u8]) {
        self.machine.cpu.write(CODE_BYTES, bytes).unwrap();
        let shader = create_object(
            &mut self.machine,
            self.device,
            create,
            &[CODE_BYTES, bytes.len() as u32, 0],
        );
        self.call(bind, &[shader, 0, 0]);
        method(&mut self.machine, shader, 2, &[]);
    }

    fn texture(&mut self, width: u32, height: u32, format: i32, bind: u32, initial: u32) -> u32 {
        self.image(
            D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: if bind == 0 { 3 } else { 0 },
                BindFlags: bind,
                CPUAccessFlags: if bind == 0 { 0x20000 } else { 0 },
                ..Default::default()
            },
            initial,
        )
    }

    fn image(&mut self, desc: D3D11_TEXTURE2D_DESC, initial: u32) -> u32 {
        write_value(&mut self.machine, DESCRIPTOR, desc);
        create_object(&mut self.machine, self.device, 5, &[DESCRIPTOR, initial])
    }

    fn target(&mut self, width: u32, format: i32) -> (u32, u32, u32) {
        let target = self.texture(width, 1, format, Bind::RENDER_TARGET, 0);
        let view = create_object(&mut self.machine, self.device, 9, &[target, 0]);
        let staging = self.texture(width, 1, format, 0, 0);
        self.targets(width, &[view]);
        (target, view, staging)
    }

    fn targets(&mut self, width: u32, views: &[u32]) {
        self.machine
            .cpu
            .write(ARRAY, bytemuck::cast_slice(views))
            .unwrap();
        self.call(33, &[views.len() as u32, ARRAY, 0]);
        write_value(
            &mut self.machine,
            DESCRIPTOR,
            D3D11_VIEWPORT {
                Width: width as f32,
                Height: 1.,
                MaxDepth: 1.,
                ..Default::default()
            },
        );
        self.call(44, &[1, DESCRIPTOR]);
    }

    fn clear(&mut self, view: u32, color: [f32; 4]) {
        write_value(&mut self.machine, DESCRIPTOR, color);
        self.call(50, &[view, DESCRIPTOR]);
    }

    fn draw(&mut self, pixels: std::ops::Range<i32>, color: [f32; 4]) {
        update_dynamic(&mut self.machine, self.context, self.constants, &color);
        write_value(
            &mut self.machine,
            DESCRIPTOR,
            [pixels.start, 0, pixels.end, 1],
        );
        self.call(45, &[1, DESCRIPTOR]);
        self.call(13, &[3, 0]);
    }

    fn read(&mut self, resource: u32, staging: u32, count: usize, unit: usize) -> Vec<u32> {
        self.call(47, &[staging, resource]);
        let mapped = map(&mut self.machine, self.context, staging, 1);
        let mut bytes = vec![0; count * unit];
        self.machine.cpu.read(mapped.pData, &mut bytes).unwrap();
        self.call(15, &[staging, 0]);
        bytes
            .chunks_exact(unit)
            .map(|texel| {
                let mut word = [0; 4];
                word[..unit].copy_from_slice(texel);
                u32::from_le_bytes(word)
            })
            .collect()
    }
}

fn row(driver: &str, name: &str, format: i32) -> Vec<u32> {
    let label = format!("{driver}:{name}");
    let line = FIXTURE
        .lines()
        .find_map(|line| {
            let (key, data) = line.split_once('\t')?;
            (key == label).then_some(data)
        })
        .unwrap();
    line.split('\t')
        .skip(1)
        .map(|field| {
            let raw = u32::from_str_radix(field, 16).unwrap();
            if matches!(format, 28 | 29) {
                raw.swap_bytes()
            } else {
                raw
            }
        })
        .collect()
}

fn match_row(name: &str, format: i32, actual: &[u32]) {
    let hw = row("hw", name, format);
    let warp = row("warp", name, format);
    assert_eq!(actual.len(), hw.len(), "{name}");
    for (index, (&actual, (&hw, &warp))) in actual.iter().zip(hw.iter().zip(&warp)).enumerate() {
        let good = if name.starts_with("filter") {
            actual
                .to_le_bytes()
                .into_iter()
                .zip(warp.to_le_bytes())
                .all(|(a, b)| a.abs_diff(b) <= 1)
        } else if name.starts_with("blend") && format == 85 && index < 5 {
            [(0, 31), (5, 63), (11, 31)]
                .into_iter()
                .all(|(shift, mask)| {
                    ((actual >> shift) & mask).abs_diff((warp >> shift) & mask) <= 1
                })
        } else if name.starts_with("convert") && index == 8 {
            match format {
                26 => {
                    [0x7c0, 0x7c0 << 11, 0x3e0 << 22]
                        .into_iter()
                        .all(|exponent| actual & exponent == exponent)
                        && [0x3f, 0x3f << 11, 0x1f << 22]
                            .into_iter()
                            .all(|mantissa| actual & mantissa != 0)
                }
                54 => (actual & 0x7c00) == 0x7c00 && (actual & 0x3ff) != 0,
                _ => actual == hw,
            }
        } else if (name.starts_with("convert") || name.starts_with("clear"))
            && matches!(format, 24 | 28 | 29 | 56 | 63 | 85)
        {
            let value = if name.starts_with("clear") {
                CONVERSION[2]
            } else {
                CONVERSION[index]
            };
            normalized_conversion(format, value, actual)
        } else if matches!(format, 28 | 29) {
            actual
                .to_le_bytes()
                .into_iter()
                .zip(hw.to_le_bytes().into_iter().zip(warp.to_le_bytes()))
                .all(|(a, (h, w))| a == h || a == w)
        } else {
            actual == hw || actual == warp
        };
        assert!(
            good,
            "{name}[{index}] = {actual:08x}, hardware {hw:08x}, WARP {warp:08x}"
        );
    }
}

fn normalized_conversion(format: i32, value: f32, mut actual: u32) -> bool {
    let widths: &[u32] = match format {
        24 => &[10, 10, 10, 2],
        28 | 29 => &[8; 4],
        56 => &[16],
        63 => &[8],
        85 => &[5, 6, 5],
        _ => unreachable!(),
    };
    widths.iter().enumerate().all(|(channel, &width)| {
        let mask = (1u32 << width) - 1;
        let code = actual & mask;
        actual >>= width;
        let signed = format == 63;
        let value = f64::from(value).clamp(if signed { -1. } else { 0. }, 1.);
        let value = if format == 29 && channel < 3 {
            if value <= 0.0031308 {
                12.92 * value
            } else {
                1.055 * value.powf(1. / 2.4) - 0.055
            }
        } else {
            value
        };
        let (actual, maximum) = if signed {
            (f64::from(code as i8), f64::from(mask >> 1))
        } else {
            (f64::from(code), f64::from(mask))
        };
        (actual - maximum * value).abs() <= 0.6
    })
}

#[test]
fn filter_ramp_and_format_conversions_use_guest_draw_and_clear() {
    let mut r = Raster::new();
    let (target, _, staging) = r.target(64, 28);
    write_value(&mut r.machine, ARRAY, [0xff000000u32, u32::MAX]);
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: ARRAY,
            SysMemPitch: 8,
            SysMemSlicePitch: 8,
        },
    );
    let texture = r.texture(2, 1, 28, Bind::SHADER, INIT);
    let srv = create_object(&mut r.machine, r.device, 7, &[texture, 0]);
    let sampler = r.object(
        23,
        D3D11_SAMPLER_DESC {
            Filter: 0x15,
            AddressU: 3,
            AddressV: 3,
            AddressW: 3,
            ComparisonFunc: 1,
            MaxLOD: f32::MAX,
            ..Default::default()
        },
    );
    write_value(&mut r.machine, ARRAY, [srv, sampler]);
    r.call(8, &[0, 1, ARRAY]);
    r.call(10, &[0, 1, ARRAY + 4]);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_ramp.dxbc"),
    );
    r.draw(0..64, [0.; 4]);
    match_row("filter-ramp-64-steps", 28, &r.read(target, staging, 64, 4));

    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_color.dxbc"),
    );
    for (name, format, unit) in [
        ("rgba8", 28, 4),
        ("rgba8-srgb", 29, 4),
        ("r16unorm", 56, 2),
        ("r8snorm", 63, 1),
        ("b5g6r5", 85, 2),
        ("rgb10a2", 24, 4),
        ("r11g11b10f", 26, 4),
        ("r16f", 54, 2),
    ] {
        if !gpu::caps(&r.machine.ext)
            .unwrap()
            .format(crate::d3d11::resource::native_format(format as u32).unwrap())
            .contains(adv_gpu::FormatCaps::COLOR)
        {
            continue;
        }
        let (target, view, staging) = r.target(10, format);
        r.clear(view, [0.; 4]);
        for (pixel, value) in CONVERSION.into_iter().enumerate() {
            let pixel = pixel as i32;
            r.draw(pixel..pixel + 1, [value; 4]);
        }
        match_row(
            &format!("convert-{name}"),
            format,
            &r.read(target, staging, 10, unit),
        );
        r.clear(view, [127.5 / 255.; 4]);
        match_row(
            &format!("clear-127.5-{name}"),
            format,
            &r.read(target, staging, 1, unit),
        );
    }
}

#[test]
fn sparse_render_targets_keep_guest_slots() {
    let mut r = Raster::new();
    let (first, first_view, first_staging) = r.target(1, 28);
    let (third, third_view, third_staging) = r.target(1, 28);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_sparse.dxbc"),
    );
    r.targets(1, &[first_view, 0, third_view]);
    r.draw(0..1, [0.; 4]);
    assert_eq!(r.read(first, first_staging, 1, 4), [0xff0000ff]);
    assert_eq!(r.read(third, third_staging, 1, 4), [0xff00ff00]);
}

#[test]
fn windows_alpha_and_destination_blending_use_guest_state() {
    let mut r = Raster::new();
    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_color.dxbc"),
    );
    let mut blend = D3D11_BLEND_DESC::default();
    blend.RenderTarget[0] = adv_abi::types::D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: 1,
        SrcBlend: 5,
        DestBlend: 6,
        BlendOp: 1,
        SrcBlendAlpha: 2,
        DestBlendAlpha: 1,
        BlendOpAlpha: 1,
        RenderTargetWriteMask: 15,
        ..Default::default()
    };
    let over = r.object(20, blend);
    blend.RenderTarget[0].SrcBlend = 9;
    blend.RenderTarget[0].DestBlend = 1;
    let multiply = r.object(20, blend);
    for (name, format, unit) in [("rgba8", 28, 4), ("rgba8-srgb", 29, 4), ("b5g6r5", 85, 2)] {
        let caps = gpu::caps(&r.machine.ext).unwrap();
        if !caps
            .format(crate::d3d11::resource::native_format(format as u32).unwrap())
            .contains(adv_gpu::FormatCaps::BLEND)
        {
            continue;
        }
        let (target, view, staging) = r.target(6, format);
        r.clear(view, [64. / 255., 64. / 255., 64. / 255., 1.]);
        r.call(35, &[over, 0, u32::MAX]);
        for (i, alpha) in [128. / 255., 127. / 255., 1. / 255., 0.5]
            .into_iter()
            .enumerate()
        {
            let i = i as i32;
            r.draw(i..i + 1, [1., 1., 1., alpha]);
        }
        r.call(35, &[multiply, 0, u32::MAX]);
        r.draw(4..5, [128. / 255., 128. / 255., 128. / 255., 1.]);
        match_row(
            &format!("blend-{name}-a128-a127-a1-a0.5-over-64-then-destcolor-128"),
            format,
            &r.read(target, staging, 6, unit),
        );
    }
}

#[test]
fn windows_multisample_coverage_uses_guest_resolve() {
    let mut r = Raster::new();
    let vs = include_bytes!("../../../../../tests/fixtures/shader-core/vs_5_0.dxbc");
    r.shader(12, 11, vs);
    r.machine.cpu.write(ARRAY, b"POSITION\0TEXCOORD\0").unwrap();
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
    r.machine
        .cpu
        .write(DESCRIPTOR, bytemuck::cast_slice(&elements))
        .unwrap();
    let layout = create_object(
        &mut r.machine,
        r.device,
        11,
        &[DESCRIPTOR, 2, CODE_BYTES, vs.len() as u32],
    );
    r.call(17, &[layout]);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_color.dxbc"),
    );
    let vb = buffer(&mut r.machine, r.device, Bind::VERTEX, 34 * 6 * 24);
    let vs_constants = buffer(&mut r.machine, r.device, Bind::CONSTANT, 32);
    let mut vertices = Vec::new();
    for k in 0..17 {
        let k = k as f32;
        for [l, t, right, b] in [
            [k + k / 16., 0., k + 1., 1.],
            [0., 2. + k + k / 16., 1., 3. + k],
        ] {
            let p = |x, y| [x, y, 0.5, 1., 0., 0.];
            vertices.extend([
                p(l, t),
                p(right, t),
                p(l, b),
                p(l, b),
                p(right, t),
                p(right, b),
            ]);
        }
    }
    update_dynamic(&mut r.machine, r.context, vb, &vertices);
    update_dynamic(
        &mut r.machine,
        r.context,
        vs_constants,
        &[2.0f32 / 17., -2.0 / 19., 1., 1., -1., 1., 0., 0.],
    );
    update_dynamic(&mut r.machine, r.context, r.constants, &[1.0f32; 4]);
    write_value(&mut r.machine, ARRAY + 16, [vb, 24, 0, vs_constants]);
    r.call(18, &[0, 1, ARRAY + 16, ARRAY + 20, ARRAY + 24]);
    r.call(7, &[0, 1, ARRAY + 28]);
    let resolved = r.texture(17, 19, 28, Bind::RENDER_TARGET, 0);
    let staging = r.texture(17, 19, 28, 0, 0);
    for samples in [4, 8] {
        let caps = gpu::caps(&r.machine.ext).unwrap();
        if caps.format_sample_counts(adv_gpu::Format::Rgba8Unorm) & samples == 0 {
            continue;
        }
        let image = r.image(
            D3D11_TEXTURE2D_DESC {
                Width: 17,
                Height: 19,
                MipLevels: 1,
                ArraySize: 1,
                Format: 28,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: samples,
                    Quality: 0,
                },
                BindFlags: Bind::RENDER_TARGET,
                ..Default::default()
            },
            0,
        );
        let view = create_object(&mut r.machine, r.device, 9, &[image, 0]);
        r.targets(17, &[view]);
        write_value(
            &mut r.machine,
            DESCRIPTOR,
            D3D11_VIEWPORT {
                Width: 17.,
                Height: 19.,
                MaxDepth: 1.,
                ..Default::default()
            },
        );
        r.call(44, &[1, DESCRIPTOR]);
        write_value(&mut r.machine, DESCRIPTOR, [0i32, 0, 17, 19]);
        r.call(45, &[1, DESCRIPTOR]);
        r.clear(view, [0.; 4]);
        r.call(13, &[vertices.len() as u32, 0]);
        r.call(57, &[resolved, 0, image, 0, 28]);
        r.call(47, &[staging, resolved]);
        let map = map(&mut r.machine, r.context, staging, 1);
        let mut x = Vec::new();
        let mut y = Vec::new();
        for k in 0..17 {
            x.push(r.machine.cpu.read_u32(map.pData + k * 4).unwrap());
            y.push(u32::from(
                r.machine
                    .cpu
                    .read_u8(map.pData + (k + 2) * map.RowPitch)
                    .unwrap(),
            ));
        }
        r.call(15, &[staging, 0]);
        match_row(&format!("msaa{samples}-xsweep-row0"), 28, &x);
        match_row(&format!("msaa{samples}-ysweep-col0"), 0, &y);
    }
}

#[test]
fn typed_uav_clear_preserves_subword_buffer_edges_and_image_bits() {
    let mut r = Raster::new();
    let data = [0x88776655u32, 0x44332211];
    write_value(&mut r.machine, ARRAY, data);
    write_value(
        &mut r.machine,
        INIT,
        D3D11_SUBRESOURCE_DATA {
            pSysMem: ARRAY,
            ..Default::default()
        },
    );
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 8,
            BindFlags: Bind::UNORDERED,
            ..Default::default()
        },
    );
    let buffer = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, INIT]);
    let desc = adv_abi::types::D3D11_UNORDERED_ACCESS_VIEW_DESC {
        Format: 50,
        ViewDimension: 1,
        Anonymous: bytemuck::cast([1u32, 2, 0]),
    };
    write_value(&mut r.machine, DESCRIPTOR, desc);
    let view = create_object(&mut r.machine, r.device, 8, &[buffer, DESCRIPTOR]);
    write_value(&mut r.machine, DESCRIPTOR, [0x1aau32, 0x1bb, 0x2cc, 0x2dd]);
    r.call(51, &[view, DESCRIPTOR]);
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 8,
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    assert_eq!(r.read(buffer, staging, 2, 4), [0xbbaa6655, 0x4433bbaa]);

    let image = r.texture(3, 1, 41, Bind::UNORDERED, 0);
    let view = create_object(&mut r.machine, r.device, 8, &[image, 0]);
    let staging = r.texture(3, 1, 41, 0, 0);
    for (slot, value) in [(51, 0x7fc01234), (52, 0.375f32.to_bits())] {
        write_value(&mut r.machine, DESCRIPTOR, [value, 0, 0, 0]);
        r.call(slot, &[view, DESCRIPTOR]);
        assert_eq!(r.read(image, staging, 3, 4), [value; 3]);
    }
}

#[test]
fn legacy_shared_handle_keeps_cross_device_aliases_live() {
    let mut r = Raster::new();
    let (other, other_context) = create(&mut r.machine, 1, 0, &[0xb000]);
    let texture = r.image(
        D3D11_TEXTURE2D_DESC {
            Width: 2,
            Height: 1,
            MipLevels: 1,
            ArraySize: 1,
            Format: 28,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: Bind::RENDER_TARGET,
            MiscFlags: Misc::SHARED,
            ..Default::default()
        },
        0,
    );
    let view = create_object(&mut r.machine, r.device, 9, &[texture, 0]);
    r.clear(view, [1., 0., 0., 1.]);
    r.machine.cpu.write(IID, &IID_IDXGIRESOURCE).unwrap();
    assert_eq!(method(&mut r.machine, texture, 0, &[IID, OUT]), 0);
    let dxgi = r.machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(method(&mut r.machine, dxgi, 8, &[OUT]), 0);
    let handle = r.machine.cpu.read_u32(OUT).unwrap();
    assert_ne!(handle, 0);
    r.machine.cpu.write(IID, &IID_ID3D11TEXTURE2D).unwrap();
    assert_eq!(method(&mut r.machine, other, 28, &[handle, IID, OUT]), 0);
    let alias = r.machine.cpu.read_u32(OUT).unwrap();
    assert_ne!(alias, texture);
    for object in [view, dxgi, texture] {
        method(&mut r.machine, object, 2, &[]);
    }
    r.device = other;
    r.context = other_context;
    let staging = r.texture(2, 1, 28, 0, 0);
    assert_eq!(r.read(alias, staging, 2, 4), [0xff0000ff; 2]);
    assert_eq!(method(&mut r.machine, alias, 2, &[]), 0);
    assert_eq!(
        method(&mut r.machine, other, 28, &[handle, IID, OUT]),
        0x80070057
    );
    assert_eq!(r.machine.cpu.read_u32(OUT).unwrap(), 0);
}

// spec: GFX-STAGE-1
#[test]
fn geometry_stream_output_appends_and_drives_draw_auto_through_guest_vtables() {
    use adv_abi::types::D3D11_SO_DECLARATION_ENTRY;
    let mut r = Raster::new();
    let gs = include_bytes!("../../../../../tests/fixtures/shader-core/gs_5_0.dxbc");
    r.machine.cpu.write(CODE_BYTES, gs).unwrap();
    r.machine.cpu.write(ARRAY, b"SV_Position\0").unwrap();
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_SO_DECLARATION_ENTRY {
            SemanticName: ARRAY,
            ComponentCount: 4,
            ..Default::default()
        },
    );
    write_value(&mut r.machine, ARRAY + 16, 16u32);
    let shader = create_object(
        &mut r.machine,
        r.device,
        14,
        &[
            CODE_BYTES,
            gs.len() as u32,
            DESCRIPTOR,
            1,
            ARRAY + 16,
            1,
            u32::MAX,
            0,
        ],
    );
    r.call(23, &[shader, 0, 0]);
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 112,
            BindFlags: Bind::STREAM_OUTPUT | Bind::VERTEX,
            ..Default::default()
        },
    );
    let output = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    write_value(&mut r.machine, ARRAY, [output, 16]);
    r.call(37, &[1, ARRAY, ARRAY + 4]);
    r.call(13, &[3, 0]);
    write_value(&mut r.machine, ARRAY + 4, u32::MAX);
    r.call(37, &[1, ARRAY, ARRAY + 4]);
    r.call(13, &[3, 0]);
    r.call(37, &[0, 0, 0]);
    r.call(23, &[0, 0, 0]);

    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 112,
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    let actual = r.read(output, staging, 28, 4);
    let expected = [-1.0f32, 1., 0.5, 1., 3., 1., 0.5, 1., -1., -3., 0.5, 1.].map(f32::to_bits);
    assert_eq!(&actual[4..16], &expected);
    assert_eq!(&actual[16..], &expected);

    let vs = include_bytes!("../../../../../tests/fixtures/shader-core/vs_5_0.dxbc");
    r.shader(12, 11, vs);
    r.machine.cpu.write(ARRAY, b"POSITION\0TEXCOORD\0").unwrap();
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: ARRAY,
                Format: 2,
                ..Default::default()
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: ARRAY + 9,
                Format: 16,
                ..Default::default()
            },
        ],
    );
    let layout = create_object(
        &mut r.machine,
        r.device,
        11,
        &[DESCRIPTOR, 2, CODE_BYTES, vs.len() as u32],
    );
    r.call(17, &[layout]);
    write_value(&mut r.machine, ARRAY, [output, 16, 16]);
    r.call(18, &[0, 1, ARRAY, ARRAY + 4, ARRAY + 8]);
    let transform = buffer(&mut r.machine, r.device, Bind::CONSTANT, 32);
    update_dynamic(
        &mut r.machine,
        r.context,
        transform,
        &[1.0f32, 1., 1., 1., 0., 0., 0., 0.],
    );
    write_value(&mut r.machine, ARRAY, transform);
    r.call(7, &[0, 1, ARRAY]);
    r.shader(
        15,
        9,
        include_bytes!("../../../../../tests/fixtures/d3d11/guest_color.dxbc"),
    );
    let (target, view, staging) = r.target(4, 28);
    r.clear(view, [0., 0., 0., 1.]);
    update_dynamic(
        &mut r.machine,
        r.context,
        r.constants,
        &[1.0f32, 0., 0., 1.],
    );
    write_value(&mut r.machine, DESCRIPTOR, [0i32, 0, 4, 1]);
    r.call(45, &[1, DESCRIPTOR]);
    r.call(38, &[]);
    assert_eq!(r.read(target, staging, 4, 4), [0xff0000ff; 4]);
}

// spec: GFX-STAGE-2
#[test]
fn shader_dispatch_indirect_consumes_recorded_group_counts() {
    let mut r = Raster::new();
    r.shader(
        18,
        69,
        include_bytes!("../../../../../tests/fixtures/shader-core/cs_5_0.dxbc"),
    );
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 64,
            BindFlags: Bind::UNORDERED,
            MiscFlags: Misc::STRUCTURED,
            StructureByteStride: 4,
            ..Default::default()
        },
    );
    let output = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    let view = create_object(&mut r.machine, r.device, 8, &[output, 0]);
    write_value(&mut r.machine, ARRAY, view);
    r.call(68, &[0, 1, ARRAY, 0]);
    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 16,
            MiscFlags: Misc::INDIRECT,
            ..Default::default()
        },
    );
    let indirect = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    write_value(&mut r.machine, ARRAY, [99u32, 2, 1, 1]);
    r.call(48, &[indirect, 0, 0, ARRAY, 0, 0]);
    r.call(42, &[indirect, 4]);

    write_value(
        &mut r.machine,
        DESCRIPTOR,
        D3D11_BUFFER_DESC {
            ByteWidth: 64,
            Usage: 3,
            CPUAccessFlags: 0x20000,
            ..Default::default()
        },
    );
    let staging = create_object(&mut r.machine, r.device, 3, &[DESCRIPTOR, 0]);
    let expected: Vec<_> = (0..16).map(|id: u32| (id ^ 7).pow(2)).collect();
    assert_eq!(r.read(output, staging, 16, 4), expected);
}

// spec: GFX-STAGE-3
#[test]
fn class_instances_select_interface_bodies_through_guest_vtables() {
    let mut r = Raster::new();
    let linkage = create_object(&mut r.machine, r.device, 19, &[]);
    let bytes = include_bytes!("../../../../../tests/fixtures/shader-core/linkage.dxbc");
    r.machine.cpu.write(CODE_BYTES, bytes).unwrap();
    let shader = create_object(
        &mut r.machine,
        r.device,
        15,
        &[CODE_BYTES, bytes.len() as u32, linkage],
    );
    let (target, _, staging) = r.target(1, 2);
    for (name, expected) in [
        (b"Blue\0".as_slice(), [0.0f32, 0., 0.5, 1.]),
        (b"Red\0".as_slice(), [0.5f32, 0., 0., 1.]),
    ] {
        r.machine.cpu.write(ARRAY, name).unwrap();
        let instance = create_object(&mut r.machine, linkage, 8, &[ARRAY, 0, 0, 0, 0]);
        write_value(&mut r.machine, ARRAY, instance);
        r.call(9, &[shader, ARRAY, 1]);
        method(&mut r.machine, instance, 2, &[]);
        r.draw(0..1, [0.; 4]);
        assert_eq!(r.read(target, staging, 4, 4), expected.map(f32::to_bits));
    }
}
