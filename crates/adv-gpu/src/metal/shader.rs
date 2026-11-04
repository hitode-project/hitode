use super::*;
pub(super) use crate::shader::compile::{SIZES, Used, binding_group, compile_error, index};

#[derive(Clone)]
pub(crate) struct Shader(Arc<ShaderData>);
struct ShaderData {
    identity: u64,
    entries: Vec<Function>,
}
pub(super) struct Function {
    pub name: String,
    pub stage: Stage,
    pub raw: Object<dyn MTLFunction>,
    pub workgroup: [u32; 3],
    /// Bytes of each threadgroup argument, by index.
    pub threadgroup: Box<[u32]>,
    /// Native indices the entry reads; `None` for precompiled libraries, whose
    /// pipelines use their declared bindings.
    pub used: Option<Used>,
    /// Storage buffer index of each entry in naga's byte-size table at
    /// `buffer(SIZES)`; `None` for runtime arrays the entry does not use.
    pub sizes: Option<Box<[Option<u8>]>>,
}

fn version() -> (u8, u8) {
    if objc2::available!(macos = 26.0, ios = 26.0) {
        (4, 0)
    } else if objc2::available!(macos = 15.0, ios = 18.0) {
        (3, 2)
    } else if objc2::available!(macos = 14.0, ios = 17.0) {
        (3, 1)
    } else if objc2::available!(macos = 13.0, ios = 16.0) {
        (3, 0)
    } else if objc2::available!(macos = 12.0, ios = 15.0) {
        (2, 4)
    } else if objc2::available!(macos = 11.0, ios = 14.0) {
        (2, 3)
    } else if objc2::available!(macos = 10.15, ios = 13.0) {
        (2, 2)
    } else {
        (2, 1)
    }
}

impl Device {
    pub(crate) fn create_shader(&self, desc: ShaderDesc<'_>) -> Result<crate::Shader> {
        self.check()?;
        // These synchronous calls stay on the caller (the compile pool).
        objc2::rc::autoreleasepool(|_| {
            let module = match desc.source {
                Source::Wgsl(source) => naga::front::wgsl::parse_str(source)
                    .map_err(|e| compile_error("WGSL", e.emit_to_string(source)))?,
                Source::Ir(module) => *module,
                Source::Precompiled {
                    metal,
                    entries: names,
                    ..
                } => {
                    let library = if metal.starts_with(b"MTLB") {
                        let file = pipeline::TemporaryFile::new()?;
                        std::fs::write(&file.0, metal).map_err(pipeline::io_error)?;
                        self.0.raw.newLibraryWithURL_error(&file.url())
                    } else {
                        let source =
                            std::str::from_utf8(metal).map_err(|e| compile_error("MSL", e))?;
                        self.library(source)
                    }
                    .map_err(|e| compile_error("Metal library", e))?;
                    let mut entries = Vec::new();
                    for entry in names {
                        let raw = library
                            .newFunctionWithName(&NSString::from_str(entry.function))
                            .ok_or_else(|| compile_error("Metal library", "missing function"))?;
                        let stage = match raw.functionType() {
                            MTLFunctionType::Vertex => Stage::Vertex,
                            MTLFunctionType::Fragment => Stage::Fragment,
                            MTLFunctionType::Kernel => Stage::Compute,
                            _ => return Err(compile_error("Metal library", "unsupported stage")),
                        };
                        entries.push(Function {
                            name: entry.name.to_owned(),
                            stage,
                            raw,
                            workgroup: entry.workgroup,
                            threadgroup: entry.threadgroup.into(),
                            used: None,
                            sizes: None,
                        });
                    }
                    entries.sort_by(|a, b| a.name.cmp(&b.name));
                    return Ok(crate::Shader {
                        identity: desc.identity,
                        inner: Shader(Arc::new(ShaderData {
                            identity: desc.identity,
                            entries,
                        })),
                    });
                }
            };
            let mut capabilities = naga::valid::Capabilities::default()
                | naga::valid::Capabilities::SHADER_FLOAT16_IN_FLOAT32
                | naga::valid::Capabilities::EARLY_DEPTH_TEST;
            if self.caps().features.contains(Features::SHADER_F16) {
                capabilities |= naga::valid::Capabilities::SHADER_FLOAT16;
            }
            if self.caps().features.contains(Features::SUBGROUP) {
                capabilities |= naga::valid::Capabilities::SUBGROUP
                    | naga::valid::Capabilities::SUBGROUP_BARRIER;
            }
            if self.caps().features.contains(Features::MATRIX_F16_8X8) {
                capabilities |= naga::valid::Capabilities::COOPERATIVE_MATRIX;
            }
            let info =
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), capabilities)
                    .validate(&module)
                    .map_err(|e| compile_error("shader validation", e))?;
            let mut entries = Vec::new();
            for ep in &module.entry_points {
                let stage = match ep.stage {
                    naga::ShaderStage::Vertex => Stage::Vertex,
                    naga::ShaderStage::Fragment => Stage::Fragment,
                    naga::ShaderStage::Compute => Stage::Compute,
                    _ => return Err(compile_error(&ep.name, "unsupported stage")),
                };
                let (module, info) = naga::back::pipeline_constants::process_overrides(
                    &module,
                    &info,
                    Some((ep.stage, &ep.name)),
                    &Default::default(),
                )
                .map_err(|e| compile_error(&ep.name, e))?;
                let lowered = crate::shader::compile::metal(
                    &module,
                    &info,
                    None,
                    crate::shader::compile::Options {
                        version: version(),
                        textures: self.caps().limits.max_sampled_images,
                        bounds: naga::proc::BoundsCheckPolicies {
                            index: naga::proc::BoundsCheckPolicy::Restrict,
                            buffer: naga::proc::BoundsCheckPolicy::Restrict,
                            image_load: naga::proc::BoundsCheckPolicy::Restrict,
                            ..Default::default()
                        },
                    },
                )?;
                let entry = lowered.entries.into_iter().next().unwrap();
                let library = self
                    .library(&lowered.source)
                    .map_err(|e| compile_error(&ep.name, e))?;
                library.setLabel(Some(&NSString::from_str(desc.label)));
                let raw = library
                    .newFunctionWithName(&NSString::from_str(&entry.name))
                    .ok_or_else(|| compile_error(&ep.name, "missing translated entry"))?;
                entries.push(Function {
                    name: ep.name.clone(),
                    stage,
                    raw,
                    workgroup: module.entry_points[0].workgroup_size,
                    threadgroup: lowered.threadgroup.into_iter().next().unwrap().into(),
                    used: Some(entry.used),
                    sizes: entry.sizes,
                });
            }
            Ok(crate::Shader {
                identity: desc.identity,
                inner: Shader(Arc::new(ShaderData {
                    identity: desc.identity,
                    entries,
                })),
            })
        })
    }
    fn library(
        &self,
        source: &str,
    ) -> std::result::Result<Object<dyn MTLLibrary>, Retained<objc2_foundation::NSError>> {
        let version = version();
        let compile = MTLCompileOptions::new();
        compile.setLanguageVersion(MTLLanguageVersion(
            ((version.0 as usize) << 16) | version.1 as usize,
        ));
        if objc2::available!(macos = 15.0, ios = 18.0) {
            compile.setMathMode(MTLMathMode::Relaxed);
        } else {
            #[allow(deprecated)]
            compile.setFastMathEnabled(false);
        }
        if objc2::available!(macos = 11.0, ios = 13.0) {
            compile.setPreserveInvariance(true);
        }
        self.0
            .raw
            .newLibraryWithSource_options_error(&NSString::from_str(source), Some(&compile))
    }
}
impl Shader {
    pub(crate) fn entry(&self, name: &str, stage: Stage) -> Result<Entry> {
        let index = self
            .0
            .entries
            .iter()
            .position(|f| f.name == name && f.stage == stage)
            .ok_or_else(|| {
                compile_error(format!("{stage:?}"), format!("entry {name} not found"))
            })?;
        Ok(Entry {
            shader: crate::Shader {
                identity: self.0.identity,
                inner: self.clone(),
            },
            index: index as u32,
            stage,
        })
    }
    pub(super) fn function(&self, entry: &Entry) -> Result<&Function> {
        self.0
            .entries
            .get(entry.index as usize)
            .filter(|f| f.stage == entry.stage)
            .ok_or_else(|| {
                compile_error(format!("{:?}", entry.stage), "invalid entry index or stage")
            })
    }
}
