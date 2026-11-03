//! Pass formation on the GPU owner thread: which operations share a render
//! pass, the pass's render area, and every attachment's load and store.
//!
//! Tile GPUs pay for each pass the attachment bytes they load into tile memory
//! and store back, inside the render area. So the area is exactly what the
//! operations may change, and an attachment is loaded only when the pass reads
//! what was there and stored only when a later use may read what it wrote.
//! The owner thread closes a pass when the next command targets other
//! attachments; by then it knows how the pass starts and how the depth buffer
//! is used next, which a pass encoded as soon as it opens cannot know.
//!
//! Pure functions over [`Op`] summaries: no GPU types, tested directly.

use adv_gpu::Rect;
use adv_gpu::encode::{Load, Store};

/// What one command does to the open attachments, inside `area`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Op {
    pub area: Rect,
    pub kind: OpKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OpKind {
    /// Each present component is cleared over the whole area.
    Clear {
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    },
    Draw {
        writes_color: bool,
        /// Depth or stencil tested or written.
        uses_depth: bool,
        writes_depth: bool,
        /// A rectangle every color channel of which the draw replaces without
        /// reading it (no blending, no discard, full write mask), if known.
        opaque: Option<Rect>,
    },
}

/// The next use of the depth attachment after a run of passes, if the owner
/// has seen it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NextDepth {
    /// Read by a later command, or not yet known: keep the contents.
    Keep,
    /// Next cleared over at least this rectangle, before any read.
    Cleared(Rect),
    /// Undefined after this point (D3D9 `DISCARD_DEPTHSTENCIL` at present).
    Discarded,
}

/// One render pass: `ops[range]` on the open attachments.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pass {
    pub ops: std::ops::Range<usize>,
    pub area: Rect,
    pub color: (Load<[f64; 4]>, Store),
    pub depth: (Load<f32>, Store),
    pub stencil: (Load<u32>, Store),
}

/// Split a run of operations on the same attachments into passes and choose
/// their areas, loads and stores. `has_depth` says whether a depth-stencil
/// attachment is bound; `next` is its next use after the run.
///
/// A clear opens a new pass, and becomes the pass's load when it covers the
/// pass area; a clear smaller than the operations after it stays a pass of
/// its own, because loads act on the whole render area.
#[cfg(test)]
pub(crate) fn plan(ops: &[Op], has_depth: bool, next: NextDepth) -> Vec<Pass> {
    let mut passes = Vec::new();
    plan_into(ops, has_depth, next, &mut passes);
    passes
}

/// Reuse the owner's planning storage across runs.
pub(super) fn plan_into(ops: &[Op], has_depth: bool, next: NextDepth, passes: &mut Vec<Pass>) {
    passes.clear();
    let mut start = 0;
    while start < ops.len() {
        // A pass runs to the next clear; a leading clear is kept alone when
        // the draws after it reach beyond its area.
        let mut end = start + 1;
        while end < ops.len() && !matches!(ops[end].kind, OpKind::Clear { .. }) {
            end += 1;
        }
        if matches!(ops[start].kind, OpKind::Clear { .. })
            && end > start + 1
            && !contains(ops[start].area, union(&ops[start + 1..end]))
        {
            end = start + 1;
        }
        passes.push(pass(ops, start..end));
        start = end;
    }
    if has_depth {
        settle_depth(ops, passes, next);
    } else {
        for pass in passes {
            pass.depth = (Load::None, Store::None);
            pass.stencil = (Load::None, Store::None);
        }
    }
}

fn pass(ops: &[Op], range: std::ops::Range<usize>) -> Pass {
    let run = &ops[range.clone()];
    let area = union(run);
    let first = run[0];
    let mut color = (Load::Load, Store::Store);
    let mut depth = (Load::Load, Store::Store);
    let mut stencil = (Load::Load, Store::Store);
    match first.kind {
        OpKind::Clear {
            color: c,
            depth: d,
            stencil: s,
        } => {
            if let Some(c) = c {
                color.0 = Load::Clear(c);
            }
            if let Some(d) = d {
                depth.0 = Load::Clear(d);
            }
            if let Some(s) = s {
                stencil.0 = Load::Clear(s);
            }
        }
        OpKind::Draw {
            opaque: Some(opaque),
            ..
        } if contains(opaque, area) => color.0 = Load::DontCare,
        OpKind::Draw { .. } => {}
    }
    let any = |pick: fn(&OpKind) -> bool| run.iter().any(|op| pick(&op.kind));
    let writes_color = any(|k| match k {
        OpKind::Clear { color, .. } => color.is_some(),
        OpKind::Draw { writes_color, .. } => *writes_color,
    });
    if !writes_color && color.0 == Load::Load {
        color = (Load::None, Store::None);
    }
    let uses_depth = any(|k| match k {
        OpKind::Clear { depth, stencil, .. } => depth.is_some() || stencil.is_some(),
        OpKind::Draw { uses_depth, .. } => *uses_depth,
    });
    let writes_depth = any(|k| match k {
        OpKind::Clear { depth, stencil, .. } => depth.is_some() || stencil.is_some(),
        OpKind::Draw { writes_depth, .. } => *writes_depth,
    });
    if !uses_depth {
        depth = (Load::None, Store::None);
        stencil = (Load::None, Store::None);
    } else if !writes_depth {
        depth.1 = Store::None;
        stencil.1 = Store::None;
    }
    Pass {
        ops: range,
        area,
        color,
        depth,
        stencil,
    }
}

/// A store is needed only when a later pass loads what this one wrote: walk
/// backwards, tracking whether the depth contents are read again.
fn settle_depth(ops: &[Op], passes: &mut [Pass], next: NextDepth) {
    let mut needed = match next {
        NextDepth::Keep => None,
        NextDepth::Cleared(rect) => Some(rect),
        NextDepth::Discarded => Some(Rect {
            x: 0,
            y: 0,
            w: u32::MAX,
            h: u32::MAX,
        }),
    };
    for pass in passes.iter_mut().rev() {
        // `needed` is the rectangle known to be overwritten before any read.
        if let Some(cleared) = needed
            && contains(cleared, pass.area)
        {
            for store in [&mut pass.depth.1, &mut pass.stencil.1] {
                if *store == Store::Store {
                    *store = Store::DontCare;
                }
            }
        }
        let first = ops[pass.ops.start];
        needed = match first.kind {
            OpKind::Clear {
                depth: Some(_),
                stencil: Some(_),
                ..
            } => Some(pass.area),
            _ if pass.depth.0 == Load::None => needed,
            _ => None,
        };
    }
}

fn union(ops: &[Op]) -> Rect {
    ops.iter().map(|op| op.area).reduce(bounds).unwrap()
}

fn bounds(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    Rect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    }
}

fn contains(outer: Rect, inner: Rect) -> bool {
    let right = |r: Rect| u64::from(r.x) + u64::from(r.w);
    let bottom = |r: Rect| u64::from(r.y) + u64::from(r.h);
    outer.x <= inner.x
        && outer.y <= inner.y
        && right(outer) >= right(inner)
        && bottom(outer) >= bottom(inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
    fn clear(area: Rect, color: bool, depth: bool) -> Op {
        Op {
            area,
            kind: OpKind::Clear {
                color: color.then_some([0.0; 4]),
                depth: depth.then_some(1.0),
                stencil: depth.then_some(0),
            },
        }
    }
    fn sprite(area: Rect, opaque: bool) -> Op {
        Op {
            area,
            kind: OpKind::Draw {
                writes_color: true,
                uses_depth: false,
                writes_depth: false,
                opaque: opaque.then_some(area),
            },
        }
    }
    fn mesh(area: Rect) -> Op {
        Op {
            area,
            kind: OpKind::Draw {
                writes_color: true,
                uses_depth: true,
                writes_depth: true,
                opaque: None,
            },
        }
    }

    #[test]
    fn small_damage_loads_and_stores_only_its_area() {
        let passes = plan(
            &[sprite(rect(10, 10, 20, 20), false)],
            false,
            NextDepth::Keep,
        );
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].area, rect(10, 10, 20, 20));
        assert_eq!(passes[0].color, (Load::Load, Store::Store));
        assert_eq!(passes[0].depth, (Load::None, Store::None));
    }

    #[test]
    fn an_opaque_first_draw_over_the_area_skips_the_load() {
        let ops = [
            sprite(rect(0, 0, 64, 64), true),
            sprite(rect(8, 8, 8, 8), false),
        ];
        let passes = plan(&ops, false, NextDepth::Keep);
        assert_eq!(passes[0].color.0, Load::DontCare);
        let ops = [
            sprite(rect(0, 0, 8, 8), true),
            sprite(rect(8, 8, 8, 8), false),
        ];
        assert_eq!(plan(&ops, false, NextDepth::Keep)[0].color.0, Load::Load);
    }

    #[test]
    fn a_covering_clear_becomes_the_load() {
        let ops = [
            clear(rect(0, 0, 64, 64), true, true),
            mesh(rect(4, 4, 8, 8)),
        ];
        let passes = plan(&ops, true, NextDepth::Keep);
        assert_eq!(passes.len(), 1);
        assert_eq!(passes[0].area, rect(0, 0, 64, 64));
        assert_eq!(passes[0].color.0, Load::Clear([0.0; 4]));
        assert_eq!(passes[0].depth, (Load::Clear(1.0), Store::Store));
    }

    #[test]
    fn a_small_clear_before_wider_draws_stays_alone() {
        let ops = [
            clear(rect(0, 0, 8, 8), true, false),
            sprite(rect(0, 0, 64, 64), false),
        ];
        let passes = plan(&ops, false, NextDepth::Keep);
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].area, rect(0, 0, 8, 8));
        assert_eq!(passes[1].color.0, Load::Load);
    }

    #[test]
    fn depth_cleared_next_is_not_stored() {
        let ops = [
            clear(rect(0, 0, 64, 64), true, true),
            mesh(rect(0, 0, 64, 64)),
        ];
        let next = NextDepth::Cleared(rect(0, 0, 64, 64));
        assert_eq!(plan(&ops, true, next)[0].depth.1, Store::DontCare);
        assert_eq!(
            plan(&ops, true, NextDepth::Discarded)[0].stencil.1,
            Store::DontCare
        );
        assert_eq!(plan(&ops, true, NextDepth::Keep)[0].depth.1, Store::Store);
    }

    #[test]
    fn depth_overwritten_by_a_later_pass_in_the_run_is_not_stored() {
        let ops = [
            mesh(rect(0, 0, 64, 64)),
            clear(rect(0, 0, 64, 64), false, true),
            mesh(rect(0, 0, 64, 64)),
        ];
        let passes = plan(&ops, true, NextDepth::Keep);
        assert_eq!(passes.len(), 2);
        assert_eq!(passes[0].depth.1, Store::DontCare);
        assert_eq!(passes[1].depth.1, Store::Store);
    }

    #[test]
    fn untouched_attachments_pay_nothing() {
        let passes = plan(&[sprite(rect(0, 0, 8, 8), false)], true, NextDepth::Keep);
        assert_eq!(passes[0].depth, (Load::None, Store::None));
        let depth_only = Op {
            area: rect(0, 0, 8, 8),
            kind: OpKind::Draw {
                writes_color: false,
                uses_depth: true,
                writes_depth: true,
                opaque: None,
            },
        };
        assert_eq!(
            plan(&[depth_only], true, NextDepth::Keep)[0].color,
            (Load::None, Store::None)
        );
    }
}
