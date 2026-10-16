//! A single lazy job owns each persisted pipeline. Prebuilding and demand share
//! its result; only real draws publish recipes for subsequent runs.
use super::*;
use compile::{Compile, CompileCounters};
use recipes::{Recipe, Store};
#[derive(Clone)]
pub(super) enum Built {
    Fixed(wgpu::RenderPipeline),
    Program(program::Pipeline),
    #[cfg(target_vendor = "apple")]
    Metal(metal::pipeline::Pipeline),
}
impl Built {
    #[cfg(target_vendor = "apple")]
    pub fn metal(self) -> Result<metal::pipeline::Pipeline, Error> {
        match self {
            Self::Metal(p) => Ok(p),
            _ => Err(invalid("wrong pipeline recipe")),
        }
    }
    pub fn fixed(self) -> Result<wgpu::RenderPipeline, Error> {
        match self {
            Self::Fixed(p) => Ok(p),
            _ => Err(invalid("wrong pipeline recipe")),
        }
    }
    pub fn program(self) -> Result<program::Pipeline, Error> {
        match self {
            Self::Program(p) => Ok(p),
            _ => Err(invalid("wrong pipeline recipe")),
        }
    }
}
#[derive(Default)]
pub(super) struct Warmup {
    store: Option<Store>,
    #[cfg(target_vendor = "apple")]
    pub(super) metal: Option<metal::pipeline::Compiler>,
    jobs: HashMap<[u8; 32], Compile<Built>>,
    batch: Option<Compile<()>>,
    pub counters: CompileCounters,
    pub hits: u64,
    pub misses: u64,
    rejected: u64,
}
impl Warmup {
    pub fn open(
        store: Store,
        context: &NativeContext,
        uniform: &wgpu::Buffer,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        driver: Option<&wgpu::PipelineCache>,
    ) -> Self {
        let mut warm = Self::default();
        #[cfg(target_vendor = "apple")]
        {
            warm.metal = context
                .pending
                .lock()
                .unwrap()
                .metal
                .as_ref()
                .map(|m| m.compiler.clone());
        }
        let recipes = store.entries();
        let bytes = recipes.iter().map(|r| r.encoded().len()).sum();
        for recipe in recipes {
            let key = recipe.key();
            warm.jobs.insert(
                key,
                job(
                    recipe,
                    context,
                    uniform,
                    shader,
                    layout,
                    driver,
                    warm.counters.clone(),
                    #[cfg(target_vendor = "apple")]
                    warm.metal.clone(),
                    None,
                ),
            );
        }
        warm.batch = Some(compile::prebuild(
            bytes,
            warm.jobs.values().cloned().collect(),
        ));
        warm.store = Some(store);
        warm
    }
    pub fn pipeline(
        &mut self,
        recipe: Recipe,
        context: &NativeContext,
        uniform: &wgpu::Buffer,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        driver: Option<&wgpu::PipelineCache>,
        prepared: Option<&Program>,
    ) -> Result<Built, Error> {
        let key = recipe.key();
        // Different live program objects may request the same persisted recipe.
        let task = if let Some(task) = self.jobs.get(&key).cloned() {
            self.hits += 1;
            task
        } else {
            self.misses += 1;
            job(
                recipe.clone(),
                context,
                uniform,
                shader,
                layout,
                driver,
                self.counters.clone(),
                #[cfg(target_vendor = "apple")]
                self.metal.clone(),
                prepared.cloned(),
            )
        };
        let result = task.wait().clone().map_err(|e| invalid(&e))?;
        if let Some(store) = &mut self.store {
            store.remember(recipe);
        }
        Ok(result)
    }
    pub fn persisted(&self) -> u64 {
        self.store.as_ref().map_or(0, Store::persisted)
    }
    pub fn discarded(&self) -> u64 {
        self.rejected + self.store.as_ref().map_or(0, |s| s.discarded)
    }
    #[cfg(test)]
    pub fn drain(&self) {
        if let Some(job) = &self.batch {
            job.resolve();
        }
    }
    pub fn flush(&mut self) {
        if let Some(store) = &mut self.store {
            store.flush();
        }
    }
}
fn job(
    recipe: Recipe,
    context: &NativeContext,
    uniform: &wgpu::Buffer,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    driver: Option<&wgpu::PipelineCache>,
    counters: CompileCounters,
    #[cfg(target_vendor = "apple")] metal: Option<metal::pipeline::Compiler>,
    prepared: Option<Program>,
) -> Compile<Built> {
    let (context, uniform, shader, layout, driver) = (
        context.clone(),
        uniform.clone(),
        shader.clone(),
        layout.clone(),
        driver.cloned(),
    );
    // The archive dispatcher owns the worker reservation. Each lazy job can
    // also be claimed directly by its first draw, regardless of queue capacity.
    Compile::spawn(usize::MAX, counters, move || {
        match recipe {
            Recipe::Fixed(key) => {
                build_fixed(&context, &shader, &layout, driver.as_ref(), key).map(Built::Fixed)
            }
            Recipe::Program {
                source,
                textures,
                streams,
                raster,
            } => {
                let program = prepared.unwrap_or_else(|| Program::new(source, textures.clone()));
                let key = program::Key {
                    program: program.id,
                    textures,
                    streams,
                    raster,
                };
                #[cfg(target_vendor = "apple")]
                if let Some(compiler) = metal {
                    return compiler
                        .build(&key, &program)
                        .map(Built::Metal)
                        .map_err(|e| e.to_string());
                }
                program::Pipeline::build(&context, &uniform, driver.as_ref(), &key, &program)
                    .map(Built::Program)
            }
        }
        .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-CACHE-8, GFX-CACHE-7, GFX-CACHE-10
    #[test]
    fn prebuild_and_demand_share_the_exact_same_native_pipeline() {
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        let recipe = Recipe::Fixed(PipelineKey {
            topology: Topology::Triangles,
            cull: Cull::None,
            color_blend: None,
            alpha_blend: None,
            depth: None,
            depth_write: false,
            depth_format: None,
            stencil: None,
            color_write: 15,
        });
        let mut warm = Warmup::default();
        let task = job(
            recipe.clone(),
            &gpu.context,
            &gpu.uniform,
            &gpu.shader,
            &gpu.layout,
            None,
            warm.counters.clone(),
            #[cfg(target_vendor = "apple")]
            None,
            None,
        );
        warm.jobs.insert(recipe.key(), task.clone());
        let worker = task.clone();
        let background =
            std::thread::spawn(move || worker.resolve().clone().unwrap().fixed().unwrap());
        let demand = warm
            .pipeline(
                recipe,
                &gpu.context,
                &gpu.uniform,
                &gpu.shader,
                &gpu.layout,
                None,
                None,
            )
            .unwrap()
            .fixed()
            .unwrap();
        assert_eq!(demand, background.join().unwrap());
        assert_eq!(demand, task.resolve().clone().unwrap().fixed().unwrap());
        assert_eq!(warm.hits, 1);
    }
}
