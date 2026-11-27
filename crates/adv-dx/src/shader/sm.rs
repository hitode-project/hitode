//! Direct3D 9 shader models 1 to 3 on the shared register IR.
//!
//! A parsed program lowers once to a `Model::Legacy` module. The shared WGSL
//! back end emits the renderer's stage library from it and the shared scalar
//! interpreter executes it for bounds proofs, so both apply the same arithmetic.
//!
//! The lowering expands the D3D9 register files onto fixed IR registers:
//!
//! | IR register | D3D9 |
//! |---|---|
//! | Temp 0–31 | r0–r31 |
//! | Temp 32, 33, 34 | a0, aL (x) and the loop iteration guard (y), p0 |
//! | Temp 35–42, 43 | writable PS 1.x t0–t7, the texm3x* matrix rows |
//! | Temp 44… | per-loop count (x), step (y) and saved aL (z), then scratch |
//! | Input 0–15, 16–23, 24, 25 | v0–v15, texture coordinates, vPos, vFace |
//! | Output 0–2, 3–4, 5–16, 17–18, 19 | oPos/oFog/oPts, oD0–1, oT0–7 or o0–o11, oC0–1, oDepth |
//! | Constant slots 0–4 | c, i, b, bump matrices, bump luminance |
//!
//! Source modifiers beyond negation and absolute value, the PS 1.x clamps,
//! destination shifts and predication become explicit instructions.
use super::ir::{self, Components, Index, OperandValue, Statement};
use super::{
    Destination, Error, Instruction, MAX_EXECUTION_STEPS, MAX_TRANSLATION_BYTES, Opcode, Register,
    RegisterKind, Relative, SamplerBinding, SamplerDimension, Shader, Source, SourceModifier,
    Stage, Translation, Version,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const INPUT_TEXCOORD: u32 = 16;
const INPUT_POSITION: u32 = 24;
const INPUT_FACE: u32 = 25;
const INPUT_COUNT: u32 = 26;
const OUTPUT_COLOR: u32 = 3;
const OUTPUT_GENERIC: u32 = 5;
const OUTPUT_TARGET: u32 = 17;
const OUTPUT_DEPTH: u32 = 19;
const OUTPUT_COUNT: u32 = 20;
const FLOATS: u32 = 0;
const INTEGERS: u32 = 1;
const BOOLEANS: u32 = 2;
const BUMP_MATRIX: u32 = 3;
const BUMP_LUMINANCE: u32 = 4;
const ADDRESS: u32 = 32;
const LOOP: u32 = 33;
const PREDICATE: u32 = 34;
const TEXTURE: u32 = 35;
const MATRIX: u32 = 43;
const FIRST_LOOP: u32 = 44;
const ONE: u32 = 0x3f80_0000;
const INFINITY: u32 = 0x7f80_0000;

/// A field of the renderer's `ShaderOutputs`, read from an output register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Position,
    Color(usize),
    Texcoord(usize),
    Generic(usize),
    Depth,
    PointSize,
    Fog,
}

#[derive(Clone, Copy, Debug)]
struct Field {
    target: Target,
    register: u32,
    mask: u8,
}

/// The outputs as the renderer reads them, in assignment order. Fog is 1 unless assigned.
fn fields(shader: &Shader) -> Vec<Field> {
    let field = |target, register, mask| Field {
        target,
        register,
        mask,
    };
    let mut fields = Vec::new();
    if shader.stage == Stage::Vertex && shader.version.major == 3 {
        for k in 0..12 {
            fields.push(field(Target::Generic(k), OUTPUT_GENERIC + k as u32, 15));
        }
        fields.push(field(Target::Depth, OUTPUT_DEPTH, 1));
        for d in &shader.outputs {
            let register = OUTPUT_GENERIC + u32::from(d.register.index);
            let target = match d.usage {
                0 => Target::Position,
                4 => Target::PointSize,
                5 if d.index < 8 => Target::Texcoord(usize::from(d.index)),
                10 if d.index < 2 => Target::Color(usize::from(d.index)),
                11 => Target::Fog,
                _ => continue,
            };
            let mask = if matches!(target, Target::PointSize | Target::Fog) {
                1
            } else {
                d.mask
            };
            fields.push(field(target, register, mask));
        }
        return fields;
    }
    fields.push(field(Target::Position, 0, 15));
    fields.push(field(Target::Color(0), OUTPUT_COLOR, 15));
    fields.push(field(Target::Color(1), OUTPUT_COLOR + 1, 15));
    for k in 0..8 {
        fields.push(field(Target::Texcoord(k), OUTPUT_GENERIC + k as u32, 15));
    }
    for k in 0..12 {
        fields.push(field(Target::Generic(k), OUTPUT_GENERIC + k as u32, 15));
    }
    fields.push(field(Target::Depth, OUTPUT_DEPTH, 1));
    fields.push(field(Target::PointSize, 2, 1));
    let fog = shader.instructions.iter().any(|i| {
        i.destination
            .as_ref()
            .is_some_and(|d| d.register.kind == RegisterKind::RasterOutput && d.register.index == 1)
    });
    if fog {
        fields.push(field(Target::Fog, 1, 1));
    }
    if shader.stage == Stage::Pixel {
        fields.push(field(Target::Color(0), OUTPUT_TARGET, 15));
        fields.push(field(Target::Color(1), OUTPUT_TARGET + 1, 15));
    }
    fields
}

fn result_wgsl(fields: &[Field]) -> String {
    let mut out = String::from("result.fog = 1.0;\n");
    for f in fields {
        let name = match f.target {
            Target::Position => "result.position".to_string(),
            Target::Color(k) => format!("result.colors[{k}]"),
            Target::Texcoord(k) => format!("result.texcoords[{k}]"),
            Target::Generic(k) => format!("result.generic[{k}]"),
            Target::Depth => "result.depth".to_string(),
            Target::PointSize => "result.point_size".to_string(),
            Target::Fog => "result.fog".to_string(),
        };
        let register = f.register;
        if matches!(f.target, Target::Depth | Target::PointSize | Target::Fog) {
            out.push_str(&format!("{name} = bitcast<f32>(o[{register}].x);\n"));
        } else if f.mask == 15 {
            out.push_str(&format!("{name} = bitcast<vec4<f32>>(o[{register}]);\n"));
        } else {
            for (lane, c) in LANES.iter().enumerate() {
                if f.mask & (1 << lane) != 0 {
                    out.push_str(&format!("{name}.{c} = bitcast<f32>(o[{register}].{c});\n"));
                }
            }
        }
    }
    out
}

const LANES: [char; 4] = ['x', 'y', 'z', 'w'];

/// A source value: an IR operand before its components are chosen.
#[derive(Clone, Debug)]
struct Value {
    value: OperandValue,
    swizzle: [u8; 4],
    modifier: ir::Modifier,
}

impl Value {
    fn new(value: OperandValue) -> Self {
        Self {
            value,
            swizzle: [0, 1, 2, 3],
            modifier: ir::Modifier::None,
        }
    }

    fn operand(&self) -> ir::Operand {
        operand(
            self.value.clone(),
            Components::Swizzle(self.swizzle),
            self.modifier,
        )
    }

    /// The value's lane `lane` in every component.
    fn lane(&self, lane: usize) -> ir::Operand {
        operand(
            self.value.clone(),
            Components::Select(self.swizzle[lane]),
            self.modifier,
        )
    }

    fn swizzled(&self, swizzle: [u8; 4]) -> Self {
        Self {
            swizzle: swizzle.map(|lane| self.swizzle[lane as usize]),
            ..self.clone()
        }
    }

    fn negated(&self) -> Self {
        use ir::Modifier::*;
        let modifier = match self.modifier {
            None => Negate,
            Negate => None,
            Absolute => AbsoluteNegate,
            AbsoluteNegate => Absolute,
        };
        Self {
            modifier,
            ..self.clone()
        }
    }

    fn absolute(&self) -> Self {
        Self {
            modifier: ir::Modifier::Absolute,
            ..self.clone()
        }
    }
}

fn operand(value: OperandValue, components: Components, modifier: ir::Modifier) -> ir::Operand {
    ir::Operand {
        value,
        components,
        modifier,
        precision: ir::Precision::Default,
        non_uniform: false,
    }
}

fn register(register: ir::Register, index: u32) -> OperandValue {
    OperandValue::Register {
        register,
        indices: vec![Index::Direct(u64::from(index))],
    }
}

fn temp(index: u32) -> OperandValue {
    register(ir::Register::Temp, index)
}

fn input(index: u32) -> OperandValue {
    register(ir::Register::Input, index)
}

fn output(index: u32) -> OperandValue {
    register(ir::Register::Output, index)
}

fn bits(value: [u32; 4]) -> Value {
    Value::new(OperandValue::Immediate(value))
}

fn float(value: f32) -> Value {
    bits([value.to_bits(); 4])
}

fn int(value: i32) -> Value {
    bits([value as u32; 4])
}

fn constant(slot: u32, row: u32) -> OperandValue {
    OperandValue::Constant {
        slot: Index::Direct(u64::from(slot)),
        byte_offset: Index::Direct(u64::from(row) * 16),
    }
}

fn mask(value: OperandValue, mask: u8) -> ir::Operand {
    operand(value, Components::Mask(mask), ir::Modifier::None)
}

fn instruction(
    opcode: ir::Opcode,
    destinations: Vec<ir::Operand>,
    sources: Vec<ir::Operand>,
) -> ir::Instruction {
    ir::Instruction {
        opcode,
        destinations,
        sources,
        saturate: false,
        test_nonzero: true,
        precise: 0,
        controls: 0,
        resource: ir::ResourceOperands::default(),
        interface_function: None,
        flush_inputs: false,
        flush_outputs: false,
    }
}

/// Where an instruction leaves its value.
struct Write {
    register: OperandValue,
    mask: u8,
    saturate: bool,
    /// The destination when the value needs further work before it lands there.
    finish: Option<Destination>,
}

impl Write {
    fn operand(&self) -> ir::Operand {
        mask(self.register.clone(), self.mask)
    }
}

enum Frame {
    If {
        test: ir::Instruction,
        accept: Option<ir::Block>,
    },
    Loop {
        opcode: Opcode,
        counter: Source,
        state: u32,
    },
}

pub(crate) struct Lowered {
    pub module: ir::Module,
    inputs: Vec<u32>,
    fields: Vec<Field>,
}

pub(crate) fn lower(shader: &Shader) -> Result<Lowered, Error> {
    let loops = shader
        .instructions
        .iter()
        .filter(|i| matches!(i.opcode, Opcode::Loop | Opcode::Rep))
        .count() as u32;
    let mut definitions = HashMap::new();
    for ins in &shader.instructions {
        let value = match ins.opcode {
            Opcode::Def | Opcode::Defi => ins.immediate[..4].try_into().unwrap(),
            Opcode::Defb => [u32::from(ins.immediate[0] != 0); 4],
            _ => continue,
        };
        let d = ins.destination.as_ref().expect("definition destination");
        definitions.insert((d.register.kind, d.register.index), value);
    }
    // Relatively addressed float constants read defined values from a table:
    // rows 0..n hold the values and rows n..2n flag the defined ones.
    let relative = shader.instructions.iter().any(|i| {
        i.sources
            .iter()
            .any(|s| s.register.kind == RegisterKind::Float && s.relative.is_some())
    });
    let defined: BTreeMap<u16, [u32; 4]> = definitions
        .iter()
        .filter(|((kind, _), _)| *kind == RegisterKind::Float)
        .map(|((_, index), value)| (*index, *value))
        .collect();
    let defined_rows = match defined.last_key_value() {
        Some((&last, _)) if relative => u32::from(last) + 1,
        _ => 0,
    };
    let mut table = vec![[0; 4]; defined_rows as usize * 2];
    if defined_rows != 0 {
        for (&n, &value) in &defined {
            table[usize::from(n)] = value;
            table[defined_rows as usize + usize::from(n)] = [u32::MAX, 0, 0, 0];
        }
    }
    let scratch = FIRST_LOOP + loops;
    let mut lower = Lower {
        shader,
        definitions,
        defined_rows,
        scratch,
        next: scratch,
        high: scratch,
        loops: FIRST_LOOP,
        blocks: vec![Vec::new()],
        frames: Vec::new(),
        matrix: 0,
        snapshot: HashMap::new(),
        coissue: false,
        offset: 0,
        inputs: BTreeSet::new(),
        textures: BTreeSet::new(),
        outputs_read: BTreeSet::new(),
    };
    for (pc, ins) in shader.instructions.iter().enumerate() {
        lower.offset = ins.offset;
        if !ins.coissue {
            lower.next = lower.scratch;
            lower.snapshot.clear();
            lower.coissue = false;
            lower
                .take_snapshot(&shader.instructions[pc + 1..])
                .map_err(|e| lower.error(e))?;
        } else {
            lower.coissue = true;
        }
        lower.instruction(ins).map_err(|e| lower.error(e))?;
    }
    if !lower.frames.is_empty() || lower.blocks.len() != 1 {
        return Err(Error::new(lower.offset, "unterminated flow block"));
    }
    if lower.pixel1() {
        // PS 1.x has no flow control and returns r0 as its color.
        lower.exec(
            ir::Opcode::Mov,
            vec![mask(output(OUTPUT_TARGET), 15)],
            vec![Value::new(temp(0)).operand()],
        );
    }
    let mut prologue = vec![Statement::Declare(ir::Declaration {
        opcode: ir::Opcode::DclTemps,
        operand: None,
        controls: 0,
        data: vec![lower.high],
        binding: None,
        interpolation: None,
    })];
    if !table.is_empty() {
        prologue.push(Statement::ImmediateConstants(table));
    }
    for &n in &lower.outputs_read {
        prologue.push(Statement::Execute(instruction(
            ir::Opcode::Mov,
            vec![mask(output(n), 15)],
            vec![bits([0; 4]).operand()],
        )));
    }
    for &n in &lower.textures {
        lower.inputs.insert(INPUT_TEXCOORD + n);
        prologue.push(Statement::Execute(instruction(
            ir::Opcode::Mov,
            vec![mask(temp(TEXTURE + n), 15)],
            vec![Value::new(input(INPUT_TEXCOORD + n)).operand()],
        )));
    }
    let mut body = prologue;
    body.append(&mut lower.blocks.pop().unwrap());
    let module = ir::Module {
        stage: match shader.stage {
            Stage::Vertex => ir::Stage::Vertex,
            Stage::Pixel => ir::Stage::Pixel,
        },
        model: ir::Model::Legacy,
        signatures: ir::Signatures::default(),
        body,
        feature_flags: 0,
        statistics: Vec::new(),
        reflection: None,
    };
    ir::verify(&module).map_err(|e| Error::new(0, format!("lowered shader: {e}")))?;
    Ok(Lowered {
        module,
        inputs: lower.inputs.into_iter().collect(),
        fields: fields(shader),
    })
}

/// A lowering failure, before its instruction offset is attached.
type Lowering<T = ()> = Result<T, &'static str>;

struct Lower<'a> {
    shader: &'a Shader,
    /// The last definition of each register in program order.
    definitions: HashMap<(RegisterKind, u16), [u32; 4]>,
    /// Rows of the defined-constant table, zero without relative float reads.
    defined_rows: u32,
    scratch: u32,
    next: u32,
    high: u32,
    loops: u32,
    blocks: Vec<ir::Block>,
    frames: Vec<Frame>,
    /// texm3x2pad and texm3x3pad rows seen since the last matrix instruction.
    matrix: usize,
    /// Pre-instruction copies of registers that co-issued instructions read.
    snapshot: HashMap<(RegisterKind, u16), u32>,
    coissue: bool,
    offset: usize,
    inputs: BTreeSet<u32>,
    textures: BTreeSet<u32>,
    outputs_read: BTreeSet<u32>,
}

impl Lower<'_> {
    fn error(&self, message: &'static str) -> Error {
        Error::new(self.offset, message)
    }

    fn version(&self) -> Version {
        self.shader.version
    }

    fn pixel1(&self) -> bool {
        self.shader.stage == Stage::Pixel && self.version().major == 1
    }

    fn alloc(&mut self) -> u32 {
        let n = self.next;
        self.next += 1;
        self.high = self.high.max(self.next);
        n
    }

    fn push(&mut self, statement: Statement) {
        self.blocks.last_mut().unwrap().push(statement);
    }

    fn exec(
        &mut self,
        opcode: ir::Opcode,
        destinations: Vec<ir::Operand>,
        sources: Vec<ir::Operand>,
    ) {
        self.push(Statement::Execute(instruction(
            opcode,
            destinations,
            sources,
        )));
    }

    /// Compute into a fresh scratch register.
    fn scratch(&mut self, opcode: ir::Opcode, mask_bits: u8, sources: Vec<ir::Operand>) -> Value {
        let n = self.alloc();
        self.exec(opcode, vec![mask(temp(n), mask_bits)], sources);
        Value::new(temp(n))
    }

    fn write_to(&mut self, write: &Write, opcode: ir::Opcode, sources: Vec<ir::Operand>) {
        let mut op = instruction(opcode, vec![write.operand()], sources);
        op.saturate = write.saturate;
        self.push(Statement::Execute(op));
    }

    /// Copy the registers co-issued instructions read, before the instruction
    /// they pair with writes them.
    fn take_snapshot(&mut self, rest: &[Instruction]) -> Lowering {
        for ins in rest.iter().take_while(|i| i.coissue) {
            for s in &ins.sources {
                let kind = s.register.kind;
                let tracked = kind == RegisterKind::Temp
                    || kind == RegisterKind::Address && self.shader.stage == Stage::Pixel;
                let key = (kind, s.register.index);
                if tracked && !self.snapshot.contains_key(&key) {
                    let value = self.register(s.register, None, 0)?;
                    let n = self.alloc();
                    self.exec(
                        ir::Opcode::Mov,
                        vec![mask(temp(n), 15)],
                        vec![value.operand()],
                    );
                    self.snapshot.insert(key, n);
                }
            }
        }
        Ok(())
    }

    /// A register's raw value, without swizzle, conversion or modifier.
    fn register(&mut self, r: Register, relative: Option<Relative>, row: u16) -> Lowering<Value> {
        if self.coissue
            && relative.is_none()
            && let Some(&n) = self.snapshot.get(&(r.kind, r.index))
        {
            return Ok(Value::new(temp(n)));
        }
        let index = u32::from(r.index) + u32::from(row);
        use RegisterKind::*;
        Ok(match r.kind {
            Temp => Value::new(temp(index)),
            Input => {
                if let Some(rel) = relative {
                    self.relative_input(index, rel)?
                } else {
                    self.inputs.insert(index);
                    Value::new(input(index))
                }
            }
            Float => {
                if let Some(rel) = relative {
                    self.relative_float(index, rel)?
                } else {
                    self.definition(Float, index, FLOATS)
                }
            }
            Address => {
                if self.shader.stage == Stage::Vertex {
                    Value::new(temp(ADDRESS))
                } else if self.version().major == 1 {
                    self.textures.insert(index);
                    Value::new(temp(TEXTURE + index))
                } else {
                    self.inputs.insert(INPUT_TEXCOORD + index);
                    Value::new(input(INPUT_TEXCOORD + index))
                }
            }
            RasterOutput => self.read_output(index),
            ColorOutputLegacy => self.read_output(OUTPUT_COLOR + index),
            Output => self.read_output(OUTPUT_GENERIC + index),
            ColorOutput => self.read_output(OUTPUT_TARGET + index),
            DepthOutput => self.read_output(OUTPUT_DEPTH).swizzled([0; 4]),
            Int => self.definition(Int, index, INTEGERS),
            Bool => self.definition(Bool, index, BOOLEANS),
            Loop => Value::new(temp(LOOP)).swizzled([0; 4]),
            Misc => {
                let n = if index == 0 {
                    INPUT_POSITION
                } else {
                    INPUT_FACE
                };
                self.inputs.insert(n);
                Value::new(input(n))
            }
            Predicate => Value::new(temp(PREDICATE)),
            Sampler | Label => bits([0; 4]),
        })
    }

    fn read_output(&mut self, n: u32) -> Value {
        self.outputs_read.insert(n);
        Value::new(output(n))
    }

    fn definition(&self, kind: RegisterKind, index: u32, slot: u32) -> Value {
        match u16::try_from(index)
            .ok()
            .and_then(|i| self.definitions.get(&(kind, i)))
        {
            Some(value) => bits(*value),
            None => Value::new(constant(slot, index)),
        }
    }

    /// The integer offset a relative operand adds, in lane x.
    fn relative_index(&mut self, rel: Relative) -> Lowering<Value> {
        Ok(if rel.register.kind == RegisterKind::Loop {
            Value::new(temp(LOOP)).swizzled([0; 4])
        } else {
            let a = Value::new(temp(ADDRESS)).lane(usize::from(rel.component));
            self.scratch(ir::Opcode::Ftoi, 1, vec![a])
        })
    }

    /// `base + offset` in lane x, with lane y set where it lies in `0..bound`.
    fn bounded_index(&mut self, base: u32, rel: Relative, bound: u32) -> Lowering<(Value, Value)> {
        let offset = self.relative_index(rel)?;
        let index = self.scratch(
            ir::Opcode::Iadd,
            1,
            vec![offset.lane(0), int(base as i32).lane(0)],
        );
        let inside = self.alloc();
        self.exec(
            ir::Opcode::Ige,
            vec![mask(temp(inside), 1)],
            vec![index.lane(0), int(0).lane(0)],
        );
        self.exec(
            ir::Opcode::Ilt,
            vec![mask(temp(inside), 2)],
            vec![index.lane(0), int(bound as i32).lane(0)],
        );
        self.exec(
            ir::Opcode::And,
            vec![mask(temp(inside), 1)],
            vec![
                Value::new(temp(inside)).lane(0),
                Value::new(temp(inside)).lane(1),
            ],
        );
        Ok((index, Value::new(temp(inside))))
    }

    /// Read `register[index]` with `index` clamped to `0..=last`, zero where `inside` is clear.
    fn bounded_read(
        &mut self,
        result: u32,
        index: &Value,
        inside: &Value,
        last: u32,
        read: impl FnOnce(Box<ir::Operand>) -> OperandValue,
    ) -> Value {
        let n = self.alloc();
        let clamped = Value::new(temp(n));
        self.exec(
            ir::Opcode::Imax,
            vec![mask(temp(n), 1)],
            vec![index.lane(0), int(0).lane(0)],
        );
        self.exec(
            ir::Opcode::Imin,
            vec![mask(temp(n), 1)],
            vec![clamped.lane(0), int(last as i32).lane(0)],
        );
        let value = Value::new(read(Box::new(clamped.lane(0))));
        self.exec(
            ir::Opcode::Mov,
            vec![mask(temp(result), 15)],
            vec![value.operand()],
        );
        self.exec(
            ir::Opcode::And,
            vec![mask(temp(result), 15)],
            vec![Value::new(temp(result)).operand(), inside.lane(0)],
        );
        clamped
    }

    fn float_bound(&self) -> u32 {
        if self.shader.stage == Stage::Vertex {
            256
        } else {
            match self.version().major {
                1 => 8,
                2 => 32,
                _ => 224,
            }
        }
    }

    fn relative_float(&mut self, base: u32, rel: Relative) -> Lowering<Value> {
        let (index, inside) = self.bounded_index(base, rel, self.float_bound())?;
        let result = self.alloc();
        let read = |register| OperandValue::Constant {
            slot: Index::Direct(u64::from(FLOATS)),
            byte_offset: Index::Relative { base: 0, register },
        };
        let clamped = self.bounded_read(result, &index, &inside, 255, read);
        let rows = self.defined_rows;
        if rows == 0 {
            return Ok(Value::new(temp(result)));
        }
        // A defined register wins over the bound, as in the reference.
        let table = |base: u32| OperandValue::Register {
            register: ir::Register::ImmediateConstantBuffer,
            indices: vec![Index::Relative {
                base: u64::from(base),
                register: Box::new(clamped.lane(0)),
            }],
        };
        let flag = self.scratch(ir::Opcode::Mov, 1, vec![Value::new(table(rows)).lane(0)]);
        let exact = self.scratch(ir::Opcode::Ieq, 1, vec![index.lane(0), clamped.lane(0)]);
        let defined = self.scratch(ir::Opcode::And, 1, vec![flag.lane(0), exact.lane(0)]);
        let current = Value::new(temp(result));
        self.exec(
            ir::Opcode::Movc,
            vec![mask(temp(result), 15)],
            vec![
                defined.lane(0),
                Value::new(table(0)).operand(),
                current.operand(),
            ],
        );
        Ok(Value::new(temp(result)))
    }

    fn relative_input(&mut self, base: u32, rel: Relative) -> Lowering<Value> {
        let (index, inside) = self.bounded_index(base, rel, 16)?;
        self.inputs.extend(0..16);
        let result = self.alloc();
        let _ = self.bounded_read(result, &index, &inside, 15, |register| {
            OperandValue::Register {
                register: ir::Register::Input,
                indices: vec![Index::Relative { base: 0, register }],
            }
        });
        Ok(Value::new(temp(result)))
    }

    /// An arithmetic source as D3D9 defines its value.
    fn source(&mut self, s: &Source, row: u16) -> Lowering<Value> {
        let mut raw = self.register(s.register, s.relative, row)?;
        use RegisterKind::*;
        if matches!(s.register.kind, Int | Bool | Loop) {
            raw = self.scratch(ir::Opcode::Itof, 15, vec![raw.operand()]);
        }
        if self.pixel1() && matches!(s.register.kind, Float | Input) {
            let (low, high) = if s.register.kind == Float {
                (-1.0, 1.0)
            } else {
                (0.0, 1.0)
            };
            let clamped = self.scratch(
                ir::Opcode::Max,
                15,
                vec![raw.operand(), float(low).operand()],
            );
            raw = self.scratch(
                ir::Opcode::Min,
                15,
                vec![clamped.operand(), float(high).operand()],
            );
        }
        let v = raw.swizzled(s.swizzle);
        use SourceModifier::*;
        let op = |s: &mut Self, opcode, sources| s.scratch(opcode, 15, sources);
        Ok(match s.modifier {
            None => v,
            Negate => v.negated(),
            Abs => v.absolute(),
            AbsNegate => v.absolute().negated(),
            Bias => op(
                self,
                ir::Opcode::Add,
                vec![v.operand(), float(-0.5).operand()],
            ),
            BiasNegate => op(
                self,
                ir::Opcode::Add,
                vec![v.negated().operand(), float(0.5).operand()],
            ),
            Sign => op(
                self,
                ir::Opcode::Mad,
                vec![v.operand(), float(2.0).operand(), float(-1.0).operand()],
            ),
            SignNegate => op(
                self,
                ir::Opcode::Mad,
                vec![
                    v.negated().operand(),
                    float(2.0).operand(),
                    float(1.0).operand(),
                ],
            ),
            Complement => op(
                self,
                ir::Opcode::Add,
                vec![v.negated().operand(), float(1.0).operand()],
            ),
            X2 => op(
                self,
                ir::Opcode::Mul,
                vec![v.operand(), float(2.0).operand()],
            ),
            X2Negate => op(
                self,
                ir::Opcode::Mul,
                vec![v.operand(), float(-2.0).operand()],
            ),
            DivideZ => op(self, ir::Opcode::Div, vec![v.operand(), v.lane(2)]),
            DivideW => op(self, ir::Opcode::Div, vec![v.operand(), v.lane(3)]),
            Not => {
                let zero = op(
                    self,
                    ir::Opcode::Eq,
                    vec![v.operand(), float(0.0).operand()],
                );
                op(
                    self,
                    ir::Opcode::And,
                    vec![zero.operand(), bits([ONE; 4]).operand()],
                )
            }
        })
    }

    /// The source's value with any float modifier applied, as plain bits.
    fn plain(&mut self, v: Value) -> Value {
        if v.modifier == ir::Modifier::None {
            v
        } else {
            self.scratch(ir::Opcode::Mov, 15, vec![v.operand()])
        }
    }

    fn sources(&mut self, ins: &Instruction) -> Lowering<Vec<Value>> {
        ins.sources.iter().map(|s| self.source(s, 0)).collect()
    }

    /// The register a destination names, and the mask it writes.
    fn destination(&mut self, d: &Destination) -> Lowering<(OperandValue, u8)> {
        let index = u32::from(d.register.index);
        use RegisterKind::*;
        Ok(match d.register.kind {
            Temp => (temp(index), d.mask),
            Address if self.shader.stage == Stage::Vertex => (temp(ADDRESS), d.mask),
            Address => {
                self.textures.insert(index);
                (temp(TEXTURE + index), d.mask)
            }
            RasterOutput => (output(index), d.mask),
            ColorOutputLegacy => (output(OUTPUT_COLOR + index), d.mask),
            Output => (output(OUTPUT_GENERIC + index), d.mask),
            ColorOutput if index < 2 => (output(OUTPUT_TARGET + index), d.mask),
            ColorOutput => return Err("renderer supports only two color outputs"),
            DepthOutput => (output(OUTPUT_DEPTH), 1),
            Predicate => (temp(PREDICATE), d.mask),
            _ => return Err("read-only destination register"),
        })
    }

    fn clamps_to_eight(&self, d: &Destination) -> bool {
        self.pixel1() && d.register.kind == RegisterKind::Temp
    }

    fn begin(&mut self, ins: &Instruction) -> Lowering<Write> {
        let d = ins.destination.as_ref().ok_or("missing destination")?;
        let (register, mask) = self.destination(d)?;
        if d.shift == 0
            && !self.clamps_to_eight(d)
            && ins.predicate.is_none()
            && d.relative.is_none()
        {
            return Ok(Write {
                register,
                mask,
                saturate: d.saturate,
                finish: None,
            });
        }
        let n = self.alloc();
        Ok(Write {
            register: temp(n),
            mask,
            saturate: false,
            finish: Some(d.clone()),
        })
    }

    fn end(&mut self, ins: &Instruction, write: Write) -> Lowering {
        let Some(d) = write.finish else {
            return Ok(());
        };
        let value = Value::new(write.register.clone());
        let lanes = write.mask;
        let at = |_: &Self| mask(write.register.clone(), lanes);
        if d.shift != 0 {
            let scale = float(2.0f32.powi(i32::from(d.shift)));
            let target = at(self);
            self.exec(
                ir::Opcode::Mul,
                vec![target],
                vec![value.operand(), scale.operand()],
            );
        }
        if d.saturate {
            let mut op = instruction(ir::Opcode::Mov, vec![at(self)], vec![value.operand()]);
            op.saturate = true;
            self.push(Statement::Execute(op));
        }
        if self.clamps_to_eight(&d) {
            let target = at(self);
            self.exec(
                ir::Opcode::Max,
                vec![target],
                vec![value.operand(), float(-8.0).operand()],
            );
            let target = at(self);
            self.exec(
                ir::Opcode::Min,
                vec![target],
                vec![value.operand(), float(8.0).operand()],
            );
        }
        let (register, mask_bits) = self.destination(&d)?;
        if let Some(p) = &ins.predicate {
            let test = self.predicate_mask(p)?;
            if d.relative.is_none()
                && matches!(
                    register,
                    OperandValue::Register {
                        register: ir::Register::Temp,
                        ..
                    }
                )
            {
                let current = Value::new(register.clone());
                self.exec(
                    ir::Opcode::Movc,
                    vec![mask(register, mask_bits)],
                    vec![test.operand(), value.operand(), current.operand()],
                );
            } else {
                for lane in 0..4 {
                    if mask_bits & (1 << lane) == 0 {
                        continue;
                    }
                    self.blocks.push(Vec::new());
                    self.store(&d, register.clone(), 1 << lane, &value)?;
                    let accept = self.blocks.pop().unwrap();
                    self.push(Statement::If {
                        test: test_instruction(test.lane(lane), true),
                        accept,
                        reject: None,
                    });
                }
            }
            return Ok(());
        }
        self.store(&d, register, mask_bits, &value)
    }

    /// Move a finished value into its destination, bounding relative output indices.
    fn store(
        &mut self,
        d: &Destination,
        register: OperandValue,
        lanes: u8,
        value: &Value,
    ) -> Lowering {
        let Some(rel) = d.relative else {
            self.exec(
                ir::Opcode::Mov,
                vec![mask(register, lanes)],
                vec![value.operand()],
            );
            return Ok(());
        };
        let (index, inside) = self.bounded_index(u32::from(d.register.index), rel, 12)?;
        let target = OperandValue::Register {
            register: ir::Register::Output,
            indices: vec![Index::Relative {
                base: u64::from(OUTPUT_GENERIC),
                register: Box::new(index.lane(0)),
            }],
        };
        self.blocks.push(Vec::new());
        self.exec(
            ir::Opcode::Mov,
            vec![mask(target, lanes)],
            vec![value.operand()],
        );
        let accept = self.blocks.pop().unwrap();
        self.push(Statement::If {
            test: test_instruction(inside.lane(0), true),
            accept,
            reject: None,
        });
        Ok(())
    }

    /// Lanes set where the predicate source holds a nonzero value.
    fn predicate_mask(&mut self, p: &Source) -> Lowering<Value> {
        let value = self.register(p.register, None, 0)?.swizzled(p.swizzle);
        let opcode = if p.modifier == SourceModifier::Not {
            ir::Opcode::Eq
        } else {
            ir::Opcode::Ne
        };
        Ok(self.scratch(opcode, 15, vec![value.operand(), float(0.0).operand()]))
    }

    /// A flow condition: an IR value tested for a nonzero lane x.
    fn condition(&mut self, s: &Source) -> Lowering<(ir::Operand, bool)> {
        if s.register.kind == RegisterKind::Bool {
            let value = self.register(s.register, None, 0)?.swizzled(s.swizzle);
            return Ok((value.lane(0), s.modifier != SourceModifier::Not));
        }
        let mask = self.predicate_mask(s)?;
        Ok((mask.lane(0), true))
    }

    fn compare(&mut self, control: u8, a: &Value, b: &Value, lanes: u8) -> Value {
        let (opcode, x, y) = match control {
            1 => (ir::Opcode::Lt, b, a),
            2 => (ir::Opcode::Eq, a, b),
            3 => (ir::Opcode::Ge, a, b),
            4 => (ir::Opcode::Lt, a, b),
            5 => (ir::Opcode::Ne, a, b),
            _ => (ir::Opcode::Ge, b, a),
        };
        self.scratch(opcode, lanes, vec![x.operand(), y.operand()])
    }

    /// The sum of the first `n` lanes of `a * b`, written into `target`.
    fn dot(&mut self, target: ir::Operand, a: &Value, b: &Value, n: usize, saturate: bool) {
        let p = self.alloc();
        let product = Value::new(temp(p));
        self.exec(
            ir::Opcode::Mul,
            vec![mask(temp(p), 15)],
            vec![a.operand(), b.operand()],
        );
        for lane in 1..n - 1 {
            self.exec(
                ir::Opcode::Add,
                vec![mask(temp(p), 1)],
                vec![product.lane(0), product.lane(lane)],
            );
        }
        let mut op = instruction(
            ir::Opcode::Add,
            vec![target],
            vec![product.lane(0), product.lane(n - 1)],
        );
        op.saturate = saturate;
        self.push(Statement::Execute(op));
    }

    fn dot_to(&mut self, write: &Write, a: &Value, b: &Value, n: usize) {
        self.dot(write.operand(), a, b, n, write.saturate);
    }

    /// A vector built lane by lane in scratch, then written as one value.
    fn compose(&mut self, write: &Write, lanes: [ir::Operand; 4]) {
        let n = self.alloc();
        for (lane, value) in lanes.into_iter().enumerate() {
            self.exec(ir::Opcode::Mov, vec![mask(temp(n), 1 << lane)], vec![value]);
        }
        self.write_to(write, ir::Opcode::Mov, vec![Value::new(temp(n)).operand()]);
    }

    /// 1/x with D3D9's +infinity for either zero.
    fn reciprocal(&mut self, x: ir::Operand) -> Value {
        let r = self.scratch(ir::Opcode::Rcp, 1, vec![x.clone()]);
        let zero = self.scratch(ir::Opcode::Eq, 1, vec![x, float(0.0).lane(0)]);
        self.scratch(
            ir::Opcode::Movc,
            1,
            vec![zero.lane(0), bits([INFINITY; 4]).lane(0), r.lane(0)],
        )
    }

    /// Round half away from zero.
    fn round(&mut self, write: &Write, v: Value) {
        let v = self.plain(v);
        let magnitude = self.scratch(
            ir::Opcode::Add,
            15,
            vec![v.absolute().operand(), float(0.5).operand()],
        );
        let floor = self.scratch(ir::Opcode::RoundNi, 15, vec![magnitude.operand()]);
        let sign = self.scratch(
            ir::Opcode::And,
            15,
            vec![v.operand(), bits([0x8000_0000; 4]).operand()],
        );
        self.write_to(write, ir::Opcode::Or, vec![floor.operand(), sign.operand()]);
    }

    /// `base` to the power `exponent` for a nonnegative base, as C `pow`: one
    /// for a zero exponent or a unit base, where `2^(b * log2 a)` would give NaN.
    fn power(&mut self, base: ir::Operand, exponent: ir::Operand) -> Value {
        let log = self.scratch(ir::Opcode::Log, 1, vec![base.clone()]);
        let scaled = self.scratch(ir::Opcode::Mul, 1, vec![log.lane(0), exponent.clone()]);
        let power = self.scratch(ir::Opcode::Exp, 1, vec![scaled.lane(0)]);
        let flat = self.scratch(ir::Opcode::Eq, 1, vec![exponent, float(0.0).lane(0)]);
        let unit = self.scratch(ir::Opcode::Eq, 1, vec![base, float(1.0).lane(0)]);
        let one = self.scratch(ir::Opcode::Or, 1, vec![flat.lane(0), unit.lane(0)]);
        self.scratch(
            ir::Opcode::Movc,
            1,
            vec![one.lane(0), float(1.0).lane(0), power.lane(0)],
        )
    }

    fn sample(
        &mut self,
        write: &Write,
        index: u32,
        coordinate: ir::Operand,
        mode: u32,
        gradients: Option<[ir::Operand; 2]>,
    ) {
        let resource = operand(
            register(ir::Register::Resource, index),
            Components::None,
            ir::Modifier::None,
        );
        let sampler = operand(
            register(ir::Register::Sampler, index),
            Components::None,
            ir::Modifier::None,
        );
        let (opcode, sources) = match gradients {
            Some([dx, dy]) => (
                ir::Opcode::SampleD,
                vec![coordinate, resource, sampler, dx, dy],
            ),
            None => (ir::Opcode::Sample, vec![coordinate, resource, sampler]),
        };
        let mut op = instruction(opcode, vec![write.operand()], sources);
        op.saturate = write.saturate;
        op.controls = mode;
        self.push(Statement::Execute(op));
    }

    fn instruction(&mut self, ins: &Instruction) -> Lowering {
        use Opcode::*;
        match ins.opcode {
            Nop | Dcl | Def | Defi | Defb | Phase => Ok(()),
            If | Ifc => {
                let test = if ins.opcode == If {
                    let (value, nonzero) = self.condition(&ins.sources[0])?;
                    test_instruction(value, nonzero)
                } else {
                    let a = self.source(&ins.sources[0], 0)?.swizzled([0; 4]);
                    let b = self.source(&ins.sources[1], 0)?.swizzled([0; 4]);
                    let m = self.compare(ins.controls, &a, &b, 1);
                    test_instruction(m.lane(0), true)
                };
                self.frames.push(Frame::If { test, accept: None });
                self.blocks.push(Vec::new());
                Ok(())
            }
            Else => {
                let block = self.blocks.pop().unwrap();
                let Some(Frame::If { accept, .. }) = self.frames.last_mut() else {
                    return Err("else without if");
                };
                *accept = Some(block);
                self.blocks.push(Vec::new());
                Ok(())
            }
            EndIf => {
                let block = self.blocks.pop().unwrap();
                let Some(Frame::If { test, accept }) = self.frames.pop() else {
                    return Err("mismatched flow terminator");
                };
                let (accept, reject) = match accept {
                    Some(accept) => (accept, Some(block)),
                    None => (block, None),
                };
                self.push(Statement::If {
                    test,
                    accept,
                    reject,
                });
                Ok(())
            }
            Loop | Rep => self.begin_loop(ins),
            EndLoop | EndRep => self.end_loop(),
            Break => {
                self.exec(ir::Opcode::Break, vec![], vec![]);
                Ok(())
            }
            Breakc | Breakp => {
                let (value, nonzero) = if ins.opcode == Breakp {
                    self.condition(&ins.sources[0])?
                } else {
                    let a = self.source(&ins.sources[0], 0)?.swizzled([0; 4]);
                    let b = self.source(&ins.sources[1], 0)?.swizzled([0; 4]);
                    (self.compare(ins.controls, &a, &b, 1).lane(0), true)
                };
                let mut op = instruction(ir::Opcode::Breakc, vec![], vec![value]);
                op.test_nonzero = nonzero;
                self.push(Statement::Execute(op));
                Ok(())
            }
            Call | CallNz => {
                let label = operand(
                    register(
                        ir::Register::Label,
                        u32::from(ins.sources[0].register.index),
                    ),
                    Components::None,
                    ir::Modifier::None,
                );
                if ins.opcode == Call {
                    self.exec(ir::Opcode::Call, vec![], vec![label]);
                } else {
                    let (value, nonzero) = self.condition(&ins.sources[1])?;
                    let mut op = instruction(ir::Opcode::Callc, vec![], vec![label, value]);
                    op.test_nonzero = nonzero;
                    self.push(Statement::Execute(op));
                }
                Ok(())
            }
            Ret => {
                self.exec(ir::Opcode::Ret, vec![], vec![]);
                Ok(())
            }
            Label => {
                if !self.frames.is_empty() {
                    return Err("label inside flow control");
                }
                let label = operand(
                    register(
                        ir::Register::Label,
                        u32::from(ins.sources[0].register.index),
                    ),
                    Components::None,
                    ir::Modifier::None,
                );
                self.exec(ir::Opcode::Label, vec![], vec![label]);
                Ok(())
            }
            Texkill => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let q = self.register(d.register, d.relative, 0)?;
                let negative =
                    self.scratch(ir::Opcode::Lt, 7, vec![q.operand(), float(0.0).operand()]);
                let any = self.scratch(ir::Opcode::Or, 1, vec![negative.lane(0), negative.lane(1)]);
                let any2 = self.scratch(ir::Opcode::Or, 1, vec![any.lane(0), negative.lane(2)]);
                self.exec(ir::Opcode::Discard, vec![], vec![any2.lane(0)]);
                Ok(())
            }
            Texm3x2pad | Texm3x3pad => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let a = self.source(&ins.sources[0], 0)?;
                let t = self.texcoord(d);
                let row = self.matrix;
                self.dot(mask(temp(MATRIX), 1 << row), &t, &a, 3, false);
                self.matrix = (self.matrix + 1).min(2);
                Ok(())
            }
            Texdepth => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let r = Value::new(temp(u32::from(d.register.index)));
                let reciprocal = self.reciprocal(r.lane(1));
                self.depth(r.lane(0), reciprocal.lane(0));
                Ok(())
            }
            Texm3x2depth => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let a = self.source(&ins.sources[0], 0)?;
                let t = self.texcoord(d);
                let z = self.alloc();
                self.dot(mask(temp(z), 1), &t, &a, 3, false);
                let reciprocal = self.reciprocal(Value::new(temp(z)).lane(0));
                self.depth(Value::new(temp(MATRIX)).lane(0), reciprocal.lane(0));
                self.matrix = 0;
                Ok(())
            }
            _ => {
                let write = self.begin(ins)?;
                self.operation(ins, &write)?;
                self.end(ins, write)
            }
        }
    }

    /// The texture coordinate input of a PS 1.x texture stage.
    fn texcoord(&mut self, d: &Destination) -> Value {
        let n = INPUT_TEXCOORD + u32::from(d.register.index);
        self.inputs.insert(n);
        Value::new(input(n))
    }

    fn depth(&mut self, numerator: ir::Operand, reciprocal: ir::Operand) {
        let product = self.scratch(ir::Opcode::Mul, 1, vec![numerator, reciprocal]);
        let mut op = instruction(
            ir::Opcode::Mov,
            vec![mask(output(OUTPUT_DEPTH), 1)],
            vec![product.lane(0)],
        );
        op.saturate = true;
        self.push(Statement::Execute(op));
    }

    fn begin_loop(&mut self, ins: &Instruction) -> Lowering {
        let state = self.loops;
        self.loops += 1;
        let counter = if ins.opcode == Opcode::Loop {
            ins.sources[1].clone()
        } else {
            ins.sources[0].clone()
        };
        let c = self
            .register(counter.register, None, 0)?
            .swizzled(counter.swizzle);
        if ins.opcode == Opcode::Loop {
            self.exec(
                ir::Opcode::Mov,
                vec![mask(temp(state), 4)],
                vec![Value::new(temp(LOOP)).lane(0)],
            );
        }
        self.exec(ir::Opcode::Mov, vec![mask(temp(state), 1)], vec![c.lane(0)]);
        self.frames.push(Frame::Loop {
            opcode: ins.opcode,
            counter,
            state,
        });
        self.blocks.push(Vec::new());
        // The guard bounds total iterations, so a guest count cannot hang the GPU.
        self.exec(
            ir::Opcode::Iadd,
            vec![mask(temp(LOOP), 2)],
            vec![Value::new(temp(LOOP)).lane(1), int(1).lane(0)],
        );
        let spent = self.scratch(
            ir::Opcode::Uge,
            1,
            vec![
                Value::new(temp(LOOP)).lane(1),
                int(MAX_EXECUTION_STEPS as i32).lane(0),
            ],
        );
        self.exec(ir::Opcode::Breakc, vec![], vec![spent.lane(0)]);
        Ok(())
    }

    fn end_loop(&mut self) -> Lowering {
        let Some(Frame::Loop {
            opcode,
            counter,
            state,
        }) = self.frames.pop()
        else {
            return Err("mismatched flow terminator");
        };
        let s = Value::new(temp(state));
        self.exec(
            ir::Opcode::Iadd,
            vec![mask(temp(state), 1)],
            vec![s.lane(0), int(-1).lane(0)],
        );
        let done = self.scratch(ir::Opcode::Ige, 1, vec![int(0).lane(0), s.lane(0)]);
        self.exec(ir::Opcode::Breakc, vec![], vec![done.lane(0)]);
        if opcode == Opcode::Loop {
            self.exec(
                ir::Opcode::Iadd,
                vec![mask(temp(LOOP), 1)],
                vec![Value::new(temp(LOOP)).lane(0), s.lane(1)],
            );
        }
        let body = self.blocks.pop().unwrap();
        self.blocks.push(Vec::new());
        if opcode == Opcode::Loop {
            let c = self
                .register(counter.register, None, 0)?
                .swizzled(counter.swizzle);
            self.exec(ir::Opcode::Mov, vec![mask(temp(LOOP), 1)], vec![c.lane(1)]);
            self.exec(ir::Opcode::Mov, vec![mask(temp(state), 2)], vec![c.lane(2)]);
        }
        self.push(Statement::Loop(body));
        if opcode == Opcode::Loop {
            self.exec(ir::Opcode::Mov, vec![mask(temp(LOOP), 1)], vec![s.lane(2)]);
        }
        let accept = self.blocks.pop().unwrap();
        let positive = self.scratch(ir::Opcode::Ilt, 1, vec![int(0).lane(0), s.lane(0)]);
        self.push(Statement::If {
            test: test_instruction(positive.lane(0), true),
            accept,
            reject: None,
        });
        Ok(())
    }

    fn operation(&mut self, ins: &Instruction, w: &Write) -> Lowering {
        use Opcode::*;
        let version = self.version();
        let vertex1 = self.shader.stage == Stage::Vertex && version.major == 1;
        let to = |s: &mut Self, opcode, sources| s.write_to(w, opcode, sources);
        match ins.opcode {
            Mov => {
                let a = self.source(&ins.sources[0], 0)?;
                let address = ins
                    .destination
                    .as_ref()
                    .is_some_and(|d| d.register.kind == RegisterKind::Address);
                if vertex1 && address {
                    self.round(w, a);
                } else {
                    to(self, ir::Opcode::Mov, vec![a.operand()]);
                }
            }
            Mova => {
                let a = self.source(&ins.sources[0], 0)?;
                self.round(w, a);
            }
            Add | Sub | Mul | Min | Max => {
                let v = self.sources(ins)?;
                let (opcode, b) = match ins.opcode {
                    Add => (ir::Opcode::Add, v[1].clone()),
                    Sub => (ir::Opcode::Add, v[1].negated()),
                    Mul => (ir::Opcode::Mul, v[1].clone()),
                    Min => (ir::Opcode::Min, v[1].clone()),
                    _ => (ir::Opcode::Max, v[1].clone()),
                };
                to(self, opcode, vec![v[0].operand(), b.operand()]);
            }
            Mad => {
                let v = self.sources(ins)?;
                to(
                    self,
                    ir::Opcode::Mad,
                    vec![v[0].operand(), v[1].operand(), v[2].operand()],
                );
            }
            Rcp => {
                let a = self.source(&ins.sources[0], 0)?;
                let r = self.reciprocal(a.lane(0));
                to(self, ir::Opcode::Mov, vec![r.lane(0)]);
            }
            Rsq => {
                let a = self.source(&ins.sources[0], 0)?;
                to(self, ir::Opcode::Rsq, vec![a.absolute().lane(0)]);
            }
            Dp3 | Dp4 => {
                let v = self.sources(ins)?;
                let n = if ins.opcode == Dp3 { 3 } else { 4 };
                self.dot_to(w, &v[0], &v[1], n);
            }
            Dp2add => {
                let v = self.sources(ins)?;
                let p = self.alloc();
                self.dot(mask(temp(p), 1), &v[0], &v[1], 2, false);
                to(
                    self,
                    ir::Opcode::Add,
                    vec![Value::new(temp(p)).lane(0), v[2].lane(0)],
                );
            }
            Slt | Sge | Setp => {
                let v = self.sources(ins)?;
                let control = match ins.opcode {
                    Slt => 4,
                    Sge => 3,
                    _ => ins.controls,
                };
                let m = self.compare(control, &v[0], &v[1], 15);
                to(
                    self,
                    ir::Opcode::And,
                    vec![m.operand(), bits([ONE; 4]).operand()],
                );
            }
            Exp => {
                let a = self.source(&ins.sources[0], 0)?;
                to(self, ir::Opcode::Exp, vec![a.lane(0)]);
            }
            Log => {
                let a = self.source(&ins.sources[0], 0)?;
                to(self, ir::Opcode::Log, vec![a.absolute().lane(0)]);
            }
            Expp => {
                let a = self.source(&ins.sources[0], 0)?;
                if vertex1 {
                    let floor = self.scratch(ir::Opcode::RoundNi, 1, vec![a.lane(0)]);
                    let x = self.scratch(ir::Opcode::Exp, 1, vec![floor.lane(0)]);
                    let y = self.scratch(ir::Opcode::Frc, 1, vec![a.lane(0)]);
                    let z = self.scratch(ir::Opcode::Exp, 1, vec![a.lane(0)]);
                    self.compose(w, [x.lane(0), y.lane(0), z.lane(0), float(1.0).lane(0)]);
                } else {
                    to(self, ir::Opcode::Exp, vec![a.lane(0)]);
                }
            }
            Logp => {
                let a = self.source(&ins.sources[0], 0)?;
                if vertex1 {
                    let log = self.scratch(ir::Opcode::Log, 1, vec![a.absolute().lane(0)]);
                    let floor = self.scratch(ir::Opcode::RoundNi, 1, vec![log.lane(0)]);
                    let power = self.scratch(ir::Opcode::Exp, 1, vec![floor.lane(0)]);
                    let y = self.scratch(
                        ir::Opcode::Div,
                        1,
                        vec![a.absolute().lane(0), power.lane(0)],
                    );
                    self.compose(
                        w,
                        [floor.lane(0), y.lane(0), log.lane(0), float(1.0).lane(0)],
                    );
                } else {
                    to(self, ir::Opcode::Log, vec![a.absolute().lane(0)]);
                }
            }
            Lit => {
                let a = self.source(&ins.sources[0], 0)?;
                let y = self.scratch(ir::Opcode::Max, 1, vec![a.lane(0), float(0.0).lane(0)]);
                // Compare and select, so a NaN power stays NaN as in the reference.
                let high = self.scratch(ir::Opcode::Lt, 1, vec![float(128.0).lane(0), a.lane(3)]);
                let low = self.scratch(
                    ir::Opcode::Movc,
                    1,
                    vec![high.lane(0), float(128.0).lane(0), a.lane(3)],
                );
                let under =
                    self.scratch(ir::Opcode::Lt, 1, vec![low.lane(0), float(-128.0).lane(0)]);
                let w_clamped = self.scratch(
                    ir::Opcode::Movc,
                    1,
                    vec![under.lane(0), float(-128.0).lane(0), low.lane(0)],
                );
                let power = self.power(a.lane(1), w_clamped.lane(0));
                let lit = self.scratch(ir::Opcode::Lt, 1, vec![float(0.0).lane(0), a.lane(0)]);
                let facing = self.scratch(ir::Opcode::Lt, 1, vec![float(0.0).lane(0), a.lane(1)]);
                let both = self.scratch(ir::Opcode::And, 1, vec![lit.lane(0), facing.lane(0)]);
                let z = self.scratch(
                    ir::Opcode::Movc,
                    1,
                    vec![both.lane(0), power.lane(0), float(0.0).lane(0)],
                );
                self.compose(
                    w,
                    [float(1.0).lane(0), y.lane(0), z.lane(0), float(1.0).lane(0)],
                );
            }
            Dst => {
                let v = self.sources(ins)?;
                let y = self.scratch(ir::Opcode::Mul, 1, vec![v[0].lane(1), v[1].lane(1)]);
                let lanes = [float(1.0).lane(0), y.lane(0), v[0].lane(2), v[1].lane(3)];
                self.compose(w, lanes);
            }
            Lrp => {
                let v = self.sources(ins)?;
                let complement = self.scratch(
                    ir::Opcode::Add,
                    15,
                    vec![float(1.0).operand(), v[0].negated().operand()],
                );
                let tail = self.scratch(
                    ir::Opcode::Mul,
                    15,
                    vec![complement.operand(), v[2].operand()],
                );
                to(
                    self,
                    ir::Opcode::Mad,
                    vec![v[0].operand(), v[1].operand(), tail.operand()],
                );
            }
            Frc => {
                let a = self.source(&ins.sources[0], 0)?;
                to(self, ir::Opcode::Frc, vec![a.operand()]);
            }
            Pow => {
                let v = self.sources(ins)?;
                let p = self.power(v[0].absolute().lane(0), v[1].lane(0));
                to(self, ir::Opcode::Mov, vec![p.lane(0)]);
            }
            Crs => {
                let v = self.sources(ins)?;
                let left = self.scratch(
                    ir::Opcode::Mul,
                    7,
                    vec![
                        v[0].swizzled([1, 2, 0, 0]).operand(),
                        v[1].swizzled([2, 0, 1, 1]).operand(),
                    ],
                );
                let right = self.scratch(
                    ir::Opcode::Mul,
                    7,
                    vec![
                        v[0].swizzled([2, 0, 1, 1]).operand(),
                        v[1].swizzled([1, 2, 0, 0]).operand(),
                    ],
                );
                let cross = self.scratch(
                    ir::Opcode::Add,
                    7,
                    vec![left.operand(), right.negated().operand()],
                );
                self.compose(
                    w,
                    [
                        cross.lane(0),
                        cross.lane(1),
                        cross.lane(2),
                        float(0.0).lane(0),
                    ],
                );
            }
            Sgn => {
                let a = self.source(&ins.sources[0], 0)?;
                let positive =
                    self.scratch(ir::Opcode::Lt, 15, vec![float(0.0).operand(), a.operand()]);
                let negative =
                    self.scratch(ir::Opcode::Lt, 15, vec![a.operand(), float(0.0).operand()]);
                let one = self.scratch(
                    ir::Opcode::And,
                    15,
                    vec![positive.operand(), bits([ONE; 4]).operand()],
                );
                let minus = self.scratch(
                    ir::Opcode::And,
                    15,
                    vec![negative.operand(), bits([(-1.0f32).to_bits(); 4]).operand()],
                );
                to(self, ir::Opcode::Or, vec![one.operand(), minus.operand()]);
            }
            Abs => {
                let a = self.source(&ins.sources[0], 0)?;
                to(self, ir::Opcode::Mov, vec![a.absolute().operand()]);
            }
            Nrm => {
                let a = self.source(&ins.sources[0], 0)?;
                let length = self.alloc();
                self.dot(mask(temp(length), 1), &a, &a, 3, false);
                let scale =
                    self.scratch(ir::Opcode::Rsq, 1, vec![Value::new(temp(length)).lane(0)]);
                let n = self.scratch(ir::Opcode::Mul, 7, vec![a.operand(), scale.lane(0)]);
                self.compose(w, [n.lane(0), n.lane(1), n.lane(2), a.lane(3)]);
            }
            SinCos => {
                let a = self.source(&ins.sources[0], 0)?;
                let n = self.alloc();
                self.exec(
                    ir::Opcode::Sincos,
                    vec![mask(temp(n), 2), mask(temp(n), 1)],
                    vec![a.lane(0)],
                );
                let r = Value::new(temp(n));
                self.compose(
                    w,
                    [r.lane(0), r.lane(1), float(0.0).lane(0), float(0.0).lane(0)],
                );
            }
            Cmp => {
                let v = self.sources(ins)?;
                let m = self.scratch(
                    ir::Opcode::Ge,
                    15,
                    vec![v[0].operand(), float(0.0).operand()],
                );
                to(
                    self,
                    ir::Opcode::Movc,
                    vec![m.operand(), v[1].operand(), v[2].operand()],
                );
            }
            Cnd => {
                let v = self.sources(ins)?;
                let a = if version.minor < 4 {
                    v[0].swizzled([3; 4])
                } else {
                    v[0].clone()
                };
                let m = self.scratch(ir::Opcode::Lt, 15, vec![float(0.5).operand(), a.operand()]);
                to(
                    self,
                    ir::Opcode::Movc,
                    vec![m.operand(), v[1].operand(), v[2].operand()],
                );
            }
            Dsx | Dsy => {
                let a = self.source(&ins.sources[0], 0)?;
                let opcode = if ins.opcode == Dsx {
                    ir::Opcode::DerivRtx
                } else {
                    ir::Opcode::DerivRty
                };
                to(self, opcode, vec![a.operand()]);
            }
            M4x4 | M4x3 | M3x4 | M3x3 | M3x2 => {
                let rows = match ins.opcode {
                    M4x4 | M3x4 => 4,
                    M4x3 | M3x3 => 3,
                    _ => 2,
                };
                let width = if matches!(ins.opcode, M4x4 | M4x3) {
                    4
                } else {
                    3
                };
                let a = self.source(&ins.sources[0], 0)?;
                let n = self.alloc();
                for row in 0..4 {
                    if row < rows {
                        let b = self.source(&ins.sources[1], row as u16)?;
                        self.dot(mask(temp(n), 1 << row), &a, &b, width, false);
                    } else {
                        self.exec(
                            ir::Opcode::Mov,
                            vec![mask(temp(n), 1 << row)],
                            vec![float(0.0).lane(0)],
                        );
                    }
                }
                to(self, ir::Opcode::Mov, vec![Value::new(temp(n)).operand()]);
            }
            Texcoord => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                if ins.sources.is_empty() {
                    let t = self.texcoord(d);
                    let n = self.alloc();
                    let mut op =
                        instruction(ir::Opcode::Mov, vec![mask(temp(n), 7)], vec![t.operand()]);
                    op.saturate = true;
                    self.push(Statement::Execute(op));
                    let r = Value::new(temp(n));
                    self.compose(w, [r.lane(0), r.lane(1), r.lane(2), float(1.0).lane(0)]);
                } else {
                    let a = self.source(&ins.sources[0], 0)?;
                    to(self, ir::Opcode::Mov, vec![a.operand()]);
                }
            }
            Tex | Texldl | Texldd => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let index = if version.major == 1 {
                    u32::from(d.register.index)
                } else {
                    let s = &ins.sources[1];
                    if s.register.kind != RegisterKind::Sampler || s.relative.is_some() {
                        return Err("texture instruction requires direct sampler operand");
                    }
                    u32::from(s.register.index)
                };
                let coordinate = if ins.sources.is_empty() {
                    self.textures.insert(index);
                    Value::new(temp(TEXTURE + index))
                } else {
                    self.source(&ins.sources[0], 0)?
                };
                if ins.opcode == Texldd {
                    let dx = self.source(&ins.sources[2], 0)?;
                    let dy = self.source(&ins.sources[3], 0)?;
                    self.sample(
                        w,
                        index,
                        coordinate.operand(),
                        0,
                        Some([dx.operand(), dy.operand()]),
                    );
                } else {
                    let mode = if ins.opcode == Texldl {
                        3
                    } else {
                        u32::from(ins.controls)
                    };
                    self.sample(w, index, coordinate.operand(), mode, None);
                }
            }
            Texreg2ar | Texreg2gb | Texreg2rgb | Texdp3tex => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let n = u32::from(d.register.index);
                let a = self.source(&ins.sources[0], 0)?;
                let c = self.alloc();
                let lanes = match ins.opcode {
                    Texreg2ar => [a.lane(3), a.lane(0), float(0.0).lane(0), float(1.0).lane(0)],
                    Texreg2gb => [a.lane(1), a.lane(2), float(0.0).lane(0), float(1.0).lane(0)],
                    Texreg2rgb => [a.lane(0), a.lane(1), a.lane(2), float(1.0).lane(0)],
                    _ => {
                        self.textures.insert(n);
                        let t = Value::new(temp(TEXTURE + n));
                        let dot = self.alloc();
                        self.dot(mask(temp(dot), 1), &t, &a, 3, false);
                        [
                            Value::new(temp(dot)).lane(0),
                            float(0.0).lane(0),
                            float(0.0).lane(0),
                            float(1.0).lane(0),
                        ]
                    }
                };
                for (lane, value) in lanes.into_iter().enumerate() {
                    self.exec(ir::Opcode::Mov, vec![mask(temp(c), 1 << lane)], vec![value]);
                }
                self.sample(w, n, Value::new(temp(c)).operand(), 0, None);
            }
            Texdp3 => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let n = u32::from(d.register.index);
                let a = self.source(&ins.sources[0], 0)?;
                self.textures.insert(n);
                self.dot_to(w, &Value::new(temp(TEXTURE + n)), &a, 3);
            }
            Texm3x2tex | Texm3x3tex | Texm3x3 | Texm3x3spec | Texm3x3vspec => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let n = u32::from(d.register.index);
                let a = self.source(&ins.sources[0], 0)?;
                let t = self.texcoord(d);
                let c = self.alloc();
                let m = Value::new(temp(MATRIX));
                if ins.opcode == Texm3x2tex {
                    self.exec(ir::Opcode::Mov, vec![mask(temp(c), 1)], vec![m.lane(0)]);
                    self.dot(mask(temp(c), 2), &t, &a, 3, false);
                    self.exec(
                        ir::Opcode::Mov,
                        vec![mask(temp(c), 4)],
                        vec![float(0.0).lane(0)],
                    );
                } else {
                    self.exec(ir::Opcode::Mov, vec![mask(temp(c), 3)], vec![m.operand()]);
                    self.dot(mask(temp(c), 4), &t, &a, 3, false);
                }
                self.exec(
                    ir::Opcode::Mov,
                    vec![mask(temp(c), 8)],
                    vec![float(1.0).lane(0)],
                );
                let coordinate = Value::new(temp(c));
                match ins.opcode {
                    Texm3x3 => to(self, ir::Opcode::Mov, vec![coordinate.operand()]),
                    Texm3x3spec | Texm3x3vspec => {
                        let eye = if ins.opcode == Texm3x3spec {
                            self.source(&ins.sources[1], 0)?
                        } else {
                            if n < 2 {
                                return Err("texm3x3vspec requires preceding stages");
                            }
                            let e = self.alloc();
                            for (lane, stage) in [n - 2, n - 1, n].into_iter().enumerate() {
                                let input_register = INPUT_TEXCOORD + stage;
                                self.inputs.insert(input_register);
                                self.exec(
                                    ir::Opcode::Mov,
                                    vec![mask(temp(e), 1 << lane)],
                                    vec![Value::new(input(input_register)).lane(3)],
                                );
                            }
                            self.exec(
                                ir::Opcode::Mov,
                                vec![mask(temp(e), 8)],
                                vec![float(1.0).lane(0)],
                            );
                            Value::new(temp(e))
                        };
                        let dots = self.alloc();
                        self.dot(mask(temp(dots), 1), &coordinate, &eye, 3, false);
                        self.dot(mask(temp(dots), 2), &coordinate, &coordinate, 3, false);
                        let d = Value::new(temp(dots));
                        let twice =
                            self.scratch(ir::Opcode::Mul, 1, vec![float(2.0).lane(0), d.lane(0)]);
                        let factor =
                            self.scratch(ir::Opcode::Div, 1, vec![twice.lane(0), d.lane(1)]);
                        let scaled = self.scratch(
                            ir::Opcode::Mul,
                            7,
                            vec![factor.lane(0), coordinate.operand()],
                        );
                        let reflected = self.alloc();
                        self.exec(
                            ir::Opcode::Add,
                            vec![mask(temp(reflected), 7)],
                            vec![scaled.operand(), eye.negated().operand()],
                        );
                        self.exec(
                            ir::Opcode::Mov,
                            vec![mask(temp(reflected), 8)],
                            vec![float(1.0).lane(0)],
                        );
                        self.sample(w, n, Value::new(temp(reflected)).operand(), 0, None);
                    }
                    _ => self.sample(w, n, coordinate.operand(), 0, None),
                }
                self.matrix = 0;
            }
            Texbem | Texbeml | Bem => {
                let d = ins.destination.as_ref().ok_or("missing destination")?;
                let n = u32::from(d.register.index);
                let matrix = Value::new(constant(BUMP_MATRIX, n));
                let v = self.sources(ins)?;
                let (base, perturb) = if ins.opcode == Bem {
                    (v[0].clone(), v[1].clone())
                } else {
                    self.inputs.insert(INPUT_TEXCOORD + n);
                    (Value::new(input(INPUT_TEXCOORD + n)), v[0].clone())
                };
                let first = self.scratch(
                    ir::Opcode::Mul,
                    3,
                    vec![matrix.operand(), perturb.swizzled([0, 0, 0, 0]).operand()],
                );
                let second = self.scratch(
                    ir::Opcode::Mul,
                    3,
                    vec![
                        matrix.swizzled([2, 3, 0, 0]).operand(),
                        perturb.swizzled([1, 1, 1, 1]).operand(),
                    ],
                );
                let offset =
                    self.scratch(ir::Opcode::Add, 3, vec![first.operand(), second.operand()]);
                let c = self.alloc();
                self.exec(
                    ir::Opcode::Add,
                    vec![mask(temp(c), 3)],
                    vec![base.operand(), offset.operand()],
                );
                self.exec(
                    ir::Opcode::Mov,
                    vec![mask(temp(c), 12)],
                    vec![base.operand()],
                );
                let coordinate = Value::new(temp(c));
                match ins.opcode {
                    Bem => to(self, ir::Opcode::Mov, vec![coordinate.operand()]),
                    Texbem => self.sample(w, n, coordinate.operand(), 0, None),
                    _ => {
                        let sampled = self.alloc();
                        let plain = Write {
                            register: temp(sampled),
                            mask: 15,
                            saturate: false,
                            finish: None,
                        };
                        self.sample(&plain, n, coordinate.operand(), 0, None);
                        let luminance = Value::new(constant(BUMP_LUMINANCE, n));
                        let scale = self.scratch(
                            ir::Opcode::Mad,
                            1,
                            vec![perturb.lane(2), luminance.lane(0), luminance.lane(1)],
                        );
                        to(
                            self,
                            ir::Opcode::Mul,
                            vec![Value::new(temp(sampled)).operand(), scale.lane(0)],
                        );
                    }
                }
            }
            _ => return Err("unsupported instruction"),
        }
        Ok(())
    }
}

fn test_instruction(value: ir::Operand, nonzero: bool) -> ir::Instruction {
    let mut test = instruction(ir::Opcode::If, vec![], vec![value]);
    test.test_nonzero = nonzero;
    test
}

fn sampler_dimension(shader: &Shader, ins: &Instruction, index: u8) -> SamplerDimension {
    let implicit = match ins.opcode {
        Opcode::Texreg2rgb | Opcode::Texm3x3tex => SamplerDimension::D3,
        Opcode::Texm3x3spec | Opcode::Texm3x3vspec => SamplerDimension::Cube,
        _ => SamplerDimension::D2,
    };
    shader
        .inputs
        .iter()
        .find(|d| d.register.kind == RegisterKind::Sampler && d.register.index == u16::from(index))
        .and_then(|d| d.sampler)
        .unwrap_or(implicit)
}

fn samples(opcode: Opcode) -> bool {
    use Opcode::*;
    matches!(
        opcode,
        Tex | Texldl
            | Texldd
            | Texbem
            | Texbeml
            | Texreg2ar
            | Texreg2gb
            | Texreg2rgb
            | Texdp3tex
            | Texm3x2tex
            | Texm3x3tex
            | Texm3x3spec
            | Texm3x3vspec
    )
}

pub(super) fn translate(shader: &Shader, ieee_f32: bool) -> Result<Translation, Error> {
    let mut samplers = BTreeMap::new();
    let mut writes_depth = false;
    let mut writes_point_size = shader.outputs.iter().any(|d| d.usage == 4);
    for ins in &shader.instructions {
        if let Some(d) = &ins.destination {
            writes_depth |= d.register.kind == RegisterKind::DepthOutput
                || matches!(ins.opcode, Opcode::Texdepth | Opcode::Texm3x2depth);
            writes_point_size |=
                d.register.kind == RegisterKind::RasterOutput && d.register.index == 2;
        }
        if samples(ins.opcode) {
            let index = if shader.version.major == 1 {
                ins.destination
                    .as_ref()
                    .ok_or_else(|| Error::new(ins.offset, "missing texture destination"))?
                    .register
                    .index as u8
            } else {
                let s = &ins.sources[1];
                if s.register.kind != RegisterKind::Sampler || s.relative.is_some() {
                    return Err(Error::new(
                        ins.offset,
                        "texture instruction requires direct sampler operand",
                    ));
                }
                if !shader.inputs.iter().any(|d| {
                    d.register.kind == RegisterKind::Sampler && d.register.index == s.register.index
                }) {
                    return Err(Error::new(ins.offset, "undeclared sampler"));
                }
                s.register.index as u8
            };
            samplers.insert(index, sampler_dimension(shader, ins, index));
        }
    }
    let lowered = lower(shader)?;
    let p = if shader.stage == Stage::Vertex {
        "vs"
    } else {
        "ps"
    };
    let inputs: Vec<(u32, String)> = lowered
        .inputs
        .iter()
        .map(|&n| {
            let value = match n {
                0..16 => format!("input.v[{n}]"),
                16..24 => format!("input.t[{}]", n - INPUT_TEXCOORD),
                INPUT_POSITION => "input.position".into(),
                _ => "vec4<f32>(select(-1.0, 1.0, input.front_face))".into(),
            };
            (n, value)
        })
        .collect();
    let constants = ["f", "i", "b", "bump_matrix", "bump_luminance"]
        .map(|bank| format!("{p}_constants.{bank}[i]"));
    let result = result_wgsl(&lowered.fields);
    let library = super::wgsl::legacy::Library {
        prefix: p,
        guard_products: !ieee_f32,
        inputs: &inputs,
        input_count: INPUT_COUNT,
        output_count: OUTPUT_COUNT,
        result: &result,
        constants: &constants,
    };
    let source = super::wgsl::legacy::emit(&lowered.module, &library)
        .map_err(|e| Error::new(0, format!("shader emission: {e}")))?;
    if source.len() > MAX_TRANSLATION_BYTES {
        return Err(Error::new(0, "translated source size limit exceeded"));
    }
    let translation = Translation {
        constants: constant_usage(shader),
        source,
        samplers: samplers
            .into_iter()
            .map(|(index, dimension)| SamplerBinding { index, dimension })
            .collect(),
        writes_depth,
        writes_point_size,
    };
    translation.validate(shader.stage)?;
    Ok(translation)
}

fn constant_usage(shader: &Shader) -> adv_device::constants::ConstantUsage {
    let mut used = adv_device::constants::ConstantUsage::default();
    for ins in &shader.instructions {
        for (operand, s) in ins.sources.iter().chain(ins.predicate.iter()).enumerate() {
            let base = match s.register.kind {
                RegisterKind::Float => 0,
                RegisterKind::Int => 256,
                RegisterKind::Bool => 272,
                _ => continue,
            };
            if s.relative.is_some() && base == 0 {
                used.relative = true;
                used.registers[..4].fill(u64::MAX);
            } else {
                let rows = if operand == 1 {
                    match ins.opcode {
                        Opcode::M4x4 | Opcode::M3x4 => 4,
                        Opcode::M4x3 | Opcode::M3x3 => 3,
                        Opcode::M3x2 => 2,
                        _ => 1,
                    }
                } else {
                    1
                };
                for row in 0..rows {
                    used.set(base + usize::from(s.register.index) + row);
                }
            }
        }
        if matches!(ins.opcode, Opcode::Texbem | Opcode::Texbeml | Opcode::Bem) {
            let n = usize::from(ins.destination.as_ref().unwrap().register.index);
            used.set(288 + n);
            if ins.opcode == Opcode::Texbeml {
                used.set(296 + n);
            }
        }
    }
    for ins in &shader.instructions {
        let base = match ins.opcode {
            Opcode::Def => 0,
            Opcode::Defi => 256,
            Opcode::Defb => 272,
            _ => continue,
        };
        let n = base + usize::from(ins.destination.as_ref().unwrap().register.index);
        used.registers[n / 64] &= !(1 << (n % 64));
    }
    used
}

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

/// Read-only constants shared by scalar analysis and the GPU command, numbered
/// as the renderer's register file: c0–c255, i0–i15, b0–b15, bump matrices, bump luminance.
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

/// Texture access for scalar execution; `mode` is as in the stage library's sample functions.
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

/// A lowered program prepared for repeated scalar execution.
#[derive(Clone, Debug)]
pub struct Program {
    prepared: ir::interpreter::Prepared,
    inputs: Vec<u32>,
    fields: Vec<Field>,
}

struct Buffers<'a, C: ?Sized>(&'a C);

impl<C: ConstantSource + ?Sized> ir::interpreter::ConstantBuffers for Buffers<'_, C> {
    fn row(&self, slot: u32, row: u32) -> ir::Vector {
        let (base, rows) = match slot {
            FLOATS => (0, 256),
            INTEGERS => (256, 16),
            BOOLEANS => (272, 16),
            BUMP_MATRIX => (288, 8),
            BUMP_LUMINANCE => (296, 8),
            _ => return [0; 4],
        };
        if row >= rows {
            return [0; 4];
        }
        self.0.register(base + row as usize)
    }
}

struct Textures<'a> {
    sampler: &'a mut dyn Sampler,
    error: Option<String>,
}

impl ir::interpreter::Resources for Textures<'_> {
    fn read(
        &mut self,
        access: ir::interpreter::ResourceAccess,
        arguments: &[ir::Vector],
    ) -> Result<ir::Vector, ir::Error> {
        let vector = |n: usize| arguments[n].map(f32::from_bits);
        let index = access.binding.1 as u8;
        let result = match access.instruction {
            ir::Opcode::Sample => self.sampler.sample(index, vector(0), access.controls),
            ir::Opcode::SampleD => self
                .sampler
                .sample_grad(index, vector(0), vector(1), vector(2)),
            other => return Err(ir::Error::UnsupportedOpcode(other)),
        };
        result.map(|v| v.map(f32::to_bits)).map_err(|e| {
            self.error = Some(e);
            ir::Error::Invalid("texture oracle")
        })
    }

    fn atomic(
        &mut self,
        access: ir::interpreter::ResourceAccess,
        _: &[ir::Vector],
    ) -> Result<ir::Vector, ir::Error> {
        Err(ir::Error::UnsupportedOpcode(access.instruction))
    }

    fn write(
        &mut self,
        access: ir::interpreter::ResourceAccess,
        _: u8,
        _: &[ir::Vector],
    ) -> Result<(), ir::Error> {
        Err(ir::Error::UnsupportedOpcode(access.instruction))
    }
}

impl Program {
    pub fn new(shader: &Shader) -> Result<Self, Error> {
        let lowered = lower(shader)?;
        let prepared = ir::interpreter::Prepared::new(&lowered.module)
            .map_err(|e| Error::new(0, format!("lowered shader: {e}")))?;
        Ok(Self {
            prepared,
            inputs: lowered.inputs,
            fields: lowered.fields,
        })
    }

    /// Run one invocation. Callers run only programs whose translation succeeded.
    pub fn run<C: ConstantSource + ?Sized>(
        &self,
        input: &Inputs,
        constants: &C,
        sampler: &mut dyn Sampler,
    ) -> Result<Outputs, Error> {
        let mut registers = ir::interpreter::Inputs::default();
        for &n in &self.inputs {
            let value = match n {
                0..16 => input.v[n as usize],
                16..24 => input.t[(n - INPUT_TEXCOORD) as usize],
                INPUT_POSITION => input.position,
                _ => [if input.front_face { 1.0 } else { -1.0 }; 4],
            };
            registers
                .registers
                .insert((ir::Register::Input, n), value.map(f32::to_bits));
        }
        let mut textures = Textures {
            sampler,
            error: None,
        };
        let result = self
            .prepared
            .run(&registers, &Buffers(constants), &mut textures)
            .map_err(|e| Error::new(0, textures.error.take().unwrap_or_else(|| e.to_string())))?;
        let read = |register: u32| {
            result
                .registers
                .get(&(ir::Register::Output, register))
                .copied()
                .unwrap_or_default()
                .map(f32::from_bits)
        };
        let mut outputs = Outputs {
            fog: 1.0,
            discarded: result.discarded,
            ..Outputs::default()
        };
        for f in &self.fields {
            let value = read(f.register);
            let vector = match f.target {
                Target::Position => &mut outputs.position,
                Target::Color(k) => &mut outputs.colors[k],
                Target::Texcoord(k) => &mut outputs.texcoords[k],
                Target::Generic(k) => &mut outputs.generic[k],
                Target::Depth => {
                    outputs.depth = value[0];
                    continue;
                }
                Target::PointSize => {
                    outputs.point_size = value[0];
                    continue;
                }
                Target::Fog => {
                    outputs.fog = value[0];
                    continue;
                }
            };
            for lane in 0..4 {
                if f.mask & (1 << lane) != 0 {
                    vector[lane] = value[lane];
                }
            }
        }
        Ok(outputs)
    }
}
