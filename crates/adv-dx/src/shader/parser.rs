use super::*;
use std::collections::BTreeMap;

pub(super) fn parse(tokens: &[u32]) -> Result<Shader, Error> {
    let header = *tokens
        .first()
        .ok_or_else(|| Error::new(0, "missing version"))?;
    let stage = match header >> 16 {
        0xfffe => Stage::Vertex,
        0xffff => Stage::Pixel,
        _ => return Err(Error::new(0, "invalid version token")),
    };
    let version = Version {
        major: ((header >> 8) & 255) as u8,
        minor: (header & 255) as u8,
    };
    let valid = matches!(
        (stage, version.major, version.minor),
        (Stage::Vertex, 1, 1) | (Stage::Pixel, 1, 1..=4) | (_, 2, 0..=1) | (_, 3, 0)
    );
    if !valid {
        return Err(Error::new(0, "unsupported shader version (expected SM1-3)"));
    }
    let mut result = Shader {
        stage,
        version,
        instructions: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        tokens: Vec::new(),
        flow: Vec::new(),
    };
    let mut cursor = 1;
    let mut executable = false;
    loop {
        if cursor >= MAX_SHADER_TOKENS {
            return Err(Error::new(cursor, "shader token limit exceeded"));
        }
        let offset = cursor;
        let token = read(tokens, &mut cursor)?;
        let code = token as u16;
        if code == 0xffff {
            if token != 0xffff {
                return Err(Error::new(offset, "malformed END token"));
            }
            result.tokens = tokens[..cursor].to_vec();
            break;
        }
        if code == 0xfffe {
            if token & 0x80000000 != 0 {
                return Err(Error::new(offset, "malformed comment token"));
            }
            let length = ((token >> 16) & 0x7fff) as usize;
            cursor = cursor
                .checked_add(length)
                .filter(|&n| n <= tokens.len() && n < MAX_SHADER_TOKENS)
                .ok_or_else(|| Error::new(offset, "truncated or oversized comment"))?;
            continue;
        }
        if token & 0xa0000000 != 0 {
            return Err(Error::new(offset, "reserved instruction bits set"));
        }
        let opcode = Opcode::decode(code)
            .ok_or_else(|| Error::new(offset, format!("unknown opcode 0x{code:04x}")))?;
        let controls = ((token >> 16) & 255) as u8;
        if !matches!(
            opcode,
            Opcode::Ifc | Opcode::Breakc | Opcode::Setp | Opcode::Tex
        ) && controls != 0
        {
            return Err(Error::new(offset, "reserved instruction controls"));
        }
        if matches!(opcode, Opcode::Ifc | Opcode::Breakc | Opcode::Setp)
            && !(1..=6).contains(&controls)
        {
            return Err(Error::new(offset, "invalid comparison control"));
        }
        if opcode == Opcode::Tex && (controls > 2 || version.major == 1 && controls != 0) {
            return Err(Error::new(offset, "invalid texture control"));
        }
        let predicated = token & 0x10000000 != 0;
        let coissue = token & 0x40000000 != 0;
        if predicated && version.major < 3 {
            return Err(Error::new(offset, "predication requires SM3"));
        }
        if coissue
            && (stage != Stage::Pixel || version.major != 1 || result.instructions.is_empty())
        {
            return Err(Error::new(offset, "invalid co-issue flag"));
        }
        if version.major == 1 && token & 0x1f000000 != 0 {
            return Err(Error::new(
                offset,
                "SM1 instruction length/predicate bits reserved",
            ));
        }
        validate_opcode(opcode, stage, version, offset)?;
        let (nd, ns, ni) = opcode.arity(version);
        let declaration = opcode == Opcode::Dcl;
        let declaration_token = if declaration {
            Some(read(tokens, &mut cursor)?)
        } else {
            None
        };
        let destination = if nd != 0 {
            Some(destination(tokens, &mut cursor, stage, version)?)
        } else {
            None
        };
        // Predication is encoded after the destination, before ordinary sources.
        let predicate = if predicated {
            Some(source(tokens, &mut cursor, stage, version)?)
        } else {
            None
        };
        if let Some(p) = &predicate
            && (p.register.kind != RegisterKind::Predicate
                || p.relative.is_some()
                || !matches!(p.modifier, SourceModifier::None | SourceModifier::Not))
        {
            return Err(Error::new(offset, "invalid predicate operand"));
        }
        let mut sources = Vec::with_capacity(ns);
        for _ in 0..ns {
            sources.push(source(tokens, &mut cursor, stage, version)?);
        }
        let mut immediate = Vec::with_capacity(ni);
        if !declaration {
            for _ in 0..ni {
                immediate.push(read(tokens, &mut cursor)?);
            }
        }
        if version.major >= 2 && (token >> 24) & 15 != (cursor - offset - 1) as u32 {
            return Err(Error::new(
                offset,
                "instruction length does not match operands",
            ));
        }
        if let Some(d) = &destination {
            let expected = match opcode {
                Opcode::Def => Some(RegisterKind::Float),
                Opcode::Defi => Some(RegisterKind::Int),
                Opcode::Defb => Some(RegisterKind::Bool),
                Opcode::Setp => Some(RegisterKind::Predicate),
                Opcode::Mova => Some(RegisterKind::Address),
                _ => None,
            };
            if expected.is_some_and(|k| k != d.register.kind) {
                return Err(Error::new(
                    offset,
                    "instruction has wrong destination register type",
                ));
            }
            if !declaration
                && !matches!(
                    opcode,
                    Opcode::Def | Opcode::Defi | Opcode::Defb | Opcode::Texkill
                )
                && matches!(
                    d.register.kind,
                    RegisterKind::Input
                        | RegisterKind::Float
                        | RegisterKind::Int
                        | RegisterKind::Bool
                        | RegisterKind::Sampler
                        | RegisterKind::Misc
                        | RegisterKind::Label
                        | RegisterKind::Loop
                )
            {
                return Err(Error::new(offset, "read-only destination register"));
            }
        }
        if matches!(opcode, Opcode::Call | Opcode::CallNz | Opcode::Label)
            && sources[0].register.kind != RegisterKind::Label
        {
            return Err(Error::new(offset, "call/label requires label register"));
        }
        if opcode == Opcode::Loop
            && (sources[0].register.kind != RegisterKind::Loop
                || sources[1].register.kind != RegisterKind::Int)
        {
            return Err(Error::new(offset, "loop requires aL and integer constant"));
        }
        if opcode == Opcode::Rep && sources[0].register.kind != RegisterKind::Int {
            return Err(Error::new(offset, "rep requires integer constant"));
        }
        if let Some(raw) = declaration_token {
            if executable {
                return Err(Error::new(
                    offset,
                    "declaration follows executable instruction",
                ));
            }
            let d = destination.as_ref().expect("DCL arity");
            let legal = match stage {
                Stage::Vertex => {
                    d.register.kind == RegisterKind::Input
                        || version.major == 3
                            && matches!(
                                d.register.kind,
                                RegisterKind::Output | RegisterKind::Sampler
                            )
                }
                Stage::Pixel => {
                    matches!(d.register.kind, RegisterKind::Input | RegisterKind::Sampler)
                        || version.major == 2 && d.register.kind == RegisterKind::Address
                        || version.major == 3 && d.register.kind == RegisterKind::Misc
                }
            };
            if !legal {
                return Err(Error::new(offset, "invalid declaration register class"));
            }
            let allowed = if d.register.kind == RegisterKind::Sampler {
                0xf8000000
            } else {
                0x800f000f
            };
            if raw & 0x80000000 == 0 || raw & !allowed != 0 {
                return Err(Error::new(offset, "reserved declaration bits"));
            }
            let sampler = if d.register.kind == RegisterKind::Sampler {
                Some(match (raw >> 27) & 15 {
                    2 => SamplerDimension::D2,
                    3 => SamplerDimension::Cube,
                    4 => SamplerDimension::D3,
                    _ => return Err(Error::new(offset, "invalid sampler dimension")),
                })
            } else {
                None
            };
            let decl = Declaration {
                usage: (raw & 15) as u8,
                index: ((raw >> 16) & 15) as u8,
                register: d.register,
                mask: d.mask,
                centroid: d.centroid,
                sampler,
            };
            if decl.usage > 13 {
                return Err(Error::new(offset, "unknown declaration semantic"));
            }
            let list = if matches!(
                d.register.kind,
                RegisterKind::Output
                    | RegisterKind::RasterOutput
                    | RegisterKind::ColorOutputLegacy
                    | RegisterKind::ColorOutput
                    | RegisterKind::DepthOutput
            ) {
                &mut result.outputs
            } else {
                &mut result.inputs
            };
            if list
                .iter()
                .any(|old| old.register == decl.register && old.mask & decl.mask != 0)
            {
                return Err(Error::new(offset, "overlapping declaration mask"));
            }
            list.push(decl);
            immediate.push(raw);
        }
        if !matches!(
            opcode,
            Opcode::Dcl | Opcode::Def | Opcode::Defi | Opcode::Defb | Opcode::Nop
        ) {
            executable = true;
        }
        result.instructions.push(Instruction {
            offset,
            opcode,
            controls,
            coissue,
            destination,
            sources,
            predicate,
            immediate,
        });
    }
    result.flow = validate_flow(&result.instructions)?;
    if stage == Stage::Vertex {
        for ins in &result.instructions {
            for source in &ins.sources {
                if source.register.kind == RegisterKind::Input
                    && !result.inputs.iter().any(|d| d.register == source.register)
                {
                    return Err(Error::new(
                        ins.offset,
                        "vertex input register is not declared",
                    ));
                }
            }
        }
    }
    Ok(result)
}
fn read(tokens: &[u32], cursor: &mut usize) -> Result<u32, Error> {
    if *cursor >= MAX_SHADER_TOKENS {
        return Err(Error::new(*cursor, "shader token limit exceeded"));
    }
    let token = tokens
        .get(*cursor)
        .copied()
        .ok_or_else(|| Error::new(*cursor, "truncated shader or missing END"))?;
    *cursor += 1;
    Ok(token)
}
fn register(token: u32, stage: Stage, version: Version, offset: usize) -> Result<Register, Error> {
    if token & 0x80000000 == 0 || token & 0xc000 != 0 {
        return Err(Error::new(offset, "malformed parameter token"));
    }
    let ty = ((token >> 28) & 7) | ((token >> 8) & 24);
    let mut index = (token & 0x7ff) as u16;
    use RegisterKind::*;
    let kind = match ty {
        0 | 16 => Temp,
        1 => Input,
        2 => Float,
        3 => Address,
        4 => RasterOutput,
        5 => ColorOutputLegacy,
        6 => Output,
        7 => Int,
        8 => ColorOutput,
        9 => DepthOutput,
        10 => Sampler,
        11..=13 => {
            index += (ty as u16 - 10) * 2048;
            Float
        }
        14 => Bool,
        15 => Loop,
        17 => Misc,
        18 => Label,
        19 => Predicate,
        _ => return Err(Error::new(offset, "unknown register type")),
    };
    let bound = match kind {
        Temp => {
            if version.major == 1 {
                if stage == Stage::Vertex {
                    12
                } else if version.minor < 4 {
                    2
                } else {
                    6
                }
            } else {
                32
            }
        }
        Input => {
            if stage == Stage::Pixel && version.major < 3 {
                2
            } else {
                16
            }
        }
        Float => {
            if stage == Stage::Pixel {
                if version.major == 1 {
                    8
                } else if version.major == 2 {
                    32
                } else {
                    224
                }
            } else {
                256
            }
        }
        Address => {
            if stage == Stage::Pixel {
                if version.major == 1 {
                    if version.minor < 4 { 4 } else { 6 }
                } else {
                    8
                }
            } else {
                1
            }
        }
        RasterOutput => 3,
        ColorOutputLegacy => 2,
        Output => {
            if version.major == 3 {
                12
            } else {
                8
            }
        }
        Int | Bool => 16,
        Sampler => {
            if stage == Stage::Vertex {
                4
            } else {
                16
            }
        }
        ColorOutput => 4,
        DepthOutput | Loop | Predicate => 1,
        Misc => 2,
        Label => 2048,
    };
    if index >= bound {
        return Err(Error::new(
            offset,
            format!(
                "register {kind:?}{index} exceeds SM{} limit {bound}",
                version.major
            ),
        ));
    }
    if stage == Stage::Vertex && matches!(kind, ColorOutput | DepthOutput | Misc) {
        return Err(Error::new(offset, "pixel-only register in vertex shader"));
    }
    if stage == Stage::Pixel && matches!(kind, RasterOutput | ColorOutputLegacy) {
        return Err(Error::new(offset, "vertex-only register in pixel shader"));
    }
    if stage == Stage::Vertex
        && version.major == 3
        && matches!(kind, RasterOutput | ColorOutputLegacy)
    {
        return Err(Error::new(
            offset,
            "SM3 vertex outputs require generic declared output registers",
        ));
    }
    if version.major < 3 && matches!(kind, Predicate | Misc) {
        return Err(Error::new(offset, "register requires SM3"));
    }
    if version.major == 1
        && matches!(
            kind,
            Int | Bool | Sampler | Loop | Label | ColorOutput | DepthOutput
        )
    {
        return Err(Error::new(offset, "register class unavailable in SM1"));
    }
    Ok(Register { kind, index })
}
fn relative(
    tokens: &[u32],
    cursor: &mut usize,
    stage: Stage,
    version: Version,
    token: u32,
    dest: bool,
) -> Result<Option<Relative>, Error> {
    if token & 0x2000 == 0 {
        return Ok(None);
    }
    if dest && (stage != Stage::Vertex || version.major < 3)
        || !dest && stage == Stage::Pixel && version.major < 3
    {
        return Err(Error::new(
            *cursor - 1,
            "relative addressing unavailable for shader version",
        ));
    }
    if version.major == 1 {
        return Ok(Some(Relative {
            register: Register {
                kind: RegisterKind::Address,
                index: 0,
            },
            component: 0,
        }));
    }
    let offset = *cursor;
    let rel = read(tokens, cursor)?;
    let reg = register(rel, stage, version, offset)?;
    if !matches!(reg.kind, RegisterKind::Address | RegisterKind::Loop) || rel & 0x0f002000 != 0 {
        return Err(Error::new(offset, "invalid relative addressing register"));
    }
    let component = ((rel >> 16) & 3) as u8;
    if (0..4).any(|i| ((rel >> (16 + i * 2)) & 3) != u32::from(component)) {
        return Err(Error::new(
            offset,
            "relative address must use replicated swizzle",
        ));
    }
    Ok(Some(Relative {
        register: reg,
        component,
    }))
}
fn source(
    tokens: &[u32],
    cursor: &mut usize,
    stage: Stage,
    version: Version,
) -> Result<Source, Error> {
    let offset = *cursor;
    let token = read(tokens, cursor)?;
    let register = register(token, stage, version, offset)?;
    let modifier = match (token >> 24) & 15 {
        0 => SourceModifier::None,
        1 => SourceModifier::Negate,
        2 => SourceModifier::Bias,
        3 => SourceModifier::BiasNegate,
        4 => SourceModifier::Sign,
        5 => SourceModifier::SignNegate,
        6 => SourceModifier::Complement,
        7 => SourceModifier::X2,
        8 => SourceModifier::X2Negate,
        9 => SourceModifier::DivideZ,
        10 => SourceModifier::DivideW,
        11 => SourceModifier::Abs,
        12 => SourceModifier::AbsNegate,
        13 => SourceModifier::Not,
        _ => return Err(Error::new(offset, "reserved source modifier")),
    };
    let relative = relative(tokens, cursor, stage, version, token, false)?;
    if relative.is_some() && !matches!(register.kind, RegisterKind::Float | RegisterKind::Input) {
        return Err(Error::new(
            offset,
            "unsupported relative source register class",
        ));
    }
    Ok(Source {
        register,
        modifier,
        relative,
        swizzle: std::array::from_fn(|i| ((token >> (16 + i * 2)) & 3) as u8),
    })
}
fn destination(
    tokens: &[u32],
    cursor: &mut usize,
    stage: Stage,
    version: Version,
) -> Result<Destination, Error> {
    let offset = *cursor;
    let token = read(tokens, cursor)?;
    let register = register(token, stage, version, offset)?;
    if token & 0x00800000 != 0 {
        return Err(Error::new(offset, "reserved destination modifier"));
    }
    let shift = ((token >> 24) & 15) as i8;
    let shift = if shift >= 8 { shift - 16 } else { shift };
    if shift != 0 && (stage != Stage::Pixel || version.major != 1) || !(-3..=3).contains(&shift) {
        return Err(Error::new(offset, "invalid destination shift"));
    }
    let mask = ((token >> 16) & 15) as u8;
    if mask == 0 {
        return Err(Error::new(offset, "empty destination write mask"));
    }
    let relative = relative(tokens, cursor, stage, version, token, true)?;
    if relative.is_some() && register.kind != RegisterKind::Output {
        return Err(Error::new(
            offset,
            "relative destination must be output register",
        ));
    }
    Ok(Destination {
        register,
        mask,
        saturate: token & 0x100000 != 0,
        partial_precision: token & 0x200000 != 0,
        centroid: token & 0x400000 != 0,
        shift,
        relative,
    })
}
fn validate_opcode(op: Opcode, stage: Stage, v: Version, offset: usize) -> Result<(), Error> {
    use Opcode::*;
    if matches!(
        op,
        Texcoord
            | Texkill
            | Tex
            | Texbem
            | Texbeml
            | Texreg2ar
            | Texreg2gb
            | Texm3x2pad
            | Texm3x2tex
            | Texm3x3pad
            | Texm3x3tex
            | Texm3x3spec
            | Texm3x3vspec
            | Cnd
            | Texreg2rgb
            | Texdp3tex
            | Texm3x2depth
            | Texdp3
            | Texm3x3
            | Texdepth
            | Cmp
            | Bem
            | Dp2add
            | Dsx
            | Dsy
            | Texldd
            | Phase
    ) && stage != Stage::Pixel
    {
        return Err(Error::new(offset, "pixel-only opcode in vertex shader"));
    }
    if matches!(
        op,
        Call | CallNz
            | Loop
            | Ret
            | EndLoop
            | Label
            | Pow
            | Crs
            | Sgn
            | Abs
            | Nrm
            | SinCos
            | Rep
            | EndRep
            | If
            | Ifc
            | Else
            | EndIf
            | Break
            | Breakc
            | Mova
            | Defb
            | Defi
            | Dp2add
            | Dsx
            | Dsy
            | Texldd
            | Setp
            | Texldl
            | Breakp
    ) && v.major < 2
    {
        return Err(Error::new(offset, "opcode requires SM2 or later"));
    }
    if op == Dcl && stage == Stage::Pixel && v.major < 2 {
        return Err(Error::new(offset, "pixel input declarations require SM2"));
    }
    if matches!(op, Setp | Texldl | Breakp) && v.major < 3 {
        return Err(Error::new(offset, "opcode requires SM3"));
    }
    if matches!(
        op,
        Texbem
            | Texbeml
            | Texreg2ar
            | Texreg2gb
            | Texm3x2pad
            | Texm3x2tex
            | Texm3x3pad
            | Texm3x3tex
            | Texm3x3spec
            | Texm3x3vspec
            | Cnd
            | Texreg2rgb
            | Texdp3tex
            | Texm3x2depth
            | Texdp3
            | Texm3x3
            | Texdepth
            | Bem
            | Phase
    ) && v.major != 1
    {
        return Err(Error::new(offset, "legacy texture opcode requires PS1"));
    }
    if op == Phase && v.minor != 4 {
        return Err(Error::new(offset, "phase requires PS1.4"));
    }
    Ok(())
}
fn validate_flow(instructions: &[Instruction]) -> Result<Vec<Flow>, Error> {
    let mut flow = vec![Flow::default(); instructions.len()];
    let mut stack: Vec<(Opcode, usize, Option<usize>)> = Vec::new();
    let mut labels = BTreeMap::new();
    for (pc, ins) in instructions.iter().enumerate() {
        use Opcode::*;
        match ins.opcode {
            If | Ifc | Loop | Rep => {
                if stack.len() >= MAX_FLOW_DEPTH {
                    return Err(Error::new(ins.offset, "flow nesting limit exceeded"));
                }
                stack.push((ins.opcode, pc, None));
            }
            Else => {
                let Some((If | Ifc, start, else_pc)) = stack.last_mut() else {
                    return Err(Error::new(ins.offset, "else without if"));
                };
                if else_pc.is_some() {
                    return Err(Error::new(ins.offset, "duplicate else"));
                }
                flow[*start].target = pc + 1;
                *else_pc = Some(pc);
            }
            EndIf | EndLoop | EndRep => {
                let Some((kind, start, else_pc)) = stack.pop() else {
                    return Err(Error::new(ins.offset, "unmatched flow terminator"));
                };
                if !matches!(
                    (kind, ins.opcode),
                    (If | Ifc, EndIf) | (Loop, EndLoop) | (Rep, EndRep)
                ) {
                    return Err(Error::new(ins.offset, "mismatched flow terminator"));
                }
                flow[start].end = pc + 1;
                flow[pc].target = start;
                if let Some(e) = else_pc {
                    flow[e].target = pc + 1;
                } else {
                    flow[start].target = pc + 1;
                }
                if matches!(kind, Loop | Rep) {
                    for b in start + 1..pc {
                        if matches!(instructions[b].opcode, Break | Breakc | Breakp)
                            && flow[b].end == usize::MAX
                        {
                            flow[b].target = pc + 1;
                            flow[b].end = start;
                        }
                    }
                }
            }
            Break | Breakc | Breakp => {
                if !stack.iter().any(|(kind, _, _)| matches!(kind, Loop | Rep)) {
                    return Err(Error::new(ins.offset, "break outside loop"));
                }
                flow[pc].end = usize::MAX;
            }
            Label => {
                let label = ins.sources[0].register.index;
                if labels.insert(label, pc).is_some() {
                    return Err(Error::new(ins.offset, "duplicate label"));
                }
            }
            _ => {}
        }
    }
    if let Some((_, pc, _)) = stack.last() {
        return Err(Error::new(
            instructions[*pc].offset,
            "unterminated flow block",
        ));
    }
    for (pc, ins) in instructions.iter().enumerate() {
        if matches!(ins.opcode, Opcode::Call | Opcode::CallNz) {
            flow[pc].target = *labels
                .get(&ins.sources[0].register.index)
                .ok_or_else(|| Error::new(ins.offset, "undefined call label"))?;
        }
    }
    // Recursive calls have no finite D3D shader execution contract.
    fn visits(
        pc: usize,
        ins: &[Instruction],
        flow: &[Flow],
        path: &mut Vec<usize>,
        complete: &mut BTreeMap<usize, usize>,
    ) -> Result<usize, Error> {
        if path.contains(&pc) {
            return Err(Error::new(ins[pc].offset, "recursive shader call"));
        }
        if path.len() >= MAX_FLOW_DEPTH {
            return Err(Error::new(ins[pc].offset, "call depth limit exceeded"));
        }
        if let Some(depth) = complete.get(&pc) {
            return Ok(*depth);
        }
        path.push(pc);
        let mut depth = 1;
        for i in pc..ins.len() {
            if ins[i].opcode == Opcode::Ret {
                break;
            }
            if matches!(ins[i].opcode, Opcode::Call | Opcode::CallNz) {
                depth = depth.max(1 + visits(flow[i].target, ins, flow, path, complete)?);
            }
        }
        path.pop();
        if depth > MAX_FLOW_DEPTH {
            return Err(Error::new(ins[pc].offset, "call depth limit exceeded"));
        }
        complete.insert(pc, depth);
        Ok(depth)
    }
    let mut complete = BTreeMap::new();
    for (pc, ins) in instructions.iter().enumerate() {
        if matches!(ins.opcode, Opcode::Call | Opcode::CallNz) {
            visits(
                flow[pc].target,
                instructions,
                &flow,
                &mut Vec::new(),
                &mut complete,
            )?;
        }
    }
    Ok(flow)
}
fn reg_name(r: Register, stage: Stage) -> String {
    use RegisterKind::*;
    let prefix = match r.kind {
        Temp => "r",
        Input => "v",
        Float => "c",
        Address => {
            if stage == Stage::Pixel {
                "t"
            } else {
                "a"
            }
        }
        RasterOutput => "oR",
        ColorOutputLegacy => "oD",
        Output => "oT",
        Int => "i",
        ColorOutput => "oC",
        DepthOutput => "oDepth",
        Sampler => "s",
        Bool => "b",
        Loop => "aL",
        Misc => "vMisc",
        Label => "l",
        Predicate => "p",
    };
    format!("{prefix}{}", r.index)
}
pub(super) fn disassemble(s: &Shader) -> String {
    use std::fmt::Write;
    let mut out = format!(
        "{}{}_{}\n",
        if s.stage == Stage::Vertex {
            "vs_"
        } else {
            "ps_"
        },
        s.version.major,
        s.version.minor
    );
    for i in &s.instructions {
        let _ = write!(
            out,
            "{:04x}: {}{:?}",
            i.offset,
            if i.coissue { "+" } else { "" },
            i.opcode
        );
        if i.controls != 0 {
            let _ = write!(out, "[{}]", i.controls);
        }
        if let Some(p) = &i.predicate {
            let _ = write!(out, " ({})", reg_name(p.register, s.stage));
        }
        if let Some(d) = &i.destination {
            let _ = write!(
                out,
                " {}.{}{}",
                reg_name(d.register, s.stage),
                mask(d.mask),
                if d.saturate { "_sat" } else { "" }
            );
            if let Some(r) = d.relative {
                let _ = write!(out, "[{}.{:?}]", reg_name(r.register, s.stage), r.component);
            }
            if d.shift != 0 {
                let _ = write!(out, "_shift{}", d.shift);
            }
        }
        for src in &i.sources {
            let _ = write!(
                out,
                ", {:?}({}.{}{}{}{}{:?})",
                src.modifier,
                reg_name(src.register, s.stage),
                "xyzw".chars().nth(src.swizzle[0] as usize).unwrap_or('x'),
                "xyzw".chars().nth(src.swizzle[1] as usize).unwrap_or('x'),
                "xyzw".chars().nth(src.swizzle[2] as usize).unwrap_or('x'),
                "xyzw".chars().nth(src.swizzle[3] as usize).unwrap_or('x'),
                src.relative
            );
        }
        for imm in &i.immediate {
            let _ = write!(out, " 0x{imm:08x}");
        }
        out.push('\n');
    }
    out
}
fn mask(mask: u8) -> String {
    (0..4)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| "xyzw".chars().nth(i).unwrap_or('x'))
        .collect()
}
