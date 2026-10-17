//! Bounded clears use the programmable pipeline's native compiler and recipes.
use super::*;

impl Gpu {
    pub(in crate::gpu) fn clear_metal(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        state: DrawState,
    ) -> Option<Result<(), Error>> {
        if self.context.pending.lock().unwrap().metal.is_none() {
            return None;
        }
        static PROGRAM: OnceLock<Program> = OnceLock::new();
        let program = PROGRAM.get_or_init(|| {
            let mut program = Program::new(
                "struct V { @builtin(position) p:vec4<f32>, @location(0) c:vec4<f32> }; @vertex fn vertex_main(@location(0) p:vec4<f32>, @location(1) c:vec4<f32>)->V { return V(p,c); } @fragment fn fragment_main(v:V)->@location(0) vec4<f32> { return v.c; }".into(),
                vec![],
            );
            program.constants = [crate::constants::ConstantUsage::default(); 2];
            program
        });
        let draw = ProgrammableDraw {
            program: program.clone(),
            streams: vec![VertexStream {
                bytes: bytemuck::cast_slice(vertices).to_vec(),
                stride: std::mem::size_of::<Vertex>() as u32,
                attributes: vec![
                    Attribute {
                        location: 0,
                        offset: 0,
                        format: AttributeFormat::Float4,
                    },
                    Attribute {
                        location: 1,
                        offset: 16,
                        format: AttributeFormat::Float4,
                    },
                ],
            }],
            indices: None,
            vertex_count: vertices.len() as u32,
            vertex_constants: ConstantBlock::default(),
            pixel_constants: ConstantBlock::default(),
            textures: vec![],
            parameters: [[0; 4]; 40],
        };
        Some(self.draw_programmable(target, &draw, &state))
    }
}
