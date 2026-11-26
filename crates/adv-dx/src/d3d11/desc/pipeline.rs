//! Validated immutable pipeline descriptions.
use super::Error;
use crate::d3d11::{
    abi::{
        D3D11_BLEND_DESC, D3D11_BLEND_DESC1, D3D11_DEPTH_STENCIL_DESC, D3D11_DEPTH_STENCILOP_DESC,
        D3D11_RASTERIZER_DESC, D3D11_RASTERIZER_DESC1, D3D11_RENDER_TARGET_BLEND_DESC1,
        D3D11_SAMPLER_DESC, limit,
    },
    caps::{FeatureLevel, Requirement},
};

#[derive(Clone, Copy)]
pub enum PipelineState {
    Blend(D3D11_BLEND_DESC1),
    DepthStencil(D3D11_DEPTH_STENCIL_DESC),
    Rasterizer(D3D11_RASTERIZER_DESC1),
    Sampler(D3D11_SAMPLER_DESC),
}

impl PipelineState {
    pub fn blend(desc: D3D11_BLEND_DESC, level: FeatureLevel) -> Result<Self, Error> {
        Self::blend1(
            D3D11_BLEND_DESC1 {
                AlphaToCoverageEnable: desc.AlphaToCoverageEnable,
                IndependentBlendEnable: desc.IndependentBlendEnable,
                RenderTarget: desc
                    .RenderTarget
                    .map(|target| D3D11_RENDER_TARGET_BLEND_DESC1 {
                        BlendEnable: target.BlendEnable,
                        LogicOpEnable: 0,
                        SrcBlend: target.SrcBlend,
                        DestBlend: target.DestBlend,
                        BlendOp: target.BlendOp,
                        SrcBlendAlpha: target.SrcBlendAlpha,
                        DestBlendAlpha: target.DestBlendAlpha,
                        BlendOpAlpha: target.BlendOpAlpha,
                        LogicOp: 4,
                        RenderTargetWriteMask: target.RenderTargetWriteMask,
                        _padding0: [0; 3],
                    }),
            },
            level,
        )
    }

    pub fn blend1(mut desc: D3D11_BLEND_DESC1, level: FeatureLevel) -> Result<Self, Error> {
        desc.AlphaToCoverageEnable = bool_field(desc.AlphaToCoverageEnable);
        desc.IndependentBlendEnable = bool_field(desc.IndependentBlendEnable);
        let independent = desc.IndependentBlendEnable != 0;
        let used = if independent {
            desc.RenderTarget.len()
        } else {
            1
        };
        for target in &mut desc.RenderTarget[..used] {
            if target.RenderTargetWriteMask > 15 || target.LogicOpEnable != 0 {
                return Err(Error::InvalidArgument);
            }
            target.LogicOp = 4;
            target._padding0 = [0; 3];
            target.BlendEnable = bool_field(target.BlendEnable);
            if target.BlendEnable == 0 {
                target.SrcBlend = 2;
                target.DestBlend = 1;
                target.BlendOp = 1;
                target.SrcBlendAlpha = 2;
                target.DestBlendAlpha = 1;
                target.BlendOpAlpha = 1;
                continue;
            }
            let color = [target.SrcBlend, target.DestBlend];
            let alpha = [target.SrcBlendAlpha, target.DestBlendAlpha];
            for factor in color.into_iter().chain(alpha) {
                match factor {
                    1..=11 | 14 | 15 => (),
                    16..=19 if level.limit(Requirement::DualSourceBlend) != 0 && !independent => (),
                    _ => return Err(Error::InvalidArgument),
                }
            }
            if alpha
                .into_iter()
                .any(|value| matches!(value, 3 | 4 | 9 | 10 | 16 | 17))
                || !(1..=5).contains(&target.BlendOp)
                || !(1..=5).contains(&target.BlendOpAlpha)
            {
                return Err(Error::InvalidArgument);
            }
        }
        if !independent {
            desc.RenderTarget = [desc.RenderTarget[0]; 8];
        }
        Ok(Self::Blend(desc))
    }

    pub fn depth_stencil(mut desc: D3D11_DEPTH_STENCIL_DESC) -> Result<Self, Error> {
        desc.DepthEnable = bool_field(desc.DepthEnable);
        desc.StencilEnable = bool_field(desc.StencilEnable);
        desc._padding0 = [0; 2];
        if !(0..=1).contains(&desc.DepthWriteMask) {
            return Err(Error::InvalidArgument);
        }
        if desc.DepthEnable == 0 {
            desc.DepthFunc = 2;
            desc.DepthWriteMask = 1;
        } else if !compare(desc.DepthFunc) {
            return Err(Error::InvalidArgument);
        }
        if desc.StencilEnable == 0 {
            desc.StencilReadMask = u8::MAX;
            desc.StencilWriteMask = u8::MAX;
            let face = D3D11_DEPTH_STENCILOP_DESC {
                StencilFailOp: 1,
                StencilDepthFailOp: 1,
                StencilPassOp: 1,
                StencilFunc: 8,
            };
            desc.FrontFace = face;
            desc.BackFace = face;
        } else {
            for face in [desc.FrontFace, desc.BackFace] {
                if !compare(face.StencilFunc)
                    || [
                        face.StencilFailOp,
                        face.StencilDepthFailOp,
                        face.StencilPassOp,
                    ]
                    .into_iter()
                    .any(|op| !(1..=8).contains(&op))
                {
                    return Err(Error::InvalidArgument);
                }
            }
        }
        Ok(Self::DepthStencil(desc))
    }

    pub fn rasterizer(desc: D3D11_RASTERIZER_DESC, level: FeatureLevel) -> Result<Self, Error> {
        Self::rasterizer1(
            D3D11_RASTERIZER_DESC1 {
                FillMode: desc.FillMode,
                CullMode: desc.CullMode,
                FrontCounterClockwise: desc.FrontCounterClockwise,
                DepthBias: desc.DepthBias,
                DepthBiasClamp: desc.DepthBiasClamp,
                SlopeScaledDepthBias: desc.SlopeScaledDepthBias,
                DepthClipEnable: desc.DepthClipEnable,
                ScissorEnable: desc.ScissorEnable,
                MultisampleEnable: desc.MultisampleEnable,
                AntialiasedLineEnable: desc.AntialiasedLineEnable,
                ForcedSampleCount: 0,
            },
            level,
        )
    }

    pub fn rasterizer1(
        mut desc: D3D11_RASTERIZER_DESC1,
        level: FeatureLevel,
    ) -> Result<Self, Error> {
        if !matches!(desc.FillMode, 2 | 3)
            || !matches!(desc.CullMode, 1..=3)
            || (desc.DepthClipEnable == 0 && level.limit(Requirement::DepthClipDisable) == 0)
            || (desc.DepthBiasClamp != 0.0 && level.limit(Requirement::DepthBiasClamp) == 0)
            || desc.ForcedSampleCount != 0
        {
            return Err(Error::InvalidArgument);
        }
        desc.FrontCounterClockwise = bool_field(desc.FrontCounterClockwise);
        desc.DepthClipEnable = bool_field(desc.DepthClipEnable);
        desc.ScissorEnable = bool_field(desc.ScissorEnable);
        desc.MultisampleEnable = bool_field(desc.MultisampleEnable);
        desc.AntialiasedLineEnable = bool_field(desc.AntialiasedLineEnable);
        Ok(Self::Rasterizer(desc))
    }

    pub fn sampler(mut desc: D3D11_SAMPLER_DESC, level: FeatureLevel) -> Result<Self, Error> {
        let comparison = desc.Filter & 0x80 != 0;
        let filter = desc.Filter & !0x80;
        if !matches!(filter, 0 | 1 | 4 | 5 | 16 | 17 | 20 | 21 | 85)
            || (comparison && !compare(desc.ComparisonFunc))
            || desc.MaxAnisotropy > level.limit(Requirement::Anisotropy)
            || !(f32::from_bits(limit("D3D11_MIP_LOD_BIAS_MIN"))
                ..=f32::from_bits(limit("D3D11_MIP_LOD_BIAS_MAX")))
                .contains(&desc.MipLODBias)
        {
            return Err(Error::InvalidArgument);
        }
        let addresses = [desc.AddressU, desc.AddressV, desc.AddressW];
        for address in addresses {
            match address {
                1..=4 => (),
                5 if level.limit(Requirement::MirrorOnce) != 0 => (),
                _ => return Err(Error::InvalidArgument),
            }
        }
        if !comparison {
            desc.ComparisonFunc = 1;
        }
        if !addresses.contains(&4) {
            desc.BorderColor = [0.0; 4];
        }
        Ok(Self::Sampler(desc))
    }

    pub fn descriptor_bytes(&self) -> &[u8] {
        match self {
            Self::Blend(desc) => bytemuck::bytes_of(desc),
            Self::DepthStencil(desc) => bytemuck::bytes_of(desc),
            Self::Rasterizer(desc) => bytemuck::bytes_of(desc),
            Self::Sampler(desc) => bytemuck::bytes_of(desc),
        }
    }
}

fn bool_field(value: i32) -> i32 {
    i32::from(value != 0)
}

fn compare(value: i32) -> bool {
    (1..=8).contains(&value)
}

#[cfg(test)]
mod tests;
