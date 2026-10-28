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
    let state = env.ext.get_mut::<State>();
    let Some(context) = state.gpu.as_ref().map(|gpu| gpu.context().clone()) else {
        return;
    };
    let cache = &mut state.programs;
    if cache.entries.contains_key(&key) || cache.warming.contains_key(&key) {
        return;
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
        context.prepare_program(program).map_err(|e| e.to_string())
    });
    cache.warming.insert(key, job);
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-CACHE-7, GFX-SHADER-3
    #[test]
    fn shader_and_declaration_creation_prepare_exact_draw_modules() {
        let mut machine = crate::d3d9::tests::machine();
        machine.sys.objects = Objects::default();
        machine.ext.get_mut::<State>().com = Com::new(&mut machine.sys.objects).unwrap();
        machine.ext.get_mut::<State>().gpu = Some(
            pollster::block_on(adv_device::gpu::Gpu::request(!cfg!(any(
                target_os = "macos",
                target_os = "android"
            ))))
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
            Some(&vs.translate().unwrap()),
            Some(&ps.translate().unwrap()),
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
