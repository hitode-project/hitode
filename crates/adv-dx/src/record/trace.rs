//! One frame's recorded commands as text, captured on request for the live
//! debugger. Idle unless armed: each hook is one relaxed load.
use super::{HashMap, ResourceKey as Content, damage};
use adv_device::surface::{Rect, Region};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

/// Idle, waiting for a frame boundary, or capturing until the next one.
static STATE: AtomicU8 = AtomicU8::new(IDLE);
const IDLE: u8 = 0;
const ARMED: u8 = 1;
const ARMED_PAIR: u8 = 2;
const CAPTURING: u8 = 3;
const CAPTURING_PAIR: u8 = 4;
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LAST: Mutex<Option<String>> = Mutex::new(None);

/// The last captured frame, or arms a capture of the next whole frame.
pub fn take_or_arm(pair: bool) -> Option<String> {
    let last = LAST.lock().unwrap().take();
    if last.is_none() {
        let armed = if pair { ARMED_PAIR } else { ARMED };
        let _ = STATE.compare_exchange(IDLE, armed, Ordering::Relaxed, Ordering::Relaxed);
    }
    last
}
pub(crate) fn capturing() -> bool {
    STATE.load(Ordering::Relaxed) >= CAPTURING
}
pub(crate) fn push(line: String) {
    LINES.lock().unwrap().push(line);
}
pub(crate) fn name(c: &Content) -> String {
    let aspect = ["color", "depth", "stencil"]
        .get(c.2 as usize)
        .copied()
        .unwrap_or("?");
    format!("{:x}.{}.{aspect}", c.0[1], c.1)
}
pub(crate) fn rect(r: Rect) -> String {
    format!("{}x{}+{}+{}", r.w, r.h, r.x, r.y)
}
pub(crate) fn forward(content: Content, generation: usize, event: std::fmt::Arguments<'_>) {
    push(format!(
        "forward {} generation {generation}: {event}",
        name(&content)
    ));
}
pub(crate) fn present(
    partial: bool,
    force: bool,
    copy: bool,
    promotions: [u64; 2],
    execution: &HashMap<u128, Region>,
) {
    if capturing() {
        let mut slots: Vec<_> = execution
            .iter()
            .map(|(slot, r)| {
                let area = r.area();
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
pub(crate) fn work(work: &HashMap<(Content, usize, u128), damage::Work>, planned: bool) {
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
pub(crate) fn frame_end() {
    match STATE.load(Ordering::Relaxed) {
        state @ (ARMED | ARMED_PAIR) => {
            LINES.lock().unwrap().clear();
            let capturing = if state == ARMED_PAIR {
                CAPTURING_PAIR
            } else {
                CAPTURING
            };
            STATE.store(capturing, Ordering::Relaxed);
        }
        CAPTURING_PAIR => {
            push("--- next frame ---".into());
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
