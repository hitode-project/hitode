use super::*;
use crate::d3d11::{
    abi::{D3D11_BLEND_DESC, D3D11_TEXTURE2D_DESC, DXGI_SAMPLE_DESC},
    caps::{FeatureLevel, tests::fixture_report},
    desc::{Bind, Texture},
};

const DEVICE: u32 = 4;
const CONTEXT: u32 = 8;
const TEXTURE: u32 = 12;
const VIEW: u32 = 16;

fn setup() -> (Objects, Table) {
    let mut objects = Objects::default();
    let table = Table::new(&mut objects).unwrap();
    table
        .attach_device(
            &mut objects,
            DEVICE,
            Arc::new(fixture_report(FeatureLevel::L11_0)),
        )
        .unwrap();
    (objects, table)
}

// spec: GFX-RES-1
#[test]
fn device_references_follow_visible_children() {
    let (mut objects, table) = setup();
    let refs = |objects: &Objects, pointer| table.references(objects, pointer).unwrap();
    assert_eq!(refs(&objects, DEVICE), 1);
    table.attach_context(&mut objects, CONTEXT, DEVICE).unwrap();
    assert_eq!((refs(&objects, DEVICE), refs(&objects, CONTEXT)), (2, 1));
    let report = table.get(&objects, DEVICE).unwrap().report().unwrap();
    let texture = Resource::Texture(
        Texture::new_2d(
            D3D11_TEXTURE2D_DESC {
                Width: 16,
                Height: 16,
                MipLevels: 1,
                ArraySize: 1,
                Format: 28,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BindFlags: Bind::RENDER_TARGET,
                ..Default::default()
            },
            false,
            report,
        )
        .unwrap(),
    );
    let view = View::new(ViewKind::RenderTarget, texture, None, report).unwrap();
    table
        .attach_resource(&mut objects, TEXTURE, DEVICE, texture, None)
        .unwrap();
    assert_eq!((refs(&objects, DEVICE), refs(&objects, TEXTURE)), (3, 1));
    assert_eq!(
        table.get(&objects, TEXTURE).unwrap().priority(),
        Some(0x78000000)
    );
    table
        .attach_view(&mut objects, VIEW, TEXTURE, view)
        .unwrap();
    table.retain_bound(&mut objects, VIEW).unwrap();
    assert_eq!(
        (
            refs(&objects, DEVICE),
            refs(&objects, TEXTURE),
            refs(&objects, VIEW)
        ),
        (4, 1, 1)
    );

    let released = table.release(&mut objects, VIEW);
    assert_eq!(released.references, 0);
    assert!(released.allocations.is_empty());
    assert_eq!((refs(&objects, DEVICE), refs(&objects, TEXTURE)), (3, 1));
    assert_eq!(table.add_ref(&mut objects, VIEW), Ok(1));
    assert_eq!((refs(&objects, DEVICE), refs(&objects, VIEW)), (4, 1));

    let released = table.release(&mut objects, TEXTURE);
    assert_eq!(released.references, 0);
    assert!(released.allocations.is_empty());
    assert_eq!(refs(&objects, DEVICE), 3);
    let resource = table.get(&objects, VIEW).unwrap().view().unwrap().0;
    assert_eq!(resource, TEXTURE);
    assert_eq!(
        table.query(&mut objects, resource, IID_ID3D11RESOURCE),
        Ok(TEXTURE)
    );
    assert_eq!((refs(&objects, DEVICE), refs(&objects, TEXTURE)), (4, 1));
    assert_eq!(
        table.query(&mut objects, VIEW, IID_ID3D11RESOURCE),
        Err(Error::NoInterface)
    );

    table.release(&mut objects, TEXTURE);
    table.release(&mut objects, VIEW);
    assert_eq!(
        table.release_bound(&mut objects, VIEW).allocations,
        [VIEW, TEXTURE]
    );
    assert_eq!(table.release(&mut objects, CONTEXT).allocations, [CONTEXT]);
    assert_eq!(table.release(&mut objects, DEVICE).allocations, [DEVICE]);
}

// spec: GFX-RES-1
#[test]
fn immutable_state_identity_uses_the_normalized_description() {
    let (mut objects, table) = setup();
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].RenderTargetWriteMask = 15;
    let pipeline = PipelineState::blend(desc, FeatureLevel::L11_0).unwrap();
    assert_eq!(
        table.find_pipeline(&mut objects, DEVICE, &pipeline),
        Ok(None)
    );
    table
        .attach_pipeline(&mut objects, 20, DEVICE, pipeline)
        .unwrap();
    desc.RenderTarget[1].BlendEnable = 1;
    let equivalent = PipelineState::blend(desc, FeatureLevel::L11_0).unwrap();
    assert_eq!(
        table.find_pipeline(&mut objects, DEVICE, &equivalent),
        Ok(Some(20))
    );
    assert_eq!(table.references(&objects, 20), Some(2));
    assert_eq!(table.release(&mut objects, 20).references, 1);
    assert_eq!(table.release(&mut objects, 20).allocations, [20]);
    assert_eq!(
        table.find_pipeline(&mut objects, DEVICE, &equivalent),
        Ok(None)
    );
}
