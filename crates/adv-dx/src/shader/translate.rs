use super::*;
use std::collections::BTreeMap;
use std::fmt::Write;

pub(super) fn translate(shader: &Shader) -> Result<Translation, Error> {
    let p = if shader.stage == Stage::Vertex {
        "vs"
    } else {
        "ps"
    };
    let mut samplers = BTreeMap::new();
    let mut writes_depth = false;
    let mut writes_point_size = false;
    let mut writes_fog = false;
    for ins in &shader.instructions {
        if let Some(d) = &ins.destination {
            writes_depth |= d.register.kind == RegisterKind::DepthOutput
                || matches!(ins.opcode, Opcode::Texdepth | Opcode::Texm3x2depth);
            writes_point_size |=
                d.register.kind == RegisterKind::RasterOutput && d.register.index == 2;
            writes_fog |= d.register.kind == RegisterKind::RasterOutput && d.register.index == 1;
            if d.register.kind == RegisterKind::ColorOutput && d.register.index >= 2 {
                return Err(Error::new(
                    ins.offset,
                    "renderer supports only two color outputs",
                ));
            }
        }
        if matches!(
            ins.opcode,
            Opcode::Tex
                | Opcode::Texldl
                | Opcode::Texldd
                | Opcode::Texbem
                | Opcode::Texbeml
                | Opcode::Texreg2ar
                | Opcode::Texreg2gb
                | Opcode::Texreg2rgb
                | Opcode::Texdp3tex
                | Opcode::Texm3x2tex
                | Opcode::Texm3x3tex
                | Opcode::Texm3x3spec
                | Opcode::Texm3x3vspec
        ) {
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
                s.register.index as u8
            };
            let implicit_dimension = match ins.opcode {
                Opcode::Texreg2rgb | Opcode::Texm3x3tex => SamplerDimension::D3,
                Opcode::Texm3x3spec | Opcode::Texm3x3vspec => SamplerDimension::Cube,
                _ => SamplerDimension::D2,
            };
            let dimension = shader
                .inputs
                .iter()
                .find(|d| {
                    d.register.kind == RegisterKind::Sampler && d.register.index == u16::from(index)
                })
                .and_then(|d| d.sampler)
                .unwrap_or(implicit_dimension);
            if shader.version.major >= 2
                && !shader.inputs.iter().any(|d| {
                    d.register.kind == RegisterKind::Sampler && d.register.index == u16::from(index)
                })
            {
                return Err(Error::new(ins.offset, "undeclared sampler"));
            }
            samplers.insert(index, dimension);
        }
    }
    for d in &shader.outputs {
        if d.usage == 11 {
            writes_fog = true;
        }
        if d.usage == 4 {
            writes_point_size = true;
        }
    }
    let mut out = String::new();
    // Explicit bit classification preserves D3D comparisons and 0*infinity even
    // when FXC otherwise folds multiplication by zero and x==x under fast math.
    writeln!(out,"fn {p}_mul_scalar(a:f32,b:f32)->f32 {{ let aa=bitcast<u32>(a)&0x7fffffffu; let bb=bitcast<u32>(b)&0x7fffffffu; if aa>0x7f800000u || bb>0x7f800000u || (aa==0x7f800000u && bb==0u) || (bb==0x7f800000u && aa==0u) {{ return bitcast<f32>(0x7fc00000u); }} return a*b; }}\nfn {p}_mul(a:vec4<f32>,b:vec4<f32>)->vec4<f32> {{ return vec4<f32>({p}_mul_scalar(a.x,b.x),{p}_mul_scalar(a.y,b.y),{p}_mul_scalar(a.z,b.z),{p}_mul_scalar(a.w,b.w)); }}").unwrap();
    writeln!(out,"fn {p}_cmp_scalar(a:f32,b:f32,mode:u32)->bool {{ let aa=bitcast<u32>(a)&0x7fffffffu; let bb=bitcast<u32>(b)&0x7fffffffu; if aa>0x7f800000u || bb>0x7f800000u {{ return mode==5u; }} switch mode {{ case 1u:{{return a>b;}} case 2u:{{return a==b;}} case 3u:{{return a>=b;}} case 4u:{{return a<b;}} case 5u:{{return a!=b;}} default:{{return a<=b;}} }} }}\nfn {p}_compare(a:vec4<f32>,b:vec4<f32>,mode:u32)->vec4<bool> {{ return vec4<bool>({p}_cmp_scalar(a.x,b.x,mode),{p}_cmp_scalar(a.y,b.y,mode),{p}_cmp_scalar(a.z,b.z,mode),{p}_cmp_scalar(a.w,b.w,mode)); }}").unwrap();
    writeln!(out,"fn {p}_dot2(a:vec4<f32>,b:vec4<f32>)->f32 {{let v={p}_mul(a,b);return v.x+v.y;}}\nfn {p}_dot3(a:vec4<f32>,b:vec4<f32>)->f32 {{let v={p}_mul(a,b);return v.x+v.y+v.z;}}\nfn {p}_dot4(a:vec4<f32>,b:vec4<f32>)->f32 {{let v={p}_mul(a,b);return v.x+v.y+v.z+v.w;}}").unwrap();
    // D3D's rcp/rsq/log edge cases are explicit instead of relying on WGSL division-by-zero.
    writeln!(out,"fn {p}_rcp(x:f32)->f32 {{ if x == 0.0 {{ return bitcast<f32>(0x7f800000u); }} return 1.0 / x; }}").unwrap();
    writeln!(out,"fn {p}_rsq(x:f32)->f32 {{ if x == 0.0 {{ return bitcast<f32>(0x7f800000u); }} return inverseSqrt(abs(x)); }}").unwrap();
    writeln!(out,"fn {p}_log(x:f32)->f32 {{ if x == 0.0 {{ return bitcast<f32>(0xff800000u); }} return log2(abs(x)); }}").unwrap();
    writeln!(out,"fn {p}_lit(x:vec4<f32>)->vec4<f32> {{ var z=0.0; if x.x > 0.0 {{ z=pow(max(x.y,0.0),clamp(x.w,-128.0,128.0)); }} return vec4<f32>(1.0,max(x.x,0.0),z,1.0); }}").unwrap();
    writeln!(out,"fn {p}_run(input:ShaderInputs)->ShaderOutputs {{\nvar r:array<vec4<f32>,32>; var t=input.t; var rr:array<vec4<f32>,32>; var tt:array<vec4<f32>,8>; var a=vec4<f32>(0.0); var al=0i; var pred=vec4<f32>(0.0);\nvar ro:array<vec4<f32>,3>; var od:array<vec4<f32>,2>; var o:array<vec4<f32>,12>; var oc:array<vec4<f32>,2>; var depth=0.0;\nvar c={p}_constants.f; var ci={p}_constants.i; var cb={p}_constants.b;\nvar matrix=vec3<f32>(0.0); var matrix_n=0u;\nvar pc=0u; var calls:array<u32,32>; var call_n=0u; var loops:array<i32,32>; var loop_steps:array<i32,32>; var loop_saved:array<i32,32>; var loop_n=0u; var steps=0u;").unwrap();
    for ins in &shader.instructions {
        if matches!(ins.opcode, Opcode::Def | Opcode::Defi | Opcode::Defb) {
            let index = ins.destination.as_ref().expect("definition").register.index;
            match ins.opcode {
                Opcode::Def => {
                    write!(out, "c[{index}u]=vec4<f32>(").unwrap();
                    for (i, bits) in ins.immediate.iter().enumerate() {
                        if i != 0 {
                            out.push(',');
                        }
                        write!(out, "bitcast<f32>(0x{bits:08x}u)").unwrap();
                    }
                    out.push_str(");\n");
                }
                Opcode::Defi => {
                    writeln!(
                        out,
                        "ci[{index}u]=bitcast<vec4<i32>>(vec4<u32>({}u,{}u,{}u,{}u));",
                        ins.immediate[0], ins.immediate[1], ins.immediate[2], ins.immediate[3]
                    )
                    .unwrap();
                }
                Opcode::Defb => {
                    writeln!(
                        out,
                        "cb[{index}u]=vec4<i32>({});",
                        i32::from(ins.immediate[0] != 0)
                    )
                    .unwrap();
                }
                _ => {}
            }
        }
    }
    let has_flow = shader.instructions.iter().any(|i| {
        matches!(
            i.opcode,
            Opcode::Call
                | Opcode::CallNz
                | Opcode::Loop
                | Opcode::Ret
                | Opcode::EndLoop
                | Opcode::Label
                | Opcode::Rep
                | Opcode::EndRep
                | Opcode::If
                | Opcode::Ifc
                | Opcode::Else
                | Opcode::EndIf
                | Opcode::Break
                | Opcode::Breakc
                | Opcode::Breakp
        )
    });
    if has_flow {
        writeln!(
            out,
            "loop {{ if pc >= {}u || steps >= {}u {{ break; }} steps += 1u; let selected_pc=pc;",
            shader.instructions.len(),
            MAX_EXECUTION_STEPS
        )
        .unwrap();
    }
    for (pc, ins) in shader.instructions.iter().enumerate() {
        if has_flow {
            writeln!(out, "if selected_pc == {pc}u {{ pc={}u;", pc + 1).unwrap();
        } else {
            out.push_str("{\n");
        }
        if shader.stage == Stage::Pixel && shader.version.major == 1 && !ins.coissue {
            out.push_str("rr=r; tt=t;\n");
        }
        if let Some(pr) = &ins.predicate {
            writeln!(out, "if ({}) .x != 0.0 {{", src(pr, shader, p, 0)).unwrap();
        }
        emit_instruction(&mut out, shader, ins, pc, p)?;
        if ins.predicate.is_some() {
            out.push_str("}\n");
        }
        out.push_str("}\n");
        if out.len() > MAX_TRANSLATION_BYTES {
            return Err(Error::new(
                ins.offset,
                "translated source size limit exceeded",
            ));
        }
    }
    if has_flow {
        out.push_str("}\n");
    }
    out.push_str("var result:ShaderOutputs; result.position=ro[0]; result.colors=od; result.texcoords=array<vec4<f32>,8>(o[0],o[1],o[2],o[3],o[4],o[5],o[6],o[7]); result.depth=depth; result.point_size=ro[2].x;\n");
    out.push_str("result.generic=o;\n");
    out.push_str(if writes_fog {
        "result.fog=ro[1].x;\n"
    } else {
        "result.fog=1.0;\n"
    });
    if shader.stage == Stage::Pixel {
        if shader.version.major == 1 {
            out.push_str("result.colors[0]=r[0];\n");
        } else {
            out.push_str("result.colors=oc;\n");
        }
    }
    if shader.stage == Stage::Vertex && shader.version.major == 3 {
        out.push_str("result.position=vec4<f32>(0.0); result.colors=array<vec4<f32>,2>(); result.texcoords=array<vec4<f32>,8>();\n");
        for d in &shader.outputs {
            let dest = match d.usage {
                0 => "result.position".into(),
                4 => "result.point_size".into(),
                5 if d.index < 8 => format!("result.texcoords[{}]", d.index),
                10 if d.index < 2 => format!("result.colors[{}]", d.index),
                11 => "result.fog".into(),
                _ => continue,
            };
            if matches!(d.usage, 4 | 11) {
                writeln!(out, "{dest}=o[{}].x;", d.register.index).unwrap();
            } else {
                for component in 0..4 {
                    if d.mask & (1 << component) != 0 {
                        let ch = component_name(component);
                        writeln!(out, "{dest}.{ch}=o[{}].{ch};", d.register.index).unwrap();
                    }
                }
            }
        }
    }
    out.push_str("return result;\n}\n");
    let translation = Translation {
        constants: constant_usage(shader),
        source: out,
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
fn component_name(i: usize) -> char {
    "xyzw".chars().nth(i).unwrap_or('x')
}
fn idx(register: Register, relative: Option<Relative>, offset: u16) -> String {
    let base = u32::from(register.index) + u32::from(offset);
    match relative {
        None => format!("{base}u"),
        Some(r) => format!(
            "u32(clamp({base}i+{},0i,{}i))",
            rel(r),
            if register.kind == RegisterKind::Float {
                255
            } else if register.kind == RegisterKind::Output {
                11
            } else {
                15
            }
        ),
    }
}
fn rel(r: Relative) -> String {
    if r.register.kind == RegisterKind::Loop {
        "al".into()
    } else {
        format!("i32(a.{})", component_name(r.component as usize))
    }
}
fn raw(register: Register, relative: Option<Relative>, shader: &Shader, offset: u16) -> String {
    let i = idx(register, relative, offset);
    use RegisterKind::*;
    match register.kind {
        Temp => format!("r[{i}]"),
        Input => {
            let value = format!("input.v[{i}]");
            let value = if let Some(rr) = relative {
                let index = format!("{}i+{}", register.index, rel(rr));
                format!("select(vec4<f32>(0.0),{value},({index})>=0i&&({index})<16i)")
            } else {
                value
            };
            if shader.stage == Stage::Pixel && shader.version.major == 1 {
                format!("clamp({value},vec4<f32>(0.0),vec4<f32>(1.0))")
            } else {
                value
            }
        }
        Float => {
            let c = format!("c[{i}]");
            if let Some(rr) = relative {
                let index = format!(
                    "{}i+{}",
                    u32::from(register.index) + u32::from(offset),
                    rel(rr)
                );
                let bound = if shader.stage == Stage::Pixel {
                    if shader.version.major == 1 {
                        8
                    } else if shader.version.major == 2 {
                        32
                    } else {
                        224
                    }
                } else {
                    256
                };
                format!("select(vec4<f32>(0.0),{c},({index})>=0i&&({index})<{bound}i)")
            } else {
                if shader.stage == Stage::Pixel && shader.version.major == 1 {
                    format!("clamp({c},vec4<f32>(-1.0),vec4<f32>(1.0))")
                } else {
                    c
                }
            }
        }
        Address => {
            if shader.stage == Stage::Pixel {
                format!("t[{i}]")
            } else {
                "a".into()
            }
        }
        RasterOutput => format!("ro[{i}]"),
        ColorOutputLegacy => format!("od[{i}]"),
        Output => format!("o[{i}]"),
        Int => format!("vec4<f32>(ci[{i}])"),
        ColorOutput => format!("oc[{i}]"),
        DepthOutput => "vec4<f32>(depth)".into(),
        Bool => format!("vec4<f32>(cb[{i}])"),
        Loop => "vec4<f32>(f32(al))".into(),
        Misc => {
            if register.index == 0 {
                "input.position".into()
            } else {
                "vec4<f32>(select(-1.0,1.0,input.front_face))".into()
            }
        }
        Predicate => "pred".into(),
        Sampler | Label => "vec4<f32>(0.0)".into(),
    }
}
fn src(source: &Source, shader: &Shader, _p: &str, offset: u16) -> String {
    let value = raw(source.register, source.relative, shader, offset);
    let sw: String = source
        .swizzle
        .iter()
        .map(|&c| component_name(c as usize))
        .collect();
    let v = format!("({value}).{sw}");
    use SourceModifier::*;
    match source.modifier {
        None => v,
        Negate => format!("(-{v})"),
        Bias => format!("({v}-vec4<f32>(0.5))"),
        BiasNegate => format!("(vec4<f32>(0.5)-{v})"),
        Sign => format!("({v}*2.0-vec4<f32>(1.0))"),
        SignNegate => format!("(vec4<f32>(1.0)-{v}*2.0)"),
        Complement => format!("(vec4<f32>(1.0)-{v})"),
        X2 => format!("({v}*2.0)"),
        X2Negate => format!("({v}*(-2.0))"),
        DivideZ => format!("({v}/({v}).z)"),
        DivideW => format!("({v}/({v}).w)"),
        Abs => format!("abs({v})"),
        AbsNegate => format!("(-abs({v}))"),
        Not => format!("select(vec4<f32>(0.0),vec4<f32>(1.0),{v}==vec4<f32>(0.0))"),
    }
}
fn compare(a: &str, b: &str, control: u8, p: &str) -> String {
    format!("{p}_cmp_scalar({a},{b},{control}u)")
}
fn emit_write(out: &mut String, d: &Destination, value: &str, shader: &Shader) {
    let mut rhs = value.to_string();
    if d.shift != 0 {
        rhs = format!("({rhs}*{})", 2.0f32.powi(i32::from(d.shift)));
    }
    if d.saturate {
        rhs = format!("clamp({rhs},vec4<f32>(0.0),vec4<f32>(1.0))");
    }
    // Match the renderer's advertised PixelShader1xMaxValue=8.
    if shader.stage == Stage::Pixel
        && shader.version.major == 1
        && d.register.kind == RegisterKind::Temp
    {
        rhs = format!("clamp({rhs},vec4<f32>(-8.0),vec4<f32>(8.0))");
    }
    writeln!(out, "let value={rhs};").unwrap();
    if let Some(rr) = d.relative {
        writeln!(
            out,
            "if {}i+{} >=0i && {}i+{} <12i {{",
            d.register.index,
            rel(rr),
            d.register.index,
            rel(rr)
        )
        .unwrap();
    }
    let target = raw(d.register, d.relative, shader, 0);
    if d.register.kind == RegisterKind::DepthOutput {
        out.push_str("depth=value.x;\n");
    } else {
        for i in 0..4 {
            if d.mask & (1 << i) != 0 {
                let c = component_name(i);
                writeln!(out, "{target}.{c}=value.{c};").unwrap();
            }
        }
    }
    if d.relative.is_some() {
        out.push_str("}\n");
    }
}
fn emit_instruction(
    out: &mut String,
    s: &Shader,
    i: &Instruction,
    pc: usize,
    p: &str,
) -> Result<(), Error> {
    use Opcode::*;
    let operands: Vec<_> = i
        .sources
        .iter()
        .map(|x| {
            let value = src(x, s, p, 0);
            if i.coissue {
                value.replace("r[", "rr[").replace("t[", "tt[")
            } else {
                value
            }
        })
        .collect();
    let a = operands.first().map_or("vec4<f32>(0.0)", String::as_str);
    let b = operands.get(1).map_or("vec4<f32>(0.0)", String::as_str);
    let c = operands.get(2).map_or("vec4<f32>(0.0)", String::as_str);
    let f = s.flow[pc];
    let mut value = String::new();
    match i.opcode {
        Nop | Dcl | Def | Defi | Defb | Label | Phase | EndIf => {}
        If | Ifc => {
            let cond = if i.opcode == If {
                format!("({a}).x!=0.0")
            } else {
                compare(&format!("({a}).x"), &format!("({b}).x"), i.controls, p)
            };
            writeln!(out, "if !({cond}) {{ pc={}u; }}", f.target).unwrap();
        }
        Else => {
            writeln!(out, "pc={}u;", f.target).unwrap();
        }
        Call | CallNz => {
            if i.opcode == CallNz {
                writeln!(out, "if ({b}).x!=0.0 {{").unwrap();
            }
            writeln!(
                out,
                "if call_n>=32u {{ pc={}u; }} else {{calls[call_n]=pc; call_n+=1u; pc={}u;}}",
                s.instructions.len(),
                f.target
            )
            .unwrap();
            if i.opcode == CallNz {
                out.push_str("}\n");
            }
        }
        Ret => {
            writeln!(
                out,
                "if call_n==0u {{ pc={}u; }} else {{ call_n-=1u; pc=calls[call_n]; }}",
                s.instructions.len()
            )
            .unwrap();
        }
        Loop | Rep => {
            let counter = if i.opcode == Loop { b } else { a };
            writeln!(out,"let count=i32(({counter}).x); if count<=0i {{ pc={}u; }} else {{ loops[loop_n]=count; loop_steps[loop_n]=i32(({counter}).z); loop_saved[loop_n]=al; loop_n+=1u;",f.end).unwrap();
            if i.opcode == Loop {
                writeln!(out, "al=i32(({counter}).y);").unwrap();
            }
            out.push_str("}\n");
        }
        EndLoop | EndRep => {
            out.push_str("loops[loop_n-1u]-=1i; if loops[loop_n-1u]>0i {");
            if i.opcode == EndLoop {
                out.push_str("al+=loop_steps[loop_n-1u];");
            }
            writeln!(
                out,
                "pc={}u; }} else {{loop_n-=1u; al=loop_saved[loop_n];}}",
                f.target + 1
            )
            .unwrap();
        }
        Break | Breakc | Breakp => {
            let cond = match i.opcode {
                Break => "true".into(),
                Breakp => format!("({a}).x!=0.0"),
                _ => compare(&format!("({a}).x"), &format!("({b}).x"), i.controls, p),
            };
            writeln!(
                out,
                "if {cond} {{ loop_n-=1u; al=loop_saved[loop_n]; pc={}u; }}",
                f.target
            )
            .unwrap();
        }
        Mov => {
            value = if s.stage == Stage::Vertex
                && s.version.major == 1
                && i.destination
                    .as_ref()
                    .is_some_and(|d| d.register.kind == RegisterKind::Address)
            {
                format!("sign({a})*floor(abs({a})+vec4<f32>(0.5))")
            } else {
                a.into()
            }
        }
        Add => value = format!("{a}+{b}"),
        Sub => value = format!("{a}-{b}"),
        Mad => value = format!("{p}_mul({a},{b})+{c}"),
        Mul => value = format!("{p}_mul({a},{b})"),
        Rcp => value = format!("vec4<f32>({p}_rcp(({a}).x))"),
        Rsq => value = format!("vec4<f32>({p}_rsq(({a}).x))"),
        Dp3 => value = format!("vec4<f32>({p}_dot3({a},{b}))"),
        Dp4 => value = format!("vec4<f32>({p}_dot4({a},{b}))"),
        Min => value = format!("min({a},{b})"),
        Max => value = format!("max({a},{b})"),
        Slt => value = format!("select(vec4<f32>(0.0),vec4<f32>(1.0),{p}_compare({a},{b},4u))"),
        Sge => value = format!("select(vec4<f32>(0.0),vec4<f32>(1.0),{p}_compare({a},{b},3u))"),
        Exp => value = format!("vec4<f32>(exp2(({a}).x))"),
        Log => value = format!("vec4<f32>({p}_log(({a}).x))"),
        Lit => value = format!("{p}_lit({a})"),
        Dst => value = format!("vec4<f32>(1.0,{p}_mul_scalar(({a}).y,({b}).y),({a}).z,({b}).w)"),
        Lrp => value = format!("{p}_mul({a},{b})+{p}_mul(vec4<f32>(1.0)-{a},{c})"),
        Frc => value = format!("fract({a})"),
        Pow => value = format!("vec4<f32>(pow(abs(({a}).x),({b}).x))"),
        Crs => {
            value = format!(
                "vec4<f32>(({p}_mul(({a}).yzxx,({b}).zxyy)-{p}_mul(({a}).zxyy,({b}).yzxx)).xyz,0.0)"
            )
        }
        Sgn => value = format!("sign({a})"),
        Abs => value = format!("abs({a})"),
        Nrm => value = format!("vec4<f32>(normalize(({a}).xyz),({a}).w)"),
        SinCos => value = format!("vec4<f32>(cos(({a}).x),sin(({a}).x),0.0,0.0)"),
        Mova => value = format!("sign({a})*floor(abs({a})+vec4<f32>(0.5))"),
        Cmp => value = format!("select({c},{b},{p}_compare({a},vec4<f32>(0.0),3u))"),
        Cnd => {
            value = if s.version.minor < 4 {
                format!("select({c},{b},{p}_cmp_scalar(({a}).w,0.5,1u))")
            } else {
                format!("select({c},{b},{p}_compare({a},vec4<f32>(0.5),1u))")
            }
        }
        Dp2add => value = format!("vec4<f32>({p}_dot2({a},{b})+({c}).x)"),
        Setp => {
            value = format!(
                "select(vec4<f32>(0.0),vec4<f32>(1.0),{p}_compare({a},{b},{}u))",
                i.controls
            )
        }
        Dsx => value = format!("dpdx({a})"),
        Dsy => value = format!("dpdy({a})"),
        M4x4 | M4x3 | M3x4 | M3x3 | M3x2 => {
            let n = match i.opcode {
                M4x4 | M3x4 => 4,
                M4x3 | M3x3 => 3,
                _ => 2,
            };
            let width = if matches!(i.opcode, M4x4 | M4x3) {
                4
            } else {
                3
            };
            let mut fields = Vec::new();
            for row in 0..n {
                let x = src(&i.sources[1], s, p, row);
                fields.push(format!("{p}_dot{width}({a},{x})"));
            }
            while fields.len() < 4 {
                fields.push("0.0".into());
            }
            value = format!("vec4<f32>({})", fields.join(","));
        }
        Expp => {
            value = if s.stage == Stage::Vertex && s.version.major == 1 {
                format!("vec4<f32>(exp2(floor(({a}).x)),fract(({a}).x),exp2(({a}).x),1.0)")
            } else {
                format!("vec4<f32>(exp2(({a}).x))")
            }
        }
        Logp => {
            value = if s.stage == Stage::Vertex && s.version.major == 1 {
                format!(
                    "vec4<f32>(floor({p}_log(({a}).x)),abs(({a}).x)/exp2(floor({p}_log(({a}).x)),{p}_log(({a}).x),1.0)"
                )
            } else {
                format!("vec4<f32>({p}_log(({a}).x))")
            }
        }
        Texcoord => {
            let d = i.destination.as_ref().expect("texcoord");
            value = if i.sources.is_empty() {
                format!(
                    "vec4<f32>(clamp(input.t[{}].xyz,vec3<f32>(0.0),vec3<f32>(1.0)),1.0)",
                    d.register.index
                )
            } else {
                a.into()
            };
        }
        Texkill => {
            let d = i.destination.as_ref().expect("texkill");
            writeln!(
                out,
                "if any(({}).xyz<vec3<f32>(0.0)) {{ discard; }}",
                raw(d.register, d.relative, s, 0)
            )
            .unwrap();
        }
        Tex | Texldl | Texldd => {
            let d = i.destination.as_ref().expect("tex");
            let index = if s.version.major == 1 {
                d.register.index
            } else {
                i.sources[1].register.index
            };
            let coord = if i.sources.is_empty() {
                format!("t[{}]", d.register.index)
            } else {
                a.into()
            };
            value = if i.opcode == Texldd {
                format!(
                    "{p}_sample_grad_{index}({coord},{},{})",
                    operands[2], operands[3]
                )
            } else {
                let mode = if i.opcode == Texldl {
                    3
                } else {
                    u32::from(i.controls)
                };
                format!("{p}_sample_{index}({coord},{mode}u)")
            };
        }
        Texreg2ar | Texreg2gb | Texreg2rgb | Texdp3tex => {
            let d = i.destination.as_ref().expect("texreg");
            let coord = match i.opcode {
                Texreg2ar => format!("vec4<f32>(({a}).w,({a}).x,0.0,1.0)"),
                Texreg2gb => format!("vec4<f32>(({a}).y,({a}).z,0.0,1.0)"),
                Texreg2rgb => format!("vec4<f32>(({a}).xyz,1.0)"),
                _ => format!(
                    "vec4<f32>(dot(t[{}].xyz,({a}).xyz),0.0,0.0,1.0)",
                    d.register.index
                ),
            };
            value = format!("{p}_sample_{}({coord},0u)", d.register.index);
        }
        Texdp3 => {
            let d = i.destination.as_ref().expect("texdp3");
            value = format!("vec4<f32>(dot(t[{}].xyz,({a}).xyz))", d.register.index);
        }
        Texm3x2pad | Texm3x3pad => {
            let d = i.destination.as_ref().expect("pad");
            writeln!(
                out,
                "matrix[matrix_n]=dot(input.t[{}].xyz,({a}).xyz); matrix_n=min(matrix_n+1u,2u);",
                d.register.index
            )
            .unwrap();
        }
        Texm3x2tex | Texm3x3tex | Texm3x3 | Texm3x3spec | Texm3x3vspec | Texm3x2depth => {
            let d = i.destination.as_ref().expect("matrix");
            let z = format!("dot(input.t[{}].xyz,({a}).xyz)", d.register.index);
            let coord = if matches!(i.opcode, Texm3x2tex | Texm3x2depth) {
                format!("vec4<f32>(matrix.x,{z},0.0,1.0)")
            } else {
                format!("vec4<f32>(matrix.xy,{z},1.0)")
            };
            if matches!(i.opcode, Texm3x3spec | Texm3x3vspec) {
                let eye = if i.opcode == Texm3x3spec {
                    b.into()
                } else {
                    if d.register.index < 2 {
                        return Err(Error::new(
                            i.offset,
                            "texm3x3vspec requires preceding stages",
                        ));
                    }
                    format!(
                        "vec4<f32>(input.t[{}].w,input.t[{}].w,input.t[{}].w,1.0)",
                        d.register.index - 2,
                        d.register.index - 1,
                        d.register.index
                    )
                };
                value = format!(
                    "{p}_sample_{}(vec4<f32>(2.0*dot(({coord}).xyz,({eye}).xyz)/dot(({coord}).xyz,({coord}).xyz)*({coord}).xyz-({eye}).xyz,1.0),0u)",
                    d.register.index
                );
            } else if i.opcode == Texm3x2depth {
                writeln!(out, "depth=clamp(matrix.x*{p}_rcp({z}),0.0,1.0);").unwrap();
            } else if i.opcode == Texm3x3 {
                value = coord;
            } else {
                value = format!("{p}_sample_{}({coord},0u)", d.register.index);
            }
            out.push_str("matrix_n=0u;\n");
        }
        Texdepth => {
            let d = i.destination.as_ref().expect("texdepth");
            writeln!(
                out,
                "depth=clamp(r[{}].x*{p}_rcp(r[{}].y),0.0,1.0);",
                d.register.index, d.register.index
            )
            .unwrap();
        }
        Texbem | Texbeml | Bem => {
            let d = i.destination.as_ref().expect("bump");
            let n = d.register.index;
            let matrix = format!("{p}_constants.bump_matrix[{n}]");
            let (base, perturb) = if i.opcode == Bem {
                (a.to_owned(), b.to_owned())
            } else {
                (format!("input.t[{n}]"), a.to_owned())
            };
            let coord = format!(
                "vec4<f32>(({base}).xy+vec2<f32>(dot(({matrix}).xz,({perturb}).xy),dot(({matrix}).yw,({perturb}).xy)),({base}).zw)"
            );
            if i.opcode == Bem {
                value = coord;
            } else {
                value = format!("{p}_sample_{n}({coord},0u)");
                if i.opcode == Texbeml {
                    value = format!(
                        "({value}*(({perturb}).z*{p}_constants.bump_luminance[{n}].x+{p}_constants.bump_luminance[{n}].y))"
                    );
                }
            }
        }
    }
    if !value.is_empty() {
        emit_write(
            out,
            i.destination
                .as_ref()
                .ok_or_else(|| Error::new(i.offset, "missing destination"))?,
            &value,
            s,
        );
    }
    Ok(())
}
