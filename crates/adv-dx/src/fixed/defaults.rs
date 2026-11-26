//! Baseline registers for the shared fixed-function pipeline.
use super::{LightKind, LightValue, Material, Registers, State, StateMap};
use std::collections::BTreeMap;

pub(crate) fn defaults(width: u32, height: u32) -> State {
    let sampler = BTreeMap::from([
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 0),
        (5, 1),
        (6, 1),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 1),
        (11, 0),
        (12, 0),
        (13, 0),
    ]);
    State::new(
        Registers {
            render: StateMap::from(BTreeMap::from([
                (7, 0),
                (8, 3), // FILLMODE: only solid rasterization is implemented.
                (9, 2), // SHADEMODE: GOURAUD.
                (14, 1),
                (15, 0),
                (16, 1), // LASTPIXEL: consumed by the shared line rasterizer.
                (19, 2),
                (20, 1),
                (22, 3),
                (23, 4),
                (24, 0),
                (25, 8),
                (27, 0),
                (28, 0),
                (34, 0),
                (35, 0),
                (36, 0f32.to_bits()),
                (37, 1f32.to_bits()),
                (38, 1f32.to_bits()),
                (140, 0),
                (48, 0),        // RANGEFOGENABLE.
                (60, u32::MAX), // TEXTUREFACTOR.
                (142, 1),       // LOCALVIEWER.
                (143, 0),       // NORMALIZENORMALS.
                (157, 0),
                (158, 1f32.to_bits()),
                (159, 0),
                (160, 0),
                (162, u32::MAX),
                (165, 0),
                (170, 0),
                (172, 3),
                (173, 1),
                (175, 0),
                (176, 0),
                (193, u32::MAX),
                (194, 0),
                (195, 0),
                (154, 1f32.to_bits()),
                (155, 1f32.to_bits()),
                (156, 0),
                (166, 64f32.to_bits()),
                (29, 0), // SPECULARENABLE.
                (26, 0), // DITHERENABLE: the renderer does not dither.
                (52, 0),
                (53, 1),
                (54, 1),
                (55, 1),
                (56, 8),
                (57, 0),
                (58, u32::MAX),
                (59, u32::MAX),
                (128, 0), // WRAP0–WRAP7: texture coordinate wrapping is disabled.
                (129, 0),
                (130, 0),
                (131, 0),
                (132, 0),
                (133, 0),
                (134, 0),
                (135, 0),
                (136, 1), // CLIPPING: GPU primitive clipping remains enabled.
                (137, 1),
                (139, 0), // AMBIENT.
                (141, 1), // COLORVERTEX: only relevant when lighting is enabled.
                (145, 1), // Material sources: COLOR1, COLOR2, MATERIAL, MATERIAL.
                (146, 2),
                (147, 0),
                (148, 0),
                (151, 0), // VERTEXBLEND: one world matrix without blending.
                (152, 0), // CLIPPLANEENABLE: no user clipping planes.
                (161, 1), // Inert on the supported single-sample render targets.
                (167, 0), // INDEXEDVERTEXBLENDENABLE: no matrix indices.
                (168, 15),
                (190, 15),
                (191, 15),
                (192, 15),
                (198, 0),
                (199, 0),
                (200, 0),
                (201, 0),
                (202, 0),
                (203, 0),
                (204, 0),
                (205, 0),
                (171, 1),
                (174, 0),
                (185, 0),
                (186, 1),
                (187, 1),
                (188, 1),
                (189, 8),
                (206, 0),
                (207, 2),
                (208, 1),
                (209, 1),
            ])),
            stage_slots: std::array::from_fn(|stage| {
                let mut states =
                    BTreeMap::from([(7, 0), (8, 0), (9, 0), (10, 0), (22, 0), (23, 0)]);
                if stage != 0 {
                    states.extend([(1, 1), (2, 2), (3, 1), (4, 1), (5, 2), (6, 1)]);
                    // TEXCOORDINDEX defaults to the stage's own coordinate set.
                    states.insert(11, stage as u32);
                }
                states.into()
            }),
            stage: StateMap::from(BTreeMap::from([
                (1, 4),
                (2, 2),
                (3, 1),
                (4, 2),
                (5, 2),
                (6, 1),
                (11, 0), // TEXCOORDINDEX: pass through the first FVF coordinate set.
                (24, 0), // TEXTURETRANSFORMFLAGS: no texture coordinate transform.
            ])),
            sampler: sampler.clone().into(),
            sampler_slots: std::array::from_fn(|_| sampler.clone().into()),
        },
        width,
        height,
        material([
            1., 1., 1., 1., 1., 1., 1., 1., 0., 0., 0., 0., 0., 0., 0., 0., 0.,
        ]),
        default_light(),
    )
}

pub(crate) fn material(words: [f32; 17]) -> Material {
    Material {
        colors: std::array::from_fn(|i| words[i * 4..][..4].try_into().unwrap()),
        power: words[16],
    }
}

pub(crate) fn default_light() -> LightValue {
    let mut components = [0; 25];
    components[..3].fill(1f32.to_bits());
    components[17] = 1f32.to_bits();
    LightValue {
        kind: LightKind::Directional,
        components,
    }
}
