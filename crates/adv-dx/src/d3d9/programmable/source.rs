use super::*;
use std::fmt::Write;

const TYPES: &str = r#"
diagnostic(off, derivative_uniformity);
struct ShaderInputs { v:array<vec4<f32>,16>, t:array<vec4<f32>,8>, position:vec4<f32>, front_face:bool }
struct ShaderOutputs { position:vec4<f32>, colors:array<vec4<f32>,2>, texcoords:array<vec4<f32>,8>, depth:f32, point_size:f32, fog:f32, generic:array<vec4<f32>,12> }
struct ShaderConstants { f:array<vec4<f32>,256>, i:array<vec4<i32>,16>, b:array<vec4<i32>,16>, bump_matrix:array<vec4<f32>,8>, bump_luminance:array<vec4<f32>,8> }
struct DrawParameters { values:array<vec4<u32>,40> }
@group(0) @binding(0) var<uniform> vs_constants:ShaderConstants;
@group(0) @binding(1) var<uniform> ps_constants:ShaderConstants;
@group(0) @binding(2) var<uniform> draw_parameters:DrawParameters;
fn adv_combine(op:u32,c:vec4<f32>,t:vec4<f32>)->vec4<f32> {
  switch op {case 0u:{return c;}case 1u:{return t;}case 2u:{return c*t;}case 3u:{return clamp(c+t,vec4<f32>(0.0),vec4<f32>(1.0));}default:{return clamp(c-t,vec4<f32>(0.0),vec4<f32>(1.0));}}
}
fn adv_argument(a:u32,diffuse:vec4<f32>,current:vec4<f32>,texel:vec4<f32>)->vec4<f32> {
  switch a {case 0u:{return diffuse;}case 1u:{return current;}default:{return texel;}}
}
fn adv_stage(op:u32,a1:vec4<f32>,a2:vec4<f32>,current:vec4<f32>)->vec4<f32> {
  switch op {case 2u:{return a1;}case 3u:{return a2;}case 4u:{return a1*a2;}case 7u:{return clamp(a1+a2,vec4<f32>(0.0),vec4<f32>(1.0));}case 10u:{return clamp(a1-a2,vec4<f32>(0.0),vec4<f32>(1.0));}default:{return current;}}
}
fn adv_alpha(a:f32)->bool {
 let r=bitcast<f32>(draw_parameters.values[1].w);
 switch draw_parameters.values[1].z {case 0u:{return false;}case 1u:{return a<r;}case 2u:{return a==r;}case 3u:{return a<=r;}case 4u:{return a>r;}case 5u:{return a!=r;}case 6u:{return a>=r;}default:{return true;}}
}
struct Interpolated {
 @builtin(position) position:vec4<f32>,
 @location(0) c0:vec4<f32>, @location(1) c1:vec4<f32>,
 @location(2) t0:vec4<f32>, @location(3) t1:vec4<f32>,
 @location(4) t2:vec4<f32>, @location(5) t3:vec4<f32>,
 @location(6) t4:vec4<f32>, @location(7) t5:vec4<f32>,
 @location(8) t6:vec4<f32>, @location(9) t7:vec4<f32>,
 @location(10) clip0:vec4<f32>, @location(11) clip1:vec2<f32>, @location(12) fog:f32,
}
"#;
pub(super) fn build(
    vs: Option<&Shader>,
    ps: Option<&Shader>,
    vt: Option<&Translation>,
    pt: Option<&Translation>,
    inputs: &[Input],
    slots: &[(bool, u8)],
    features: (bool, u32, bool),
) -> Result<String, u32> {
    let (second, clip, fog) = features;
    let used = varyings(ps, second);
    let mut s = TYPES.replace("struct Interpolated", "struct Varyings");
    for n in 0..13 {
        s = s.replace(&format!("@location({n}) "), "");
    }
    s = s.replace("@builtin(position) ", "");
    s.push_str("struct Interpolated { @builtin(position) position:vec4<f32>,\n");
    for (n, name) in fields().iter().enumerate() {
        if used[n] {
            writeln!(s, "@location({n}) {name}:vec4<f32>,").unwrap();
        }
    }
    if clip & 15 != 0 {
        s.push_str("@location(10) clip0:vec4<f32>,\n");
    }
    if clip & 48 != 0 {
        s.push_str("@location(11) clip1:vec2<f32>,\n");
    }
    if fog {
        s.push_str("@location(12) fog:f32,\n");
    }
    s.push_str("}\n");
    s.push_str(adv_device::gpu::BORDER_WGSL);
    for (ordinal, &(vertex, index)) in slots.iter().enumerate() {
        let p = if vertex { "vs" } else { "ps" };
        writeln!(s,"@group(1) @binding({}) var {p}_image_{index}:texture_2d<f32>;\n@group(1) @binding({}) var {p}_sampler_{index}:sampler;",ordinal*2,ordinal*2+1).unwrap();
        writeln!(s,"fn {p}_alpha_{index}(value:vec4<f32>)->vec4<f32> {{if (draw_parameters.values[14].x&{}u)!=0u {{return vec4<f32>(value.rgb,1.0);}}return value;}}",1u32<<ordinal).unwrap();
        writeln!(
            s,
            "fn {p}_sample_{index}(coord:vec4<f32>,mode:u32)->vec4<f32> {{"
        )
        .unwrap();
        s.push_str("var uv=coord.xy;if mode==1u {uv=uv/coord.w;}\n");
        writeln!(s, "let config=draw_parameters.values[{}];", 16 + ordinal).unwrap();
        if vertex {
            writeln!(s,"if (config.x&3u)!=0u {{return {p}_alpha_{index}(adv_border_sample({p}_image_{index},uv,coord.w,config.x,config.y,config.z,config.w));}}return {p}_alpha_{index}(textureSampleLevel({p}_image_{index},{p}_sampler_{index},uv,coord.w));}}").unwrap();
        } else {
            writeln!(s,"if (config.x&3u)!=0u {{let size=vec2<f32>(textureDimensions({p}_image_{index},0));let dx=dpdx(uv)*size;let dy=dpdy(uv)*size;var lod=0.5*log2(max(max(dot(dx,dx),dot(dy,dy)),1e-30));if mode==3u {{lod=coord.w;}}else if mode==2u {{lod+=coord.w;}}return {p}_alpha_{index}(adv_border_sample({p}_image_{index},uv,lod,config.x,config.y,config.z,config.w));}}").unwrap();
            writeln!(s,"if mode==3u {{return {p}_alpha_{index}(textureSampleLevel({p}_image_{index},{p}_sampler_{index},uv,coord.w));}}\nif mode==2u {{return {p}_alpha_{index}(textureSampleBias({p}_image_{index},{p}_sampler_{index},uv,coord.w));}}\nreturn {p}_alpha_{index}(textureSample({p}_image_{index},{p}_sampler_{index},uv));}}").unwrap();
            writeln!(s,"fn {p}_sample_grad_{index}(coord:vec4<f32>,dx:vec4<f32>,dy:vec4<f32>)->vec4<f32>{{let config=draw_parameters.values[{}];if (config.x&3u)!=0u {{let size=vec2<f32>(textureDimensions({p}_image_{index},0));let gx=dx.xy*size;let gy=dy.xy*size;let lod=0.5*log2(max(max(dot(gx,gx),dot(gy,gy)),1e-30));return {p}_alpha_{index}(adv_border_sample({p}_image_{index},coord.xy,lod,config.x,config.y,config.z,config.w));}}return {p}_alpha_{index}(textureSampleGrad({p}_image_{index},{p}_sampler_{index},coord.xy,dx.xy,dy.xy));}}",16+ordinal).unwrap();
        }
    }
    if let Some(t) = vt {
        s.push_str(&t.source);
    }
    if let Some(t) = pt {
        s.push_str(&t.source);
    }
    s.push_str("@vertex fn vertex_main(@builtin(vertex_index) ordinal:u32,");
    let transformed = vs.is_none() && inputs.iter().any(|i| i.register == 0);
    if vs.is_some() || transformed {
        for (n, i) in inputs.iter().enumerate() {
            if n > 0 {
                s.push(',');
            }
            write!(
                s,
                "@location({}) a{}:{}",
                i.register,
                i.register,
                i.format.wgsl_type()
            )
            .unwrap();
        }
    } else {
        s.push_str("@location(0) a0:vec4<f32>,@location(1) a1:vec4<f32>");
        if inputs.is_empty() {
            s.push_str(",@location(2) a2:vec2<f32>");
        } else {
            for input in inputs {
                write!(
                    s,
                    ",@location({}) a{}:vec4<f32>",
                    input.register, input.register
                )
                .unwrap();
            }
        }
    }
    s.push_str(")->Interpolated {\nvar out:Varyings;\n");
    if let Some(vs) = vs {
        s.push_str("var input:ShaderInputs;\n");
        for i in inputs {
            writeln!(
                s,
                "input.v[{}]={};",
                i.register,
                i.format.unpack(&format!("a{}", i.register))
            )
            .unwrap();
        }
        s.push_str("let result=vs_run(input);out.position=result.position;out.fog=result.fog;let clip_position=out.position;\n");
        // D3D9 integer pixel centres map to the host's half-integer centres.
        s.push_str("out.position.x+=out.position.w/bitcast<f32>(draw_parameters.values[0].x);\nout.position.y-=out.position.w/bitcast<f32>(draw_parameters.values[0].y);\nout.c0=result.colors[0];out.c1=result.colors[1];\n");
        for n in 0..8 {
            writeln!(s, "out.t{n}=result.texcoords[{n}];").unwrap();
        }
        if vs.version.major == 3
            && let Some(ps) = ps.filter(|p| p.version.major == 3)
        {
            for input in ps
                .inputs
                .iter()
                .filter(|d| d.register.kind == crate::shader::RegisterKind::Input)
            {
                let output = vs
                    .outputs
                    .iter()
                    .find(|o| o.usage == input.usage && o.index == input.index)
                    .ok_or(INVALID)?;
                let varying = match input.register.index {
                    0 => "c0".into(),
                    1 => "c1".into(),
                    n @ 2..=9 => format!("t{}", n - 2),
                    _ => return Err(UNAVAILABLE),
                };
                writeln!(
                    s,
                    "out.{varying}=result.generic[{}];",
                    output.register.index
                )
                .unwrap();
            }
        }
    } else if transformed {
        s.push_str("let viewport=bitcast<vec4<f32>>(draw_parameters.values[0]);let w=1.0/a0.w;out.position=vec4<f32>((((a0.x+0.5-viewport.z)/viewport.x)*2.0-1.0)*w,(1.0-((a0.y+0.5-viewport.w)/viewport.y)*2.0)*w,a0.z*w,w);let clip_position=out.position;out.fog=1.0;\n");
        let color = inputs
            .iter()
            .find(|i| i.register == 1)
            .map_or_else(|| "vec4<f32>(1.0)".into(), |i| i.format.unpack("a1"));
        writeln!(s, "out.c0={color};").unwrap();
        for n in 0..8 {
            let register = 2 + n;
            let value = inputs.iter().find(|i| i.register == register).map_or_else(
                || "vec4<f32>(0.0,0.0,0.0,1.0)".into(),
                |i| i.format.unpack(&format!("a{register}")),
            );
            writeln!(s, "out.t{n}={value};").unwrap();
        }
    } else {
        s.push_str("out.position=a0;let clip_position=out.position;out.c0=a1;out.fog=1.0;\n");
        if inputs.is_empty() {
            s.push_str("out.t0=vec4<f32>(a2,0.0,1.0);\n");
        } else {
            for input in inputs {
                writeln!(s, "out.t{}=a{};", input.register - 2, input.register).unwrap();
            }
        }
    }
    for n in (0..6).filter(|n| clip & (1 << n) != 0) {
        let field = if n < 4 {
            format!("clip0.{}", ["x", "y", "z", "w"][n])
        } else {
            format!("clip1.{}", ["x", "y"][n - 4])
        };
        writeln!(
            s,
            "out.{field}=dot(clip_position,bitcast<vec4<f32>>(draw_parameters.values[{}]));",
            4 + n
        )
        .unwrap();
    }
    s.push_str("if draw_parameters.values[2].w!=0u {\nlet corners=array<vec2<f32>,6>(vec2<f32>(-1.0,1.0),vec2<f32>(-1.0,-1.0),vec2<f32>(1.0,1.0),vec2<f32>(1.0,1.0),vec2<f32>(-1.0,-1.0),vec2<f32>(1.0,-1.0));let corner=corners[ordinal%6u];\n");
    if vt.is_some_and(|t| t.writes_point_size) {
        s.push_str("var size=result.point_size;\n");
    } else {
        s.push_str("var size=bitcast<f32>(draw_parameters.values[11].w);\n");
    }
    s.push_str("size=clamp(size,bitcast<f32>(draw_parameters.values[12].x),bitcast<f32>(draw_parameters.values[12].y));out.position.x+=corner.x*size*out.position.w/bitcast<f32>(draw_parameters.values[0].x);out.position.y+=corner.y*size*out.position.w/bitcast<f32>(draw_parameters.values[0].y);if draw_parameters.values[12].z!=0u {out.t0=vec4<f32>((corner.x+1.0)*0.5,(1.0-corner.y)*0.5,0.0,1.0);} }\n");
    s.push_str("return Interpolated(out.position");
    for (n, field) in fields().iter().enumerate() {
        if used[n] {
            write!(s, ",out.{field}").unwrap();
        }
    }
    if clip & 15 != 0 {
        s.push_str(",out.clip0");
    }
    if clip & 48 != 0 {
        s.push_str(",out.clip1");
    }
    if fog {
        s.push_str(",out.fog");
    }
    s.push_str(");}\n");
    let depth = pt.is_some_and(|p| p.writes_depth);
    if depth {
        s.push_str(
            "struct FragmentOutput {@location(0) color:vec4<f32>,@builtin(frag_depth) depth:f32}\n",
        );
    }
    writeln!(
        s,
        "@fragment fn fragment_main(input:Interpolated,@builtin(front_facing) front:bool)->{} {{",
        if depth {
            "FragmentOutput"
        } else {
            "@location(0) vec4<f32>"
        }
    )
    .unwrap();
    s.push_str("let enabled=draw_parameters.values[3].x;\n");
    for n in (0..6).filter(|n| clip & (1 << n) != 0) {
        let field = if n < 4 {
            format!("clip0.{}", ["x", "y", "z", "w"][n])
        } else {
            format!("clip1.{}", ["x", "y"][n - 4])
        };
        writeln!(
            s,
            "if (enabled&{}u)!=0u && input.{field}<0.0 {{discard;}}",
            1 << n
        )
        .unwrap();
    }
    if let Some(ps) = ps {
        s.push_str("var registers:ShaderInputs;registers.position=input.position;registers.position.x-=0.5;registers.position.y-=0.5;registers.front_face=!front;\n");
        let (colors, coordinates) = used.split_at(2);
        for n in (0..2).filter(|&n| colors[n]) {
            writeln!(s, "registers.v[{n}]=input.c{n};").unwrap();
        }
        for n in (0..8).filter(|&n| coordinates[n]) {
            writeln!(s, "registers.t[{n}]=input.t{n};").unwrap();
        }
        if ps.version.major >= 3 {
            for i in ps
                .inputs
                .iter()
                .filter(|i| i.register.kind == crate::shader::RegisterKind::Input)
            {
                let source = if vs.is_some_and(|v| v.version.major == 3) {
                    match i.register.index {
                        0 => "input.c0".into(),
                        1 => "input.c1".into(),
                        n @ 2..=9 => format!("input.t{}", n - 2),
                        _ => return Err(UNAVAILABLE),
                    }
                } else {
                    match (i.usage, i.index) {
                        (10, 0) => "input.c0".into(),
                        (10, 1) => "input.c1".into(),
                        (5, n @ 0..=7) => format!("input.t{n}"),
                        _ => return Err(UNAVAILABLE),
                    }
                };
                if fields()
                    .iter()
                    .position(|name| source == format!("input.{name}"))
                    .is_some_and(|n| used[n])
                {
                    writeln!(s, "registers.v[{}]={source};", i.register.index).unwrap();
                }
            }
        }
        s.push_str("let result=ps_run(registers);var color=result.colors[0];\n");
    } else {
        s.push_str("var texel=ps_sample_0(input.t0,0u);if draw_parameters.values[2].x!=0u {texel.a=1.0;}\nlet rgb=adv_combine(draw_parameters.values[1].x,input.c0,texel).rgb;let alpha=adv_combine(draw_parameters.values[1].y,input.c0,texel).a;var color=vec4<f32>(rgb,alpha);\n");
        if second {
            // Stage one: operation, arguments and coordinate set as D3D values.
            s.push_str("{let stage=draw_parameters.values[13];var coords=array<vec4<f32>,8>(input.t0,input.t1,input.t2,input.t3,input.t4,input.t5,input.t6,input.t7);let texel=ps_sample_1(coords[stage.w&7u],0u);\nlet rgb=adv_stage(stage.x,adv_argument(stage.y&255u,input.c0,color,texel),adv_argument(stage.y>>8u,input.c0,color,texel),color).rgb;\nlet alpha=adv_stage(stage.z&255u,adv_argument((stage.z>>8u)&255u,input.c0,color,texel),adv_argument(stage.z>>16u,input.c0,color,texel),color).a;color=vec4<f32>(rgb,alpha);}\n");
        }
    }
    s.push_str("if !adv_alpha(color.a) {discard;}\n");
    if fog && ps.is_none_or(|p| p.version.major < 3) {
        s.push_str("if draw_parameters.values[3].y!=0u {var factor=input.fog;let mode=draw_parameters.values[3].z;let values=bitcast<vec4<f32>>(draw_parameters.values[11]);let distance=input.position.z;if mode==1u {factor=exp(-values.z*distance);}else if mode==2u {factor=exp(-pow(values.z*distance,2.0));}else if mode==3u {factor=(values.y-distance)/(values.y-values.x);}color=vec4<f32>(mix(bitcast<vec4<f32>>(draw_parameters.values[10]).rgb,color.rgb,clamp(factor,0.0,1.0)),color.a);}\n");
    }
    if depth {
        s.push_str("return FragmentOutput(color,clamp(result.depth,0.0,1.0));}\n");
    } else {
        s.push_str("return color;}\n");
    }
    Ok(s)
}

fn fields() -> [&'static str; 10] {
    ["c0", "c1", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7"]
}
fn varyings(ps: Option<&Shader>, second: bool) -> [bool; 10] {
    use crate::shader::RegisterKind;
    let mut used = [false; 10];
    let Some(ps) = ps else {
        used[0] = true;
        used[2] = true;
        if second {
            used[2..].fill(true);
        }
        return used;
    };
    for instruction in &ps.instructions {
        for source in instruction
            .sources
            .iter()
            .chain(instruction.predicate.iter())
        {
            if source.register.kind == RegisterKind::Input {
                if source.relative.is_some() {
                    used.fill(true);
                } else if ps.version.major < 3 || source.register.index < 10 {
                    let n = source.register.index as usize;
                    if n < used.len() {
                        used[n] = true;
                    }
                }
            }
            if source.register.kind == RegisterKind::Address && source.register.index < 8 {
                used[2 + source.register.index as usize] = true;
            }
        }
    }
    // Legacy texture instructions include implicit coordinate dependencies.
    if ps.version.major == 1 {
        used[2..].fill(true);
    }
    if ps.version.major >= 3 {
        for input in ps
            .inputs
            .iter()
            .filter(|d| d.register.kind == RegisterKind::Input)
        {
            let read = used
                .get(input.register.index as usize)
                .copied()
                .unwrap_or(false);
            if read {
                match (input.usage, input.index) {
                    (10, n @ 0..=1) => used[n as usize] = true,
                    (5, n @ 0..=7) => used[2 + n as usize] = true,
                    _ => {}
                }
            }
        }
    }
    used
}
