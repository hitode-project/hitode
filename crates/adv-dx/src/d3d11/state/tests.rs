use super::*;
use crate::d3d11::{
    abi::{
        D3D11_DEPTH_STENCIL_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC,
        D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_TEXTURE2D_DESC, DXGI_SAMPLE_DESC,
    },
    caps::{FeatureLevel, tests::fixture_report},
    desc::{Bind, Resource, Texture},
};

fn image(format: i32, flags: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: 16,
        Height: 16,
        MipLevels: 2,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BindFlags: flags,
        ..Default::default()
    }
}

fn binding(pointer: u32, desc: View) -> BoundView {
    BoundView {
        pointer,
        resource: 10,
        allocation: (10, 0),
        desc: Arc::new(desc),
    }
}

fn bind(state: &mut ContextState, slot: ViewSlot, view: &BoundView) {
    state.bind_view(slot, Some(view.clone()), |_, _, _| ());
}

fn bound(state: &ContextState, slot: ViewSlot) -> Option<u32> {
    state.groups().view(slot).map(|binding| binding.pointer)
}

// spec: GFX-REC-1 GFX-CAP-5
#[test]
fn outputs_evict_inputs_across_stages_and_inputs_cannot_evict_outputs() {
    let report = fixture_report(FeatureLevel::L11_0);
    let texture = Resource::Texture(
        Texture::new_2d(
            image(28, Bind::SHADER | Bind::RENDER_TARGET | Bind::UNORDERED),
            false,
            &report,
        )
        .unwrap(),
    );
    let srv = binding(
        1,
        View::new(ViewKind::ShaderResource, texture, None, &report).unwrap(),
    );
    let rtv = binding(
        2,
        View::new(ViewKind::RenderTarget, texture, None, &report).unwrap(),
    );
    let uav = binding(
        3,
        View::new(ViewKind::UnorderedAccess, texture, None, &report).unwrap(),
    );
    let vs = ViewSlot::srv(Stage::Vertex, 0).unwrap();
    let ps = ViewSlot::srv(Stage::Pixel, 0).unwrap();
    let cs = ViewSlot::srv(Stage::Compute, 0).unwrap();
    let rt = ViewSlot::rtv(0).unwrap();
    let ua = ViewSlot::cs_uav(0).unwrap();
    let mut state = ContextState::default();

    for slot in [vs, ps, cs] {
        bind(&mut state, slot, &srv);
    }
    let prior = state.snapshot();
    bind(&mut state, rt, &rtv);
    for slot in [vs, ps, cs] {
        assert_eq!(bound(&state, slot), None);
        assert_eq!(prior.view(slot).unwrap().pointer, 1);
    }
    bind(&mut state, ps, &srv);
    assert_eq!(bound(&state, ps), None);
    assert_eq!(bound(&state, rt), Some(2));

    bind(&mut state, ua, &uav);
    assert_eq!(bound(&state, rt), None);
    bind(&mut state, cs, &srv);
    assert_eq!(bound(&state, cs), None);
    assert_eq!(bound(&state, ua), Some(3));

    state.clear();
    for slot in [vs, ps, cs] {
        bind(&mut state, slot, &srv);
    }
    bind(&mut state, ua, &uav);
    for slot in [vs, ps, cs] {
        assert_eq!(bound(&state, slot), None);
    }
    assert_eq!(bound(&state, ua), Some(3));
}

// spec: GFX-REC-1 GFX-CAP-5
#[test]
fn separate_mips_and_read_only_depth_can_coexist() {
    let report = fixture_report(FeatureLevel::L11_0);
    let texture = Resource::Texture(
        Texture::new_2d(
            image(28, Bind::SHADER | Bind::RENDER_TARGET),
            false,
            &report,
        )
        .unwrap(),
    );
    let srv = binding(
        1,
        View::new(
            ViewKind::ShaderResource,
            texture,
            Some(
                D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: 28,
                    ViewDimension: 4,
                    Anonymous: bytemuck::cast([1u32, 1, 0, 0]),
                }
                .into(),
            ),
            &report,
        )
        .unwrap(),
    );
    let rtv = binding(
        2,
        View::new(
            ViewKind::RenderTarget,
            texture,
            Some(
                D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: 28,
                    ViewDimension: 4,
                    Anonymous: bytemuck::cast([0u32; 3]),
                }
                .into(),
            ),
            &report,
        )
        .unwrap(),
    );
    let ps = ViewSlot::srv(Stage::Pixel, 127).unwrap();
    let rt = ViewSlot::rtv(0).unwrap();
    let mut state = ContextState::default();
    bind(&mut state, ps, &srv);
    bind(&mut state, rt, &rtv);
    assert_eq!(bound(&state, rt), Some(2));
    assert_eq!(bound(&state, ps), Some(1));
    assert!(Arc::ptr_eq(&state.snapshot().ia, &state.groups().ia));

    let texture = Resource::Texture(
        Texture::new_2d(
            image(44, Bind::SHADER | Bind::DEPTH_STENCIL),
            false,
            &report,
        )
        .unwrap(),
    );
    let plane = |pointer, format| {
        binding(
            pointer,
            View::new(
                ViewKind::ShaderResource,
                texture,
                Some(
                    D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: format,
                        ViewDimension: 4,
                        Anonymous: bytemuck::cast([0u32, 1, 0, 0]),
                    }
                    .into(),
                ),
                &report,
            )
            .unwrap(),
        )
    };
    let dsv = binding(
        5,
        View::new(
            ViewKind::DepthStencil,
            texture,
            Some(
                D3D11_DEPTH_STENCIL_VIEW_DESC {
                    Format: 45,
                    ViewDimension: 3,
                    Flags: 1,
                    ..Default::default()
                }
                .into(),
            ),
            &report,
        )
        .unwrap(),
    );
    let depth = plane(3, 46);
    let stencil = plane(4, 47);
    let stencil_slot = ViewSlot::srv(Stage::Pixel, 126).unwrap();

    let mut other_context = ContextState::default();
    bind(&mut other_context, ps, &depth);
    bind(&mut other_context, stencil_slot, &stencil);
    bind(&mut other_context, ViewSlot::DSV, &dsv);
    assert_eq!(bound(&other_context, ps), Some(3));
    assert_eq!(bound(&other_context, stencil_slot), None);
    assert_eq!(bound(&other_context, ViewSlot::DSV), Some(5));
    assert_eq!(bound(&state, ps), Some(1));
}

// spec: GFX-REC-1 GFX-REC-2
#[test]
fn repeated_setters_keep_the_snapshot_and_changes_copy_one_group() {
    let mut state = ContextState::default();
    let buffer = VertexBuffer {
        buffer: 1,
        stride: 32,
        offset: 16,
    };
    state.set_vertices(0, &[buffer]);
    let retained = state.snapshot();
    state.set_vertices(0, &[buffer]);
    assert!(Arc::ptr_eq(&retained.ia, &state.groups().ia));

    state.set_vertices(
        0,
        &[VertexBuffer {
            offset: 32,
            ..buffer
        }],
    );
    assert_eq!(retained.ia.vertices[0].offset, 16);
    assert_eq!(state.groups().ia.vertices[0].offset, 32);
    assert!(Arc::ptr_eq(&retained.rs, &state.groups().rs));
    assert!(Arc::ptr_eq(&retained.om, &state.groups().om));
    assert!(
        retained
            .stages
            .iter()
            .zip(&state.groups().stages)
            .all(|(before, after)| Arc::ptr_eq(before, after))
    );
}
