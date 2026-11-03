use super::*;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Key {
    colors: [MTLPixelFormat; MAX_COLOR_TARGETS],
    write: u8,
    depth: MTLPixelFormat,
    stencil: MTLPixelFormat,
    clear_depth: bool,
    clear_stencil: bool,
    samples: usize,
}
pub(super) struct Pipeline {
    raw: Object<dyn MTLRenderPipelineState>,
    depth: Object<dyn MTLDepthStencilState>,
}

impl Device {
    pub(super) fn clear(
        &self,
        encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
        desc: &RenderPassDesc<'_>,
        area: Rect,
    ) -> bool {
        let mut key = Key {
            colors: [MTLPixelFormat::Invalid; MAX_COLOR_TARGETS],
            write: 0,
            depth: MTLPixelFormat::Invalid,
            stencil: MTLPixelFormat::Invalid,
            clear_depth: false,
            clear_stencil: false,
            samples: 1,
        };
        let mut values = [[0u32; 4]; MAX_COLOR_TARGETS];
        for (i, color) in desc.colors.iter().enumerate() {
            if !encode::attached(color.load, color.store) {
                continue;
            }
            key.colors[i] = color.view.inner.raw.pixelFormat();
            key.samples = color.view.inner.raw.sampleCount();
            if encode::partial(desc.area, color.view.size()) {
                match color.load {
                    Load::Clear(c) => {
                        values[i] = if key.colors[i] == MTLPixelFormat::R32Uint {
                            c.map(|v| v as u32)
                        } else {
                            c.map(|v| (v as f32).to_bits())
                        };
                        key.write |= 1 << i;
                    }
                    Load::DontCare => {
                        key.write |= 1 << i;
                    }
                    _ => {}
                }
            }
        }
        let (mut depth_value, mut stencil_value) = (0.0f32, 0u32);
        if let Some(depth) = &desc.depth_stencil {
            let bounded = encode::partial(desc.area, depth.view.size());
            let format = depth.view.inner.raw.pixelFormat();
            key.samples = depth.view.inner.raw.sampleCount();
            if encode::attached(depth.depth.0, depth.depth.1) {
                key.depth = format;
                if bounded && !depth.read_only {
                    match depth.depth.0 {
                        Load::Clear(value) => {
                            depth_value = value;
                            key.clear_depth = true;
                        }
                        Load::DontCare => {
                            key.clear_depth = true;
                        }
                        _ => {}
                    }
                }
            }
            if format == MTLPixelFormat::Depth32Float_Stencil8
                && encode::attached(depth.stencil.0, depth.stencil.1)
            {
                key.stencil = format;
                if bounded && !depth.read_only {
                    match depth.stencil.0 {
                        Load::Clear(value) => {
                            stencil_value = value;
                            key.clear_stencil = true;
                        }
                        Load::DontCare => {
                            key.clear_stencil = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        if (key.write == 0 && !key.clear_depth && !key.clear_stencil) || area.w == 0 || area.h == 0
        {
            return false;
        }
        let mut clears = self.0.clears.lock().unwrap();
        let pipeline = clears
            .entry(key.clone())
            .or_insert_with(|| self.clear_pipeline(&key));
        encoder.setRenderPipelineState(&pipeline.raw);
        encoder.setDepthStencilState(Some(&pipeline.depth));
        encoder.setStencilReferenceValue(stencil_value);
        unsafe {
            encoder.setVertexBytes_length_atIndex(
                NonNull::from(&depth_value).cast(),
                std::mem::size_of_val(&depth_value),
                0,
            );
            encoder.setFragmentBytes_length_atIndex(
                NonNull::from(&values).cast(),
                std::mem::size_of_val(&values),
                0,
            );
        }
        unsafe {
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        // Internal clearing is not caller-visible dynamic state.
        encoder.setDepthStencilState(None);
        encoder.setStencilReferenceValue(0);
        true
    }

    fn clear_pipeline(&self, key: &Key) -> Pipeline {
        let mut source = String::from(
            "#include <metal_stdlib>\nusing namespace metal;\nvertex float4 clear_vertex(uint id [[vertex_id]], constant float& z [[buffer(0)]]) { float2 p = float2(float((id << 1) & 2), float(id & 2)); return float4(p * 2.0 - 1.0, z, 1.0); }\nstruct Output {\n",
        );
        for i in 0..MAX_COLOR_TARGETS {
            if key.write & (1 << i) == 0 {
                continue;
            }
            let ty = if key.colors[i] == MTLPixelFormat::R32Uint {
                "uint4"
            } else {
                "float4"
            };
            source.push_str(&format!("{ty} c{i} [[color({i})]];\n"));
        }
        source.push_str("};\n");
        if key.write == 0 {
            source.push_str("fragment void clear_fragment() {}\n");
        } else {
            source.push_str("fragment Output clear_fragment(constant uint4* values [[buffer(0)]]) { Output o;\n");
            for i in 0..MAX_COLOR_TARGETS {
                if key.write & (1 << i) == 0 {
                    continue;
                }
                let value = if key.colors[i] == MTLPixelFormat::R32Uint {
                    format!("values[{i}]")
                } else {
                    format!("as_type<float4>(values[{i}])")
                };
                source.push_str(&format!("o.c{i} = {value};\n"));
            }
            source.push_str("return o; }\n");
        }
        let library = self
            .0
            .raw
            .newLibraryWithSource_options_error(&NSString::from_str(&source), None)
            .expect("Metal internal clear shader");
        let d = MTLRenderPipelineDescriptor::new();
        d.setVertexFunction(
            library
                .newFunctionWithName(&NSString::from_str("clear_vertex"))
                .as_deref(),
        );
        d.setFragmentFunction(
            library
                .newFunctionWithName(&NSString::from_str("clear_fragment"))
                .as_deref(),
        );
        d.setRasterSampleCount(key.samples);
        for (i, &format) in key.colors.iter().enumerate() {
            let color = unsafe { d.colorAttachments().objectAtIndexedSubscript(i) };
            color.setPixelFormat(format);
            color.setWriteMask(if key.write & (1 << i) != 0 {
                MTLColorWriteMask::All
            } else {
                MTLColorWriteMask::empty()
            });
        }
        d.setDepthAttachmentPixelFormat(key.depth);
        d.setStencilAttachmentPixelFormat(key.stencil);
        let raw = self
            .0
            .raw
            .newRenderPipelineStateWithDescriptor_error(&d)
            .expect("Metal internal clear pipeline");
        let d = MTLDepthStencilDescriptor::new();
        d.setDepthCompareFunction(MTLCompareFunction::Always);
        d.setDepthWriteEnabled(key.clear_depth);
        if key.clear_stencil {
            let stencil = MTLStencilDescriptor::new();
            stencil.setStencilCompareFunction(MTLCompareFunction::Always);
            stencil.setDepthStencilPassOperation(MTLStencilOperation::Replace);
            stencil.setReadMask(0xff);
            stencil.setWriteMask(0xff);
            d.setFrontFaceStencil(Some(&stencil));
            d.setBackFaceStencil(Some(&stencil));
        }
        let depth = self
            .0
            .raw
            .newDepthStencilStateWithDescriptor(&d)
            .expect("Metal internal clear depth state");
        Pipeline { raw, depth }
    }
}
