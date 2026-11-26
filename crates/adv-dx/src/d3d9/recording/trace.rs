//! D3D9 command details for the frame trace.
use super::*;
use crate::record::Contents;
pub(super) use crate::record::trace::capturing;
pub use crate::record::trace::take_or_arm;
use crate::record::trace::{name, push, rect};
use std::sync::Mutex;

/// Why the command being prepared has no proven sample mapping.
static UNMAPPED: Mutex<Option<&'static str>> = Mutex::new(None);
static DETAIL: Mutex<Option<String>> = Mutex::new(None);

pub(super) fn detail(text: String) {
    *DETAIL.lock().unwrap() = Some(text);
}
/// A shader program, one instruction per line.
pub(super) fn program(shader: &crate::shader::Shader) -> String {
    use crate::shader::{Register, Source};
    let reg = |r: Register| format!("{:?}{}", r.kind, r.index);
    let source = |s: &Source| {
        let swizzle: String = s
            .swizzle
            .iter()
            .map(|&c| b"xyzw"[usize::from(c & 3)] as char)
            .collect();
        format!(
            "{}{}.{swizzle}{}",
            if s.modifier == crate::shader::SourceModifier::None {
                String::new()
            } else {
                format!("{:?} ", s.modifier)
            },
            reg(s.register),
            if s.relative.is_some() { "[rel]" } else { "" }
        )
    };
    let mut lines = vec![format!(
        "ps/vs {}.{}",
        shader.version.major, shader.version.minor
    )];
    for i in &shader.instructions {
        let destination = i.destination.as_ref().map_or(String::new(), |d| {
            let mask: String = (0..4)
                .filter(|c| d.mask & (1 << c) != 0)
                .map(|c| b"xyzw"[c] as char)
                .collect();
            format!(
                " {}.{mask}{}{}",
                reg(d.register),
                if d.saturate { "_sat" } else { "" },
                if d.shift != 0 {
                    format!(" shift {}", d.shift)
                } else {
                    String::new()
                }
            )
        });
        let sources: Vec<_> = i.sources.iter().map(source).collect();
        let immediate = if i.immediate.is_empty() {
            String::new()
        } else {
            format!(
                " = {:?}",
                i.immediate
                    .iter()
                    .map(|v| f32::from_bits(*v))
                    .collect::<Vec<_>>()
            )
        };
        lines.push(format!(
            "      {:?}{destination} {}{immediate}",
            i.opcode,
            sources.join(", ")
        ));
    }
    lines.join("\n")
}
pub(super) fn unmapped(why: &'static str) {
    if capturing() {
        *UNMAPPED.lock().unwrap() = Some(why);
    }
}
pub(super) fn command(index: usize, p: &key::Prepared, reused: bool, contents: &Contents) {
    if !capturing() {
        return;
    }
    let why = UNMAPPED.lock().unwrap().take();
    let detail = DETAIL.lock().unwrap().take();
    let image = damage::image(&p.operation);
    let kind = match &p.operation {
        Operation::Clear {
            color,
            depth,
            stencil,
            rectangle,
            ..
        } => format!(
            "clear{}{}{} {}",
            if color.is_some() { " color" } else { "" },
            if depth.is_some() { " depth" } else { "" },
            if stencil.is_some() { " stencil" } else { "" },
            rect(*rectangle)
        ),
        Operation::Draw(d) => format!(
            "draw{} depth {:?} alpha {:?}",
            if d.programmable.is_some() {
                " shader"
            } else {
                " fixed"
            },
            d.state.depth,
            d.state.alpha_test,
        ),
        Operation::Copy {
            rectangle, region, ..
        } => {
            format!("copy {} -> {}", rect(*rectangle), rect(*region))
        }
        Operation::Upload { rectangle, .. } => format!("upload {}", rect(*rectangle)),
        _ => "other".into(),
    };
    let writes: Vec<_> = p
        .writes
        .iter()
        .map(|(k, overwrite)| {
            format!(
                "{}{}",
                name(k),
                if *overwrite { " (overwrite)" } else { "" }
            )
        })
        .collect();
    // A proven read's mapping shows its texel phase, such as a pan's.
    let reads: Vec<_> = p
        .reads
        .iter()
        .map(|k| {
            let mappings: Vec<_> = p
                .samples
                .iter()
                .filter(|r| r.source == *k)
                .map(|r| format!(" {:?}", r.mapping))
                .collect();
            format!(
                "{}@{:032x}{}",
                name(k),
                contents.get(k).copied().unwrap_or(0),
                mappings.concat()
            )
        })
        .collect();
    push(format!(
        "#{index} {kind} on {} bounds {}{}: key {:032x} writes [{}] reads [{}]{}",
        image.map_or("-".into(), |i| format!(
            "{}x{} image {:x}",
            i.width(),
            i.height(),
            hash(i.id()) as u16
        )),
        rect(p.bounds),
        if reused { " reused" } else { "" },
        p.key,
        writes.join(", "),
        reads.join(", "),
        why.map_or(String::new(), |why| format!("; unmapped: {why}")),
    ));
    if let Some(detail) = detail {
        push(detail);
    }
}
pub(in crate::d3d9) fn readback(env: &Env<'_>, id: Id) {
    if capturing() {
        push(format!(
            "GetRenderTargetData of {}",
            name(&key::content(env, id, None, 0))
        ));
    }
}
pub(super) fn flush(reason: Reason, commands: usize) {
    if capturing() {
        push(format!("flush {reason:?} after {commands} commands"));
    }
}
