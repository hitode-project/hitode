//! One frame's recorded commands as text, captured on request for the live
//! debugger. Idle unless armed: each hook is one relaxed load.
use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

/// Idle, waiting for a frame boundary, or capturing until the next one.
static STATE: AtomicU8 = AtomicU8::new(IDLE);
const IDLE: u8 = 0;
const ARMED: u8 = 1;
const CAPTURING: u8 = 2;
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LAST: Mutex<Option<String>> = Mutex::new(None);
/// Why the command being prepared has no proven sample mapping.
static UNMAPPED: Mutex<Option<&'static str>> = Mutex::new(None);
static DETAIL: Mutex<Option<String>> = Mutex::new(None);

/// The last captured frame, or arms a capture of the next whole frame.
pub fn take_or_arm() -> Option<String> {
    let last = LAST.lock().unwrap().take();
    if last.is_none() {
        let _ = STATE.compare_exchange(IDLE, ARMED, Ordering::Relaxed, Ordering::Relaxed);
    }
    last
}
pub(super) fn capturing() -> bool {
    STATE.load(Ordering::Relaxed) == CAPTURING
}
fn push(line: String) {
    LINES.lock().unwrap().push(line);
}
fn name(c: &Content) -> String {
    let aspect = ["color", "depth", "stencil"]
        .get(c.2 as usize)
        .copied()
        .unwrap_or("?");
    format!("{:x}.{}.{aspect}", c.3, c.1)
}
fn rect(r: adv_device::surface::Rect) -> String {
    format!("{}x{}+{}+{}", r.w, r.h, r.x, r.y)
}
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
pub(super) fn command(index: usize, p: &key::Prepared, reused: bool) {
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
            "draw{}",
            if d.programmable.is_some() {
                " shader"
            } else {
                " fixed"
            }
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
            format!("{}{}", name(k), mappings.concat())
        })
        .collect();
    push(format!(
        "#{index} {kind} on {} bounds {}{}: writes [{}] reads [{}]{}",
        image.map_or("-".into(), |i| format!(
            "{}x{} image {:x}",
            i.width(),
            i.height(),
            hash(i.id()) as u16
        )),
        rect(p.bounds),
        if reused { " reused" } else { "" },
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
pub(super) fn present(
    partial: bool,
    force: bool,
    copy: bool,
    promotions: [u64; 2],
    execution: &rustc_hash::FxHashMap<u128, adv_device::surface::Region>,
) {
    if capturing() {
        let mut slots: Vec<_> = execution
            .iter()
            .map(|(slot, r)| {
                let area: u64 = adv_device::surface::rects(r)
                    .iter()
                    .map(|r| u64::from(r.w) * u64::from(r.h))
                    .sum();
                let (w, h) = r.size();
                format!(
                    "{:04x} {area}/{} px",
                    *slot as u16,
                    u64::from(w) * u64::from(h)
                )
            })
            .collect();
        slots.sort();
        push(format!(
            "present: partial {partial}, force {force}, copy {copy}, promotions intermediate {} late {}; planned [{}]",
            promotions[0],
            promotions[1],
            slots.join(", ")
        ));
    }
}
pub(super) fn work(work: &HashMap<(Content, usize, u128), damage::Work>, planned: bool) {
    if !capturing() {
        return;
    }
    if !planned {
        push("present: repeated; E 0 px, submissions 0, vertices 0".into());
        return;
    }
    let mut rows: Vec<_> = work.iter().map(|((content, generation, slot), work)| {
        let [delta, executed, extra] = work.areas();
        format!(
            "slot {:04x} {} generation {generation}: delta {delta} px, E {executed} px, E\\delta {extra} px; submissions {}, vertices {}, repeated vertices {}",
            *slot as u16, name(content), work.submissions, work.vertices, work.repeated_vertices
        )
    }).collect();
    rows.sort();
    for row in rows {
        push(row);
    }
}
pub(super) fn frame_end() {
    match STATE.load(Ordering::Relaxed) {
        ARMED => {
            LINES.lock().unwrap().clear();
            STATE.store(CAPTURING, Ordering::Relaxed);
        }
        CAPTURING => {
            let lines = std::mem::take(&mut *LINES.lock().unwrap());
            *LAST.lock().unwrap() = Some(lines.join("\n"));
            STATE.store(IDLE, Ordering::Relaxed);
        }
        _ => {}
    }
}
