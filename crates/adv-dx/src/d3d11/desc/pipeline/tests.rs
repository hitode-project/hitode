use super::*;

// spec: GFX-CAP-4
#[test]
fn normalized_blend_replicates_the_first_target() {
    let mut desc = D3D11_BLEND_DESC::default();
    desc.RenderTarget[0].RenderTargetWriteMask = 15;
    let original = PipelineState::blend(desc, FeatureLevel::L11_0).unwrap();
    desc.RenderTarget[1].RenderTargetWriteMask = 2;
    desc.RenderTarget[0].SrcBlend = 99;
    let equivalent = PipelineState::blend(desc, FeatureLevel::L11_0).unwrap();
    assert_eq!(original.descriptor_bytes(), equivalent.descriptor_bytes());
    let PipelineState::Blend(desc) = equivalent else {
        unreachable!()
    };
    for target in desc.RenderTarget {
        assert_eq!(target.RenderTargetWriteMask, 15);
        assert_eq!(
            (target.SrcBlend, target.DestBlend, target.BlendOp),
            (2, 1, 1)
        );
    }
}

// spec: GFX-CAP-4
#[test]
fn sampler_canonicalizes_unused_values_and_checks_device_limits() {
    let mut desc = D3D11_SAMPLER_DESC {
        AddressU: 3,
        AddressV: 3,
        AddressW: 3,
        ComparisonFunc: 99,
        BorderColor: [4.0, 0.0, 0.0, 0.0],
        MaxLOD: 1.0,
        ..Default::default()
    };
    let PipelineState::Sampler(parsed) = PipelineState::sampler(desc, FeatureLevel::L11_0).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(parsed.BorderColor, [0.0; 4]);
    assert_eq!(parsed.ComparisonFunc, 1);
    desc.MaxAnisotropy = 17;
    assert!(matches!(
        PipelineState::sampler(desc, FeatureLevel::L11_0),
        Err(Error::InvalidArgument)
    ));
    desc.MaxAnisotropy = 1;
    desc.AddressU = 5;
    assert!(matches!(
        PipelineState::sampler(desc, FeatureLevel::L9_1),
        Err(Error::InvalidArgument)
    ));
    assert!(PipelineState::sampler(desc, FeatureLevel::L9_2).is_ok());
}
