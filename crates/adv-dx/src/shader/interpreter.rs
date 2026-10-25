//! Scalar reference execution for differential tests. Texture access is injected.
//! Derivative instructions require a quad oracle and fail in scalar execution.
use super::*;
pub type Vec4 = [f32; 4];
#[derive(Clone, Debug)]
pub struct Inputs {
    pub v: [Vec4; 16],
    pub t: [Vec4; 8],
    pub position: Vec4,
    pub front_face: bool,
}
impl Default for Inputs {
    fn default() -> Self {
        Self {
            v: [[0.0; 4]; 16],
            t: [[0.0; 4]; 8],
            position: [0.0; 4],
            front_face: true,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Constants {
    pub f: [Vec4; 256],
    pub i: [[i32; 4]; 16],
    pub b: [[i32; 4]; 16],
    pub bump_matrix: [Vec4; 8],
    pub bump_luminance: [Vec4; 8],
}
impl Default for Constants {
    fn default() -> Self {
        Self {
            f: [[0.0; 4]; 256],
            i: [[0; 4]; 16],
            b: [[0; 4]; 16],
            bump_matrix: [[0.0; 4]; 8],
            bump_luminance: [[0.0; 4]; 8],
        }
    }
}
/// Read-only constants shared by scalar analysis and the GPU command.
pub trait ConstantSource {
    fn register(&self, n: usize) -> [u32; 4];
}
impl ConstantSource for Constants {
    fn register(&self, n: usize) -> [u32; 4] {
        match n {
            0..256 => self.f[n].map(f32::to_bits),
            256..272 => self.i[n - 256].map(|v| v as u32),
            272..288 => self.b[n - 272].map(|v| v as u32),
            288..296 => self.bump_matrix[n - 288].map(f32::to_bits),
            296..304 => self.bump_luminance[n - 296].map(f32::to_bits),
            _ => [0; 4],
        }
    }
}
#[cfg(feature = "gpu")]
impl ConstantSource for adv_device::gpu::ConstantSnapshot {
    fn register(&self, n: usize) -> [u32; 4] {
        self.register(n)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outputs {
    pub position: Vec4,
    pub colors: [Vec4; 2],
    pub texcoords: [Vec4; 8],
    pub depth: f32,
    pub point_size: f32,
    pub fog: f32,
    pub generic: [Vec4; 12],
    pub discarded: bool,
}
pub trait Sampler {
    fn sample(&mut self, index: u8, coord: Vec4, mode: u32) -> Result<Vec4, String>;
    fn sample_grad(&mut self, index: u8, coord: Vec4, dx: Vec4, dy: Vec4) -> Result<Vec4, String> {
        let _ = (index, coord, dx, dy);
        Err("gradient sampling oracle unavailable".into())
    }
}
pub struct NoTextures;
impl Sampler for NoTextures {
    fn sample(&mut self, _: u8, _: Vec4, _: u32) -> Result<Vec4, String> {
        Err("texture oracle unavailable".into())
    }
}
struct State<'a, C> {
    r: [Vec4; 32],
    t: [Vec4; 8],
    a: Vec4,
    al: i32,
    p: Vec4,
    ro: [Vec4; 3],
    od: [Vec4; 2],
    o: [Vec4; 12],
    oc: [Vec4; 2],
    depth: f32,
    constants: &'a C,
    definitions: &'a [(Register, [u32; 4])],
    matrix: [f32; 3],
    matrix_n: usize,
}
impl<C> Copy for State<'_, C> {}
impl<C> Clone for State<'_, C> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<C: ConstantSource> State<'_, C> {
    fn constant(&self, kind: RegisterKind, n: usize) -> [u32; 4] {
        let (offset, count) = match kind {
            RegisterKind::Float => (0, 256),
            RegisterKind::Int => (256, 16),
            RegisterKind::Bool => (272, 16),
            _ => unreachable!(),
        };
        if n >= count {
            return [0; 4];
        }
        self.definitions
            .iter()
            .rev()
            .find(|(r, _)| r.kind == kind && r.index as usize == n)
            .map_or_else(|| self.constants.register(offset + n), |(_, value)| *value)
    }
    fn index(&self, r: Register, rel: Option<Relative>, offset: u16) -> i32 {
        i32::from(r.index)
            .wrapping_add(i32::from(offset))
            .wrapping_add(rel.map_or(0, |rel| {
                if rel.register.kind == RegisterKind::Loop {
                    self.al
                } else {
                    self.a[rel.component as usize] as i32
                }
            }))
    }
    fn read(&self, r: Register, rel: Option<Relative>, input: &Inputs, offset: u16) -> Vec4 {
        let index = self.index(r, rel, offset);
        if index < 0 {
            return [0.0; 4];
        }
        let n = index as usize;
        use RegisterKind::*;
        match r.kind {
            Temp => self.r.get(n).copied().unwrap_or_default(),
            Input => input.v.get(n).copied().unwrap_or_default(),
            Float => self.constant(Float, n).map(f32::from_bits),
            Address => {
                if n == 0 && rel.is_none() {
                    self.a
                } else {
                    self.t.get(n).copied().unwrap_or_default()
                }
            }
            RasterOutput => self.ro.get(n).copied().unwrap_or_default(),
            ColorOutputLegacy => self.od.get(n).copied().unwrap_or_default(),
            Output => self.o.get(n).copied().unwrap_or_default(),
            Int => self.constant(Int, n).map(|v| v as i32 as f32),
            Bool => self.constant(Bool, n).map(|v| v as i32 as f32),
            ColorOutput => self.oc.get(n).copied().unwrap_or_default(),
            DepthOutput => [self.depth; 4],
            Loop => [self.al as f32; 4],
            Predicate => self.p,
            Misc => {
                if n == 0 {
                    input.position
                } else {
                    [if input.front_face { 1.0 } else { -1.0 }; 4]
                }
            }
            Sampler | Label => [0.0; 4],
        }
    }
    fn source(&self, src: &Source, input: &Inputs, s: &Shader, offset: u16) -> Vec4 {
        let mut raw = if src.register.kind == RegisterKind::Address && s.stage == Stage::Pixel {
            self.t[src.register.index as usize]
        } else {
            self.read(src.register, src.relative, input, offset)
        };
        if src.relative.is_some()
            && src.register.kind == RegisterKind::Float
            && s.stage == Stage::Pixel
        {
            let n = self.index(src.register, src.relative, offset);
            let bound = if s.version.major == 3 { 224 } else { 32 };
            if n < 0 || n >= bound {
                raw = [0.0; 4];
            }
        }
        if s.stage == Stage::Pixel && s.version.major == 1 {
            if src.register.kind == RegisterKind::Float {
                raw = raw.map(|x| x.clamp(-1.0, 1.0));
            } else if src.register.kind == RegisterKind::Input {
                raw = raw.map(|x| x.clamp(0.0, 1.0));
            }
        }
        let mut v = src.swizzle.map(|c| raw[c as usize]);
        use SourceModifier::*;
        let dz = v[2];
        let dw = v[3];
        for x in &mut v {
            *x = match src.modifier {
                None => *x,
                Negate => -*x,
                Bias => *x - 0.5,
                BiasNegate => 0.5 - *x,
                Sign => *x * 2.0 - 1.0,
                SignNegate => 1.0 - *x * 2.0,
                Complement => 1.0 - *x,
                X2 => *x * 2.0,
                X2Negate => *x * (-2.0),
                DivideZ => *x / dz,
                DivideW => *x / dw,
                Abs => x.abs(),
                AbsNegate => -x.abs(),
                Not => f32::from(*x == 0.0),
            };
        }
        v
    }
    fn write(&mut self, d: &Destination, mut v: Vec4, s: &Shader) {
        for x in &mut v {
            *x *= 2.0f32.powi(i32::from(d.shift));
            if d.saturate {
                *x = x.clamp(0.0, 1.0);
            }
            if s.stage == Stage::Pixel
                && s.version.major == 1
                && d.register.kind == RegisterKind::Temp
            {
                *x = x.clamp(-8.0, 8.0);
            }
        }
        let n = self.index(d.register, d.relative, 0);
        if n < 0 {
            return;
        }
        let n = n as usize;
        use RegisterKind::*;
        let target = match d.register.kind {
            Temp => self.r.get_mut(n),
            Address => {
                if s.stage == Stage::Vertex {
                    Some(&mut self.a)
                } else {
                    self.t.get_mut(n)
                }
            }
            RasterOutput => self.ro.get_mut(n),
            ColorOutputLegacy => self.od.get_mut(n),
            Output => self.o.get_mut(n),
            ColorOutput => self.oc.get_mut(n),
            Predicate => Some(&mut self.p),
            DepthOutput => {
                self.depth = v[0];
                None
            }
            _ => None,
        };
        if let Some(target) = target {
            for i in 0..4 {
                if d.mask & (1 << i) != 0 {
                    target[i] = v[i];
                }
            }
        }
    }
}
/// Run one invocation. Keep the accepted semantic boundary identical to
/// translation: callers run only shaders whose (cached) translation succeeded,
/// since translating here would repeat it for every vertex.
pub fn run<C: ConstantSource>(
    shader: &Shader,
    input: &Inputs,
    constants: &C,
    sampler: &mut dyn Sampler,
) -> Result<Outputs, Error> {
    let definitions: Vec<_> = shader
        .instructions
        .iter()
        .filter_map(|ins| {
            let destination = ins.destination.as_ref()?;
            let value = match ins.opcode {
                Opcode::Def | Opcode::Defi => ins.immediate.as_slice().try_into().unwrap(),
                Opcode::Defb => [u32::from(ins.immediate[0] != 0); 4],
                _ => return None,
            };
            Some((destination.register, value))
        })
        .collect();
    let mut st = State {
        r: [[0.0; 4]; 32],
        t: input.t,
        a: [0.0; 4],
        al: 0,
        p: [0.0; 4],
        ro: [[0.0; 4]; 3],
        od: [[0.0; 4]; 2],
        o: [[0.0; 4]; 12],
        oc: [[0.0; 4]; 2],
        depth: 0.0,
        constants,
        definitions: &definitions,
        matrix: [0.0; 3],
        matrix_n: 0,
    };
    let mut pc = 0usize;
    let mut steps = 0;
    let mut calls = Vec::new();
    let mut loops: Vec<(i32, i32, i32)> = Vec::new();
    let mut discarded = false;
    let mut snapshot = st;
    while pc < shader.instructions.len() {
        if steps >= MAX_EXECUTION_STEPS {
            return Err(Error::new(
                shader.instructions[pc].offset,
                "reference execution step limit exceeded",
            ));
        }
        steps += 1;
        let ins = &shader.instructions[pc];
        let flow = shader.flow[pc];
        pc += 1;
        if ins
            .predicate
            .as_ref()
            .is_some_and(|p| st.source(p, input, shader, 0)[0] == 0.0)
        {
            continue;
        }
        if !ins.coissue {
            snapshot = st;
        }
        let reader = if ins.coissue { &snapshot } else { &st };
        let v: Vec<Vec4> = ins
            .sources
            .iter()
            .map(|p| reader.source(p, input, shader, 0))
            .collect();
        let a = v.first().copied().unwrap_or_default();
        let b = v.get(1).copied().unwrap_or_default();
        let c = v.get(2).copied().unwrap_or_default();
        let mut result = None;
        use Opcode::*;
        match ins.opcode {
            Nop | Dcl | Def | Defi | Defb | Label | Phase | EndIf => {}
            If => {
                if a[0] == 0.0 {
                    pc = flow.target;
                }
            }
            Ifc => {
                if !compare(a[0], b[0], ins.controls) {
                    pc = flow.target;
                }
            }
            Else => pc = flow.target,
            Call | CallNz => {
                if ins.opcode == Call || b[0] != 0.0 {
                    if calls.len() >= MAX_FLOW_DEPTH {
                        return Err(Error::new(ins.offset, "call stack exhausted"));
                    }
                    calls.push(pc);
                    pc = flow.target;
                }
            }
            Ret => {
                if let Some(next) = calls.pop() {
                    pc = next;
                } else {
                    break;
                }
            }
            Loop | Rep => {
                let ctr = if ins.opcode == Loop { b } else { a };
                if ctr[0] <= 0.0 {
                    pc = flow.end;
                } else {
                    loops.push((ctr[0] as i32, ctr[2] as i32, st.al));
                    if ins.opcode == Loop {
                        st.al = ctr[1] as i32;
                    }
                }
            }
            EndLoop | EndRep => {
                let entry = loops
                    .last_mut()
                    .ok_or_else(|| Error::new(ins.offset, "empty loop stack"))?;
                entry.0 -= 1;
                if entry.0 > 0 {
                    if ins.opcode == EndLoop {
                        st.al = st.al.wrapping_add(entry.1);
                    }
                    pc = flow.target + 1;
                } else {
                    st.al = entry.2;
                    loops.pop();
                }
            }
            Break | Breakc | Breakp => {
                if ins.opcode == Break
                    || ins.opcode == Breakp && a[0] != 0.0
                    || ins.opcode == Breakc && compare(a[0], b[0], ins.controls)
                {
                    st.al = loops
                        .pop()
                        .ok_or_else(|| Error::new(ins.offset, "empty loop stack"))?
                        .2;
                    pc = flow.target;
                }
            }
            Mov => {
                result = Some(
                    if shader.stage == Stage::Vertex
                        && shader.version.major == 1
                        && ins
                            .destination
                            .as_ref()
                            .is_some_and(|d| d.register.kind == RegisterKind::Address)
                    {
                        a.map(f32::round)
                    } else {
                        a
                    },
                )
            }
            Add => result = Some(zip(a, b, |x, y| x + y)),
            Sub => result = Some(zip(a, b, |x, y| x - y)),
            Mad => result = Some(zip(zip(a, b, |x, y| x * y), c, |x, y| x + y)),
            Mul => result = Some(zip(a, b, |x, y| x * y)),
            Rcp => result = Some([rcp(a[0]); 4]),
            Rsq => {
                result = Some(
                    [if a[0] == 0.0 {
                        f32::INFINITY
                    } else {
                        1.0 / a[0].abs().sqrt()
                    }; 4],
                )
            }
            Dp3 => result = Some([dot(a, b, 3); 4]),
            Dp4 => result = Some([dot(a, b, 4); 4]),
            Min => result = Some(zip(a, b, f32::min)),
            Max => result = Some(zip(a, b, f32::max)),
            Slt => result = Some(zip(a, b, |x, y| f32::from(x < y))),
            Sge => result = Some(zip(a, b, |x, y| f32::from(x >= y))),
            Exp => result = Some([a[0].exp2(); 4]),
            Log => result = Some([a[0].abs().log2(); 4]),
            Lit => {
                result = Some([
                    1.0,
                    a[0].max(0.0),
                    if a[0] > 0.0 {
                        a[1].max(0.0).powf(a[3].clamp(-128.0, 128.0))
                    } else {
                        0.0
                    },
                    1.0,
                ])
            }
            Dst => result = Some([1.0, a[1] * b[1], a[2], b[3]]),
            Lrp => result = Some(std::array::from_fn(|i| a[i] * b[i] + (1.0 - a[i]) * c[i])),
            Frc => result = Some(a.map(|x| x - x.floor())),
            Pow => result = Some([a[0].abs().powf(b[0]); 4]),
            Crs => {
                result = Some([
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                    0.0,
                ])
            }
            Sgn => {
                result = Some(a.map(|x| {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        0.0
                    }
                }))
            }
            Abs => result = Some(a.map(f32::abs)),
            Nrm => {
                let norm = dot(a, a, 3).sqrt();
                result = Some([a[0] / norm, a[1] / norm, a[2] / norm, a[3]]);
            }
            SinCos => result = Some([a[0].cos(), a[0].sin(), 0.0, 0.0]),
            Mova => result = Some(a.map(f32::round)),
            Cmp => {
                result = Some(std::array::from_fn(
                    |i| if a[i] >= 0.0 { b[i] } else { c[i] },
                ))
            }
            Cnd => {
                result = Some(std::array::from_fn(|i| {
                    if a[if shader.version.minor < 4 { 3 } else { i }] > 0.5 {
                        b[i]
                    } else {
                        c[i]
                    }
                }))
            }
            Dp2add => result = Some([dot(a, b, 2) + c[0]; 4]),
            Setp => result = Some(zip(a, b, |x, y| f32::from(compare(x, y, ins.controls)))),
            M4x4 | M4x3 | M3x4 | M3x3 | M3x2 => {
                let n = match ins.opcode {
                    M4x4 | M3x4 => 4,
                    M4x3 | M3x3 => 3,
                    _ => 2,
                };
                let width = if matches!(ins.opcode, M4x4 | M4x3) {
                    4
                } else {
                    3
                };
                let mut matrix_result = [0.0; 4];
                for (row, x) in matrix_result.iter_mut().enumerate().take(n) {
                    *x = dot(
                        a,
                        st.source(&ins.sources[1], input, shader, row as u16),
                        width,
                    );
                }
                result = Some(matrix_result);
            }
            Expp => {
                result = Some(
                    if shader.stage == Stage::Vertex && shader.version.major == 1 {
                        [a[0].floor().exp2(), a[0] - a[0].floor(), a[0].exp2(), 1.0]
                    } else {
                        [a[0].exp2(); 4]
                    },
                )
            }
            Logp => {
                let x = a[0].abs().log2();
                result = Some(
                    if shader.stage == Stage::Vertex && shader.version.major == 1 {
                        [x.floor(), a[0].abs() / x.floor().exp2(), x, 1.0]
                    } else {
                        [x; 4]
                    },
                );
            }
            Dsx | Dsy => {
                return Err(Error::new(
                    ins.offset,
                    "scalar reference requires a quad derivative oracle",
                ));
            }
            Texcoord => {
                let d = ins.destination.as_ref().expect("texcoord");
                result = Some(if ins.sources.is_empty() {
                    let q = input.t[d.register.index as usize];
                    [
                        q[0].clamp(0.0, 1.0),
                        q[1].clamp(0.0, 1.0),
                        q[2].clamp(0.0, 1.0),
                        1.0,
                    ]
                } else {
                    a
                });
            }
            Texkill => {
                let d = ins.destination.as_ref().expect("texkill");
                let q = if d.register.kind == RegisterKind::Address && shader.stage == Stage::Pixel
                {
                    st.t[d.register.index as usize]
                } else {
                    st.read(d.register, d.relative, input, 0)
                };
                if q[..3].iter().any(|x| *x < 0.0) {
                    discarded = true;
                    break;
                }
            }
            Tex | Texldl | Texldd => {
                let d = ins.destination.as_ref().expect("texture");
                let index = if shader.version.major == 1 {
                    d.register.index
                } else {
                    ins.sources[1].register.index
                };
                let coord = if ins.sources.is_empty() {
                    st.t[d.register.index as usize]
                } else {
                    a
                };
                let sampled = if ins.opcode == Texldd {
                    sampler.sample_grad(index as u8, coord, v[2], v[3])
                } else {
                    sampler.sample(
                        index as u8,
                        coord,
                        if ins.opcode == Texldl {
                            3
                        } else {
                            u32::from(ins.controls)
                        },
                    )
                };
                result = Some(sampled.map_err(|e| Error::new(ins.offset, e))?);
            }
            Texreg2ar | Texreg2gb | Texreg2rgb | Texdp3tex => {
                let d = ins.destination.as_ref().expect("texture");
                let coord = match ins.opcode {
                    Texreg2ar => [a[3], a[0], 0.0, 1.0],
                    Texreg2gb => [a[1], a[2], 0.0, 1.0],
                    Texreg2rgb => [a[0], a[1], a[2], 1.0],
                    _ => [dot(st.t[d.register.index as usize], a, 3), 0.0, 0.0, 1.0],
                };
                result = Some(
                    sampler
                        .sample(d.register.index as u8, coord, 0)
                        .map_err(|e| Error::new(ins.offset, e))?,
                );
            }
            Texdp3 => {
                let d = ins.destination.as_ref().expect("dot");
                result = Some([dot(st.t[d.register.index as usize], a, 3); 4]);
            }
            Texm3x2pad | Texm3x3pad => {
                let d = ins.destination.as_ref().expect("matrix");
                st.matrix[st.matrix_n] = dot(input.t[d.register.index as usize], a, 3);
                st.matrix_n = (st.matrix_n + 1).min(2);
            }
            Texm3x2tex | Texm3x3tex | Texm3x3 | Texm3x3spec | Texm3x3vspec | Texm3x2depth => {
                let d = ins.destination.as_ref().expect("matrix");
                let n = d.register.index as usize;
                let z = dot(input.t[n], a, 3);
                let mut coord = if matches!(ins.opcode, Texm3x2tex | Texm3x2depth) {
                    [st.matrix[0], z, 0.0, 1.0]
                } else {
                    [st.matrix[0], st.matrix[1], z, 1.0]
                };
                if matches!(ins.opcode, Texm3x3spec | Texm3x3vspec) {
                    let eye = if ins.opcode == Texm3x3spec {
                        b
                    } else {
                        [input.t[n - 2][3], input.t[n - 1][3], input.t[n][3], 1.0]
                    };
                    let factor = 2.0 * dot(coord, eye, 3) / dot(coord, coord, 3);
                    for j in 0..3 {
                        coord[j] = factor * coord[j] - eye[j];
                    }
                }
                if ins.opcode == Texm3x2depth {
                    st.depth = (st.matrix[0] * rcp(z)).clamp(0.0, 1.0);
                } else if ins.opcode == Texm3x3 {
                    result = Some(coord);
                } else {
                    result = Some(
                        sampler
                            .sample(d.register.index as u8, coord, 0)
                            .map_err(|e| Error::new(ins.offset, e))?,
                    );
                }
                st.matrix_n = 0;
            }
            Texdepth => {
                let d = ins.destination.as_ref().expect("depth");
                let q = st.r[d.register.index as usize];
                st.depth = (q[0] * rcp(q[1])).clamp(0.0, 1.0);
            }
            Texbem | Texbeml | Bem => {
                let d = ins.destination.as_ref().expect("bump");
                let n = d.register.index as usize;
                let m = st.constants.register(288 + n).map(f32::from_bits);
                let (base, perturb) = if ins.opcode == Bem {
                    (a, b)
                } else {
                    (input.t[n], a)
                };
                let coord = [
                    base[0] + m[0] * perturb[0] + m[2] * perturb[1],
                    base[1] + m[1] * perturb[0] + m[3] * perturb[1],
                    base[2],
                    base[3],
                ];
                if ins.opcode == Bem {
                    result = Some(coord);
                } else {
                    let mut sampled = sampler
                        .sample(n as u8, coord, 0)
                        .map_err(|e| Error::new(ins.offset, e))?;
                    if ins.opcode == Texbeml {
                        let lum = st.constants.register(296 + n).map(f32::from_bits);
                        for x in &mut sampled {
                            *x *= perturb[2] * lum[0] + lum[1];
                        }
                    }
                    result = Some(sampled);
                }
            }
        }
        if let Some(value) = result {
            st.write(
                ins.destination
                    .as_ref()
                    .ok_or_else(|| Error::new(ins.offset, "missing destination"))?,
                value,
                shader,
            );
        }
    }
    let mut output = Outputs {
        position: st.ro[0],
        colors: st.od,
        texcoords: std::array::from_fn(|i| st.o[i]),
        depth: st.depth,
        point_size: st.ro[2][0],
        fog: if shader.instructions.iter().any(|i| {
            i.destination.as_ref().is_some_and(|d| {
                d.register.kind == RegisterKind::RasterOutput && d.register.index == 1
            })
        }) {
            st.ro[1][0]
        } else {
            1.0
        },
        discarded,
        generic: st.o,
    };
    if shader.stage == Stage::Pixel {
        output.colors = if shader.version.major == 1 {
            [st.r[0], [0.0; 4]]
        } else {
            st.oc
        };
    }
    if shader.stage == Stage::Vertex && shader.version.major == 3 {
        output.position = [0.0; 4];
        output.colors = [[0.0; 4]; 2];
        output.texcoords = [[0.0; 4]; 8];
        for d in &shader.outputs {
            let value = st.o[d.register.index as usize];
            let target = match d.usage {
                0 => Some(&mut output.position),
                4 => {
                    output.point_size = value[0];
                    None
                }
                11 => {
                    output.fog = value[0];
                    None
                }
                5 => Some(&mut output.texcoords[d.index as usize]),
                10 => Some(&mut output.colors[d.index as usize]),
                _ => None,
            };
            if let Some(target) = target {
                for i in 0..4 {
                    if d.mask & (1 << i) != 0 {
                        target[i] = value[i];
                    }
                }
            }
        }
    }
    Ok(output)
}
fn zip(a: Vec4, b: Vec4, f: impl Fn(f32, f32) -> f32) -> Vec4 {
    std::array::from_fn(|i| f(a[i], b[i]))
}
fn dot(a: Vec4, b: Vec4, n: usize) -> f32 {
    (0..n).map(|i| a[i] * b[i]).sum()
}
fn rcp(x: f32) -> f32 {
    if x == 0.0 { f32::INFINITY } else { 1.0 / x }
}
fn compare(x: f32, y: f32, control: u8) -> bool {
    match control {
        1 => x > y,
        2 => x == y,
        3 => x >= y,
        4 => x < y,
        5 => x != y,
        6 => x <= y,
        _ => false,
    }
}
