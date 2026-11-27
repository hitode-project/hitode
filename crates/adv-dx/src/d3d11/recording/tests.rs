use super::*;
use adv_device::gpu::{DrawArguments, ModernColor, ModernShader, StorageBinding};
use adv_gpu::{
    Extent, Format, Image,
    encode::{Load, Store},
    pipeline::{Cull, Primitive, Topology},
    resource::{BufferDesc, BufferUsage, Dimension, ImageDesc, Memory, Usage},
    shader::Stage,
};

fn image(gpu: &Gpu, usage: Usage) -> Image {
    gpu.device()
        .create_image(&ImageDesc {
            dimension: Dimension::D2,
            size: Extent {
                width: 2,
                height: 2,
                depth_or_layers: 1,
            },
            format: Format::R32Uint,
            mips: 1,
            samples: 1,
            usage,
            view_formats: &[],
            label: "recording fixture",
        })
        .unwrap()
}

fn shader(source: &str, stage: Stage, id: u64) -> Arc<ModernShader> {
    Arc::new(ModernShader::wgsl(id, source, "main", stage).unwrap())
}

// spec: GFX-REC-1 GFX-PASS-1
#[test]
fn recorded_dispatch_finishes_before_the_draw_reads_its_image() {
    let mut gpu = Gpu::request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
    let storage = image(&gpu, Usage::STORAGE | Usage::SAMPLED);
    let output = image(&gpu, Usage::COLOR | Usage::COPY_SRC);
    let extent = Rect {
        x: 0,
        y: 0,
        w: 2,
        h: 2,
    };
    let key = |image: &Image| ResourceKey((image.id(), 0), 0, 0);

    let compute = shader(
        "@group(3) @binding(0) var pixels: texture_storage_2d<r32uint, write>;
         @compute @workgroup_size(1) fn main(@builtin(global_invocation_id) id: vec3<u32>) {
             textureStore(pixels, vec2<i32>(id.xy), vec4<u32>(37u + id.x + 2u * id.y));
         }",
        Stage::Compute,
        1,
    );
    let dispatch = Command::Dispatch {
        pipeline: Arc::new(gpu.prepare_dispatch(compute).unwrap()),
        bindings: SlotBindings {
            storage: vec![Some(StorageBinding::Image(storage.view().clone()))],
            ..Default::default()
        },
        arguments: DispatchArguments::Direct([2, 2, 1]),
    };
    let vertex = shader(
        "@vertex fn main(@builtin(vertex_index) id: u32) -> @builtin(position) vec4<f32> {
             let p = array(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
             return vec4(p[id], 0.0, 1.0);
         }",
        Stage::Vertex,
        2,
    );
    let fragment = shader(
        "@group(1) @binding(0) var pixels: texture_2d<u32>;
         @fragment fn main(@builtin(position) position: vec4<f32>) -> @location(0) u32 {
             return textureLoad(pixels, vec2<i32>(position.xy), 0).x;
         }",
        Stage::Fragment,
        3,
    );
    let draw = Command::Draw {
        draw: Box::new(ModernDraw {
            color_slots: None,
            state_ids: [0; 4],
            vertex,
            fragment: Some(fragment),
            colors: std::iter::once(ModernColor {
                view: output.view().clone(),
                blend: None,
                write_mask: 0xf,
                load: Load::DontCare,
                store: Store::Store,
            })
            .collect(),
            depth: None,
            samples: 1,
            sample_mask: u32::MAX,
            inexact_sample_mask: false,
            alpha_to_coverage: false,
            primitive: Primitive {
                topology: Topology::Triangles,
                cull: Cull::None,
                front_ccw: true,
                wireframe: false,
                unclipped_depth: false,
            },
            area: extent.into(),
            viewport: [0., 0., 2., 2., 0., 1.],
            scissor: extent.into(),
            stencil_reference: 0,
            blend_constant: [0.; 4],
            depth_bias: [0.; 3],
            arguments: DrawArguments::Direct {
                vertices: 0..3,
                instances: 0..1,
                base_vertex: 0,
            },
            index: None,
            occlusion: None,
            predicate: None,
        }),
        statistics: None,
        vertex: SlotBindings::default(),
        fragment: SlotBindings {
            images: vec![Some(storage.view().clone())],
            ..Default::default()
        },
    };

    let mut recording = Recording::default();
    for (command, key, reads, writes) in [
        (dispatch, 1, &[][..], &[(key(&storage), true)][..]),
        (draw, 2, &[key(&storage)][..], &[(key(&output), true)][..]),
    ] {
        recording
            .append(
                &mut gpu,
                command,
                Effects {
                    extent,
                    bounds: extent,
                    key,
                    reads,
                    writes,
                    retained_bytes: 0,
                },
            )
            .unwrap();
    }
    recording.flush(&mut gpu, Boundary::Flush).unwrap();

    let buffer = gpu
        .device()
        .create_buffer(&BufferDesc {
            size: 16,
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "recording fixture readback",
        })
        .unwrap();
    gpu.copy_image_to_buffer(&output, 0, 0, buffer.clone(), 8)
        .unwrap();
    let fence = gpu.target(1, 1, false).unwrap();
    let mut readback = gpu.readback(&fence.image, None).unwrap();
    readback.take_completion_job().unwrap()();
    readback.poll().unwrap().unwrap();
    gpu.check().unwrap();
    // SAFETY: the later readback completed on the same ordered queue.
    let bytes = unsafe { buffer.mapped().unwrap().as_ref() };
    assert_eq!(bytemuck::cast_slice::<_, u32>(bytes), &[37, 38, 39, 40]);
}

// spec: GFX-REC-3
#[test]
fn frame_verification_observes_pixels_outside_damage_after_present_returns() {
    let mut gpu = Gpu::request(!cfg!(any(target_vendor = "apple", target_os = "android"))).unwrap();
    let target = gpu.target(16, 16, false).unwrap().image;
    let mut recording = Recording::default();
    let mut damage = Region::empty((16, 16));
    damage.add(
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        }
        .into(),
    );
    let all = Rect {
        x: 0,
        y: 0,
        w: 16,
        h: 16,
    };
    gpu.upload(&target, all, 64, &[0u8; 16 * 16 * 4]).unwrap();
    recording
        .verify(&mut gpu, &target, Region::whole((16, 16)))
        .unwrap();
    let changed = Rect {
        x: 15,
        y: 15,
        w: 1,
        h: 1,
    };
    gpu.upload(&target, changed, 4, &[255, 0, 0, 0]).unwrap();
    recording.verify(&mut gpu, &target, damage).unwrap();

    // Wait from the fixture after submitting both observations.
    gpu.completion().unwrap().recv().unwrap();
    let verification = recording.verification.as_ref().unwrap();
    let progress = verification.progress.lock().unwrap();
    assert_eq!(
        progress.previous.as_ref().unwrap().2[1020..1024],
        [255, 0, 0, 0]
    );
    #[cfg(feature = "d3d9-counters")]
    assert_eq!(
        verification
            .mismatches
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}
