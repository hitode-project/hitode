//! Speculative modules use the exact draw source builder. Unknown layouts are
//! provisional; declaration creation schedules its exact variant.
use super::*;
use adv_device::gpu::compile::Compile;

pub(in crate::d3d9) fn warm(
    env: &mut Env<'_>,
    pointer: u32,
    candidate: Option<(crate::shader::Stage, Id)>,
) {
    schedule(env, pointer, candidate, None);
}
pub(in crate::d3d9) fn declaration(env: &mut Env<'_>, pointer: u32, elements: &[[u8; 8]]) {
    schedule(env, pointer, None, Some(elements));
}
fn schedule(
    env: &mut Env<'_>,
    pointer: u32,
    candidate: Option<(crate::shader::Stage, Id)>,
    declaration: Option<&[[u8; 8]]>,
) {
    let Some(d) = device(env, pointer) else {
        return;
    };
    let mut ids = [d.shaders.vertex, d.shaders.pixel];
    if let Some((stage, id)) = candidate {
        ids[usize::from(stage == crate::shader::Stage::Pixel)] = Some(id);
    }
    if ids == [None, None] {
        return;
    }
    let stages = ids.map(|id| {
        id.and_then(|id| shaders::bound(env, id))
            .map(|s| (s.parsed.clone(), s.translation.clone()))
    });
    let inputs = if let Some((vs, _)) = &stages[0] {
        let result = match declaration {
            Some(elements) => declaration_inputs(elements, vs),
            None => super::inputs(env, pointer, vs),
        };
        result.unwrap_or_else(|_| {
            vs.inputs
                .iter()
                .filter(|i| i.register.kind == crate::shader::RegisterKind::Input)
                .map(|i| Input {
                    stream: 0,
                    offset: 0,
                    format: AttributeFormat::Float4,
                    register: u32::from(i.register.index),
                })
                .collect()
        })
    } else {
        Vec::new()
    };
    let key = ProgramKey {
        vertex: ids[0],
        pixel: ids[1],
        inputs: inputs.iter().map(|i| (i.register, i.format)).collect(),
        second: false,
        features: (d.fixed.render[&152], d.fixed.render[&28] != 0),
        fixed: fixed_features(&d.fixed, None),
    };
    // The first draw is predicted to match the last one (triangle lists
    // before any); raster conversion and attachments are the draw's own.
    let (kind, up_stride) = env
        .ext
        .get::<State>()
        .programs
        .last_draw
        .unwrap_or((4, None));
    let prediction = fixed::prepare_state(env, pointer, kind)
        .ok()
        .map(|(target, _, state)| {
            let textures = std::array::from_fn(|slot| texture(env, pointer, slot, &state).ok());
            (target, state, textures)
        });
    let streams: Vec<_> = (0..16)
        .filter_map(|n| {
            let attributes: Vec<_> = inputs
                .iter()
                .filter(|i| i.stream == n)
                .map(|i| Attribute {
                    location: i.register,
                    offset: i.offset,
                    format: i.format,
                })
                .collect();
            (!attributes.is_empty()).then(|| VertexStream {
                stride: up_stride
                    .filter(|_| n == 0)
                    .unwrap_or(d.buffers.streams[n].stride),
                attributes,
                instance: d.buffers.frequencies[0] & buffers::INDEXED_DATA != 0
                    && d.buffers.frequencies[n] & buffers::INSTANCE_DATA != 0,
                bytes: Vec::new().into(),
            })
        })
        .collect();
    let state = env.ext.get_mut::<State>();
    let Some(context) = state.gpu.as_ref().map(|gpu| {
        gpu.compile_handle(
            prediction
                .as_ref()
                .map(|(target, state, textures)| (target, state, textures)),
            &streams,
        )
    }) else {
        return;
    };
    let cache = &mut state.programs;
    if cache.warming.remove(&key).is_some() {
        let at = cache
            .warming_order
            .iter()
            .position(|(k, _)| k == &key)
            .unwrap();
        cache.warming_bytes -= cache.warming_order.remove(at).unwrap().1;
    }
    // Reserve the maximum result for each speculative variant. Larger programs
    // remain supported through the ordinary draw path, without speculative copies.
    const RESERVATION: usize = 512 * 1024;
    let bytes = RESERVATION
        + stages
            .iter()
            .flatten()
            .map(|(s, _)| s.bytecode().len() * 4 + crate::shader::MAX_TRANSLATION_BYTES)
            .sum::<usize>();
    while cache.warming.len() == 16 || cache.warming_bytes + bytes > 32 * 1024 * 1024 {
        let (oldest, bytes) = cache.warming_order.pop_front().unwrap();
        cache.warming.remove(&oldest);
        cache.warming_bytes -= bytes;
    }
    cache.warming_order.push_back((key.clone(), bytes));
    cache.warming_bytes += bytes;
    let features = (false, key.features.0, key.features.1, key.fixed);
    let job = Compile::spawn(bytes, cache.counters.clone(), move || {
        let translations = stages
            .each_ref()
            .map(|s| s.as_ref().map(|(_, t)| t.0.resolve().clone()).transpose())
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let mut slots = Vec::new();
        for (stage, t) in translations.iter().enumerate() {
            if let Some(t) = t {
                if t.samplers
                    .iter()
                    .any(|s| s.dimension != SamplerDimension::D2)
                {
                    return Err("unsupported warm sampler".into());
                }
                slots.extend(t.samplers.iter().map(|s| (stage == 0, s.index)));
            }
        }
        if stages[1].is_none() {
            slots.push((false, 0));
        }
        let source = source::build(
            stages[0].as_ref().map(|s| s.0.as_ref()),
            stages[1].as_ref().map(|s| s.0.as_ref()),
            translations[0].as_deref(),
            translations[1].as_deref(),
            &inputs,
            &slots,
            features,
        )
        .map_err(|_| "warm source unavailable")?;
        if source.len() > RESERVATION {
            return Err("speculative source budget exceeded".into());
        }
        let mut program = Program::new(source.into(), slots.iter().map(|s| s.0).collect());
        program.constants = std::array::from_fn(|n| {
            translations[n]
                .as_ref()
                .map_or(Default::default(), |t| t.constants)
        });
        context
            .prepare_program(program, &slots)
            .map_err(|e| e.to_string())
    });
    cache.warming.insert(key, job);
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::Guest;
    // spec: GFX-CACHE-7, GFX-CACHE-10
    #[test]
    fn creation_anticipates_the_first_matching_draw_without_a_new_compile() {
        use crate::d3d9::stretch::tests::Fixture;
        let mut f = Fixture::new();
        for (state, value) in [(7, 0), (22, 1)] {
            f.okay(
                "IDirect3DDevice9::SetRenderState",
                &[f.device, state, value],
            );
        }
        let output = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateVertexBuffer",
            &[f.device, 60, 0, 0, 1, output, 0],
        );
        let buffer = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DVertexBuffer9::Lock", &[buffer, 0, 0, output, 0]);
        f.m.cpu
            .write(
                f.m.cpu.read_u32(output).unwrap(),
                bytemuck::cast_slice(&[
                    [0f32, -1., -1., 0.5, 1.],
                    [0., 3., -1., 0.5, 1.],
                    [0., -1., 3., 0.5, 1.],
                ]),
            )
            .unwrap();
        f.okay("IDirect3DVertexBuffer9::Unlock", &[buffer]);
        f.okay(
            "IDirect3DDevice9::SetStreamSource",
            &[f.device, 0, buffer, 0, 20],
        );
        let vertex = f.packet([
            0xfffe0200u32,
            0x0200001f,
            0x80000000,
            0x900f0000,
            0x02000001,
            0xc00f0000,
            0x90e40000,
            0xffff,
        ]);
        f.okay(
            "IDirect3DDevice9::CreateVertexShader",
            &[f.device, vertex, output],
        );
        let vertex = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, vertex]);
        let pixel = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff]);
        f.okay(
            "IDirect3DDevice9::CreatePixelShader",
            &[f.device, pixel, output],
        );
        let pixel = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, pixel]);
        let elements = f.packet([[0u8, 0, 4, 0, 3, 0, 0, 0], [255, 0, 0, 0, 17, 0, 0, 0]]);
        f.okay(
            "IDirect3DDevice9::CreateVertexDeclaration",
            &[f.device, elements, output],
        );
        let declaration = f.m.cpu.read_u32(output).unwrap();
        f.okay(
            "IDirect3DDevice9::SetVertexDeclaration",
            &[f.device, declaration],
        );
        for job in f.m.ext.get::<State>().programs.warming.values() {
            job.resolve().as_ref().unwrap();
        }
        let before = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats();
        f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
        f.okay("IDirect3DDevice9::DrawPrimitive", &[f.device, 4, 0, 1]);
        let image = f.image(f.back);
        f.pixels(&image);
        let after = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats();
        assert_eq!(after.draws - before.draws, 1);
        assert_eq!(
            after.anticipated_pipeline_draws - before.anticipated_pipeline_draws,
            1
        );
        assert_eq!(after.prebuild_misses, before.prebuild_misses);
    }

    // spec: GFX-CACHE-7
    #[test]
    fn creation_predicts_the_last_draws_primitive_type_and_user_stride() {
        use crate::d3d9::stretch::tests::Fixture;
        let mut f = Fixture::new();
        for (state, value) in [(7, 0), (22, 1)] {
            f.okay(
                "IDirect3DDevice9::SetRenderState",
                &[f.device, state, value],
            );
        }
        let output = f.packet(0u32);
        let vertex = f.packet([
            0xfffe0200u32,
            0x0200001f,
            0x80000000,
            0x900f0000,
            0x02000001,
            0xc00f0000,
            0x90e40000,
            0xffff,
        ]);
        f.okay(
            "IDirect3DDevice9::CreateVertexShader",
            &[f.device, vertex, output],
        );
        let vertex = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DDevice9::SetVertexShader", &[f.device, vertex]);
        let elements = f.packet([[0u8, 0, 4, 0, 3, 0, 0, 0], [255, 0, 0, 0, 17, 0, 0, 0]]);
        f.okay(
            "IDirect3DDevice9::CreateVertexDeclaration",
            &[f.device, elements, output],
        );
        let declaration = f.m.cpu.read_u32(output).unwrap();
        f.okay(
            "IDirect3DDevice9::SetVertexDeclaration",
            &[f.device, declaration],
        );
        // A quad as a user-pointer triangle strip, stride 20.
        let quad = f.packet([
            [0f32, -1., -1., 0.5, 1.],
            [0., -1., 1., 0.5, 1.],
            [0., 1., -1., 0.5, 1.],
            [0., 1., 1., 0.5, 1.],
        ]);
        let mut draw = |f: &mut Fixture, constant: u32| {
            let pixel = f.packet([0xffff0200u32, 0x02000001, 0x800f0800, constant, 0xffff]);
            f.okay(
                "IDirect3DDevice9::CreatePixelShader",
                &[f.device, pixel, output],
            );
            let pixel = f.m.cpu.read_u32(output).unwrap();
            f.okay("IDirect3DDevice9::SetPixelShader", &[f.device, pixel]);
            for job in f.m.ext.get::<State>().programs.warming.values() {
                job.resolve().as_ref().unwrap();
            }
            let before = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats();
            f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
            f.okay(
                "IDirect3DDevice9::DrawPrimitiveUP",
                &[f.device, 5, 2, quad, 20],
            );
            f.okay("IDirect3DDevice9::EndScene", &[f.device]);
            let image = f.image(f.back);
            f.pixels(&image);
            let after = f.m.ext.get::<State>().gpu.as_ref().unwrap().stats();
            after.anticipated_pipeline_draws - before.anticipated_pipeline_draws
        };
        // The first strip draw follows a triangle-list prediction; the next
        // shader is predicted as the strip it will draw.
        assert_eq!(draw(&mut f, 0xa0e40000), 0);
        assert_eq!(draw(&mut f, 0xa0e40001), 1);
    }

    // spec: GFX-CACHE-7, GFX-SHADER-3
    #[test]
    fn shader_and_declaration_creation_prepare_exact_draw_modules() {
        let mut machine = crate::d3d9::tests::machine();
        machine.sys.objects = Objects::default();
        machine.ext.get_mut::<State>().com = Com::new(&mut machine.sys.objects).unwrap();
        machine.ext.get_mut::<State>().gpu = Some(
            adv_device::gpu::Gpu::request(!cfg!(any(target_os = "macos", target_os = "android")))
                .unwrap(),
        );
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let (device, _, _, _) = crate::d3d9::tests::setup(&mut env);
        let vertex: [u32; 8] = [
            0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000,
            0xffff,
        ];
        let pixel: [u32; 5] = [0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0xffff];
        let mut pointers = Vec::new();
        for (tokens, vertex_stage) in [(&vertex[..], true), (&pixel[..], false)] {
            let input = alloc(&mut env, tokens.len() as u32 * 4).unwrap();
            env.cpu
                .write(
                    input,
                    &tokens
                        .iter()
                        .flat_map(|t| t.to_le_bytes())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
            let status = if vertex_stage {
                shaders::create_vertex(
                    &mut env,
                    GuestPtr::new(device),
                    GuestPtr::new(input),
                    output,
                )
            } else {
                shaders::create_pixel(
                    &mut env,
                    GuestPtr::new(device),
                    GuestPtr::new(input),
                    output,
                )
            };
            assert_eq!(status, 0);
            pointers.push(output.read(env.cpu).unwrap());
        }
        let prepared = env.ext.get::<State>().programs.warming.len();
        for _ in 0..20 {
            for (vertex, pixel) in [(0, 0), (pointers[0], 0), (pointers[0], pointers[1])] {
                assert_eq!(
                    shaders::set_vertex(&mut env, GuestPtr::new(device), GuestPtr::new(vertex)),
                    0
                );
                assert_eq!(
                    shaders::set_pixel(&mut env, GuestPtr::new(device), GuestPtr::new(pixel)),
                    0
                );
            }
        }
        assert_eq!(env.ext.get::<State>().programs.warming.len(), prepared);
        let elements = [[0, 0, 0, 0, 2, 0, 0, 0], [255, 0, 0, 0, 17, 0, 0, 0]];
        let input = alloc(&mut env, 16).unwrap();
        env.cpu.write(input, &elements.concat()).unwrap();
        let output = GuestPtr::new(alloc(&mut env, 4).unwrap());
        assert_eq!(
            crate::d3d9::declaration::create(
                &mut env,
                GuestPtr::new(device),
                GuestPtr::new(input),
                output
            ),
            0
        );
        let vs = Shader::parse(&vertex).unwrap();
        let ps = Shader::parse(&pixel).unwrap();
        let expected = source::build(
            Some(&vs),
            Some(&ps),
            Some(&vs.translate(false).unwrap()),
            Some(&ps.translate(false).unwrap()),
            &[Input {
                stream: 0,
                offset: 0,
                format: AttributeFormat::Float3,
                register: 0,
            }],
            &[],
            (false, 0, false, 0),
        )
        .unwrap();
        let cache = &env.ext.get::<State>().programs;
        let (key, job) = cache
            .warming
            .iter()
            .find(|(key, _)| {
                key.vertex.is_some()
                    && key.pixel.is_some()
                    && key.inputs == vec![(0, AttributeFormat::Float3)]
            })
            .unwrap();
        assert!(key.vertex.is_some());
        assert_eq!(job.resolve().as_ref().unwrap().source(), expected);
        env.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .check()
            .unwrap();
    }
}
