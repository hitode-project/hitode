//! Conservative per-subresource command comparisons and eight-frame slot history.
use super::*;
use adv_device::{
    gpu::{AttributeFormat, Image},
    surface::Rect,
};
use std::collections::HashSet;
pub(super) mod sampling;

pub(super) fn image(operation: &Operation) -> Option<&Image> {
    match operation {
        Operation::Draw(d) => Some(&d.target.image),
        Operation::Clear { target, .. } | Operation::Upload { target, .. } => Some(&target.image),
        Operation::Copy { destination, .. } => Some(&destination.image),
        _ => None,
    }
}
fn full(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    }
}

pub(super) fn bounds(env: &Env<'_>, device: &Device, operation: &Operation) -> Rect {
    match operation {
        Operation::Clear { rectangle, .. } => *rectangle,
        Operation::Copy { region, .. } => *region,
        Operation::Upload { target, .. } => full(&target.image),
        Operation::Draw(d) => draw_bounds(env, device, d).unwrap_or_else(|| full(&d.target.image)),
        _ => Rect::default(),
    }
}
fn draw_bounds(env: &Env<'_>, device: &Device, d: &fixed::Draw) -> Option<Rect> {
    use crate::shader::interpreter::{self, Constants, NoTextures};
    let mut positions = [[0.; 4]; 64];
    let count;
    let point = d.programmable.as_ref().filter(|p| p.parameters[2][3] != 0);
    let point_size = |value: f32| -> Option<f32> {
        let p = point?;
        let lo = f32::from_bits(p.parameters[12][0]);
        let hi = f32::from_bits(p.parameters[12][1]);
        (value.is_finite() && lo.is_finite() && hi.is_finite() && lo >= 0. && lo <= hi)
            .then(|| value.clamp(lo, hi))
    };
    let mut margin = if let Some(p) = point {
        1. + point_size(f32::from_bits(p.parameters[11][3]))? * 0.5
    } else {
        1.
    };
    if !d.vertices.is_empty() {
        if d.vertices.len() > 64 {
            return None;
        }
        count = d.vertices.len();
        for (out, v) in positions.iter_mut().zip(&d.vertices) {
            *out = v.position;
        }
    } else {
        let p = d.programmable.as_ref()?;
        if p.vertex_count > 64 {
            return None;
        }
        count = p.vertex_count as usize;
        let shader = shaders::bound(env, device.shaders.vertex?)?;
        let constants = Constants {
            f: p.vertex_constants.floats,
            i: p.vertex_constants.integers,
            b: p.vertex_constants.booleans,
            bump_matrix: p.vertex_constants.bump_matrix,
            bump_luminance: p.vertex_constants.bump_luminance,
        };
        for n in 0..p.vertex_count {
            let input = vertex_input(p, n)?;
            let output =
                interpreter::run(&shader.parsed, &input, &constants, &mut NoTextures).ok()?;
            if point.is_some() && shader.translation.as_ref().ok()?.writes_point_size {
                margin = margin.max(1. + point_size(output.point_size)? * 0.5);
            }
            let mut position = output.position;
            position[0] += position[3] / d.state.viewport.width;
            position[1] -= position[3] / d.state.viewport.height;
            positions[n as usize] = position;
        }
    }
    let v = d.state.viewport;
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for p in &positions[..count] {
        if p.iter().any(|x| !x.is_finite()) || p[3] <= 0. {
            return None;
        }
        let screen = [
            v.x + (p[0] / p[3] + 1.) * v.width * 0.5,
            v.y + (1. - p[1] / p[3]) * v.height * 0.5,
        ];
        for n in 0..2 {
            if !screen[n].is_finite() {
                return None;
            }
            lo[n] = lo[n].min(screen[n]);
            hi[n] = hi[n].max(screen[n]);
        }
    }
    if !margin.is_finite() {
        return None;
    }
    let x = (lo[0] - margin).floor().max(v.x).max(0.) as u32;
    let y = (lo[1] - margin).floor().max(v.y).max(0.) as u32;
    let r = Rect {
        x,
        y,
        w: ((hi[0] + margin).ceil().min(v.x + v.width) as u32).saturating_sub(x),
        h: ((hi[1] + margin).ceil().min(v.y + v.height) as u32).saturating_sub(y),
    }
    .clipped(full(&d.target.image));
    Some(d.state.scissor.map_or(r, |s| r.clipped(s)))
}
fn vertex_input(
    p: &adv_device::gpu::ProgrammableDraw,
    n: u32,
) -> Option<crate::shader::interpreter::Inputs> {
    let mut input = crate::shader::interpreter::Inputs::default();
    for stream in &p.streams {
        for a in &stream.attributes {
            if !matches!(
                a.format,
                AttributeFormat::Float1
                    | AttributeFormat::Float2
                    | AttributeFormat::Float3
                    | AttributeFormat::Float4
            ) {
                return None;
            }
            let mut v = [0., 0., 0., 1.];
            let start = (n * stream.stride + a.offset) as usize;
            for (component, bytes) in stream
                .bytes
                .get(start..start + a.format.size() as usize)?
                .chunks_exact(4)
                .enumerate()
            {
                v[component] = f32::from_le_bytes(bytes.try_into().ok()?);
            }
            *input.v.get_mut(a.location as usize)? = v;
        }
    }
    Some(input)
}
#[derive(Default)]
struct Samples {
    base: u128,
    reads: Vec<(sampling::Read, u128)>,
}
impl Samples {
    fn changed(&self, old: &Self, changes: &HashMap<Content, Rect>, bounds: Rect) -> Option<Rect> {
        if self.base != old.base || self.reads.len() != old.reads.len() {
            return None;
        }
        let mut damage = Rect::default();
        for ((read, version), (previous, before)) in self.reads.iter().zip(&old.reads) {
            if read != previous {
                return None;
            }
            if version != before {
                damage = damage.union(read.mapping.map(*changes.get(&read.source)?, bounds));
            }
        }
        Some(damage)
    }
}
#[derive(Clone, Default)]
struct Damage {
    rects: [Rect; 4],
    len: usize,
}
impl Damage {
    fn add(&mut self, r: Rect) {
        if r.empty() {
            return;
        }
        if self.len == 4 {
            self.rects[0] = self.bound().union(r);
            self.len = 1;
        } else {
            self.rects[self.len] = r;
            self.len += 1;
        }
    }
    fn bound(&self) -> Rect {
        self.rects[..self.len]
            .iter()
            .copied()
            .fold(Rect::default(), Rect::union)
    }
    fn extend(&mut self, other: &Self) {
        for &r in &other.rects[..other.len] {
            self.add(r);
        }
    }
}
#[derive(Default)]
struct Sequence {
    entries: Vec<(u128, Rect)>,
    samples: HashMap<usize, Samples>,
    last_write: u64,
    full: Rect,
    view: Rect,
    shape: u128,
    slot: u128,
    independent: bool,
}
#[derive(Default)]
struct History {
    previous: Sequence,
    next: Sequence,
    changes: std::collections::VecDeque<Damage>,
    slots: HashMap<u128, u64>,
    serial: u64,
    seen: u64,
}
impl History {
    fn advance(&mut self, force: bool, changes: &HashMap<Content, Rect>) -> (Rect, Rect) {
        let next = &self.next;
        let mut changed = Damage::default();
        if force || self.serial == 0 || self.previous.shape != next.shape {
            changed.add(next.full);
            self.slots.clear();
        } else {
            for n in 0..self.previous.entries.len().max(next.entries.len()) {
                let old = self.previous.entries.get(n);
                let new = next.entries.get(n);
                if old != new {
                    if let (Some((_, old_bounds)), Some((_, bounds)), Some(previous), Some(samples)) = (
                        old,
                        new,
                        self.previous.samples.get(&n),
                        next.samples.get(&n),
                    ) && old_bounds == bounds
                        && let Some(local) = samples.changed(previous, changes, *bounds)
                    {
                        changed.add(local);
                        continue;
                    }
                    if let Some((_, r)) = old {
                        changed.add(*r);
                    }
                    if let Some((_, r)) = new {
                        changed.add(*r);
                    }
                }
            }
        }
        self.serial += 1;
        self.changes.push_back(changed.clone());
        if self.changes.len() > 8 {
            self.changes.pop_front();
        }
        let since = self.slots.get(&next.slot).copied().unwrap_or(0);
        let mut redraw = Damage::default();
        if since == 0 || self.serial - since > self.changes.len() as u64 {
            redraw.add(next.full);
        } else {
            for d in self
                .changes
                .iter()
                .rev()
                .take((self.serial - since) as usize)
            {
                redraw.extend(d);
            }
        }
        let result = (changed.bound(), redraw.bound());
        self.slots.insert(next.slot, self.serial);
        self.slots.retain(|_, n| self.serial - *n <= 8);
        std::mem::swap(&mut self.previous, &mut self.next);
        self.next.entries.clear();
        self.next.samples.clear();
        result
    }
}
#[derive(Default)]
pub(super) struct Analysis {
    histories: HashMap<Content, History>,
    reads: HashSet<Content>,
    unsafe_order: bool,
    pub clips: HashMap<u128, Rect>,
    pub changed: HashMap<u128, Rect>,
    pub planned: bool,
    epoch: u64,
    ordinal: u64,
}
impl Analysis {
    pub fn written(&self, content: &Content) -> bool {
        self.histories
            .get(content)
            .is_some_and(|h| !h.next.entries.is_empty())
    }
    pub fn record(&mut self, p: &key::Prepared, command: u128, contents: &Contents) {
        let Some(image) = image(&p.operation) else {
            return;
        };
        let slot = hash(image.view());
        self.ordinal += 1;
        for &(k, overwrite) in &p.writes {
            self.unsafe_order |= self.reads.contains(&k);
            let s = &mut self.histories.entry(k).or_default().next;
            self.unsafe_order |= !s.entries.is_empty() && s.slot != slot;
            let depth = match &p.operation {
                Operation::Draw(d) => d.target.depth(),
                Operation::Clear { target, .. } => target.depth(),
                _ => None,
            };
            let bounds = if k.2 == 0 {
                full(image)
            } else {
                let depth = depth.unwrap();
                Rect {
                    x: 0,
                    y: 0,
                    w: depth.width(),
                    h: depth.height(),
                }
            };
            s.full = bounds;
            s.last_write = self.ordinal;
            s.view = full(image);
            s.shape = hash((
                rect(bounds),
                if k.2 == 0 {
                    image.alpha_mode() as u8
                } else {
                    depth.unwrap().format() as u8
                },
            ));
            s.slot = slot;
            let bounds = p.bounds.clipped(bounds);
            if s.entries.is_empty() {
                s.independent = overwrite && bounds == s.full;
            }
            // Only drawing can depend on the old values inside its write bounds.
            // Clears and copies overwrite that region even when the attachment is larger.
            let key = hash((
                p.write_key(k, command),
                (matches!(p.operation, Operation::Draw(_)) && !s.independent)
                    .then(|| contents.get(&k).copied().unwrap_or(0)),
            ));
            if !p.samples.is_empty() {
                let external: Vec<_> = p
                    .reads
                    .iter()
                    .filter(|k| !p.samples.iter().any(|r| r.source == **k))
                    .map(|k| (*k, contents.get(k).copied().unwrap_or(0)))
                    .collect();
                s.samples.insert(
                    s.entries.len(),
                    Samples {
                        base: hash((
                            p.key,
                            external,
                            (matches!(p.operation, Operation::Draw(_)) && !s.independent)
                                .then(|| contents.get(&k).copied().unwrap_or(0)),
                        )),
                        reads: p
                            .samples
                            .iter()
                            .map(|r| (*r, contents.get(&r.source).copied().unwrap_or(0)))
                            .collect(),
                    },
                );
            }
            s.entries.push((key, bounds));
        }
        self.reads
            .extend(p.reads.iter().copied().filter(|k| k.2 == 0));
    }
    pub fn plan(&mut self, force: bool) {
        if self.planned {
            return;
        }
        self.epoch += 1;
        let mut order: Vec<_> = self
            .histories
            .iter()
            .filter(|(_, h)| !h.next.entries.is_empty())
            .map(|(k, h)| (*k, h.next.last_write))
            .collect();
        order.sort_unstable_by_key(|(_, last)| *last);
        let mut changes = HashMap::new();
        for (content, _) in order {
            let h = self.histories.get_mut(&content).unwrap();
            let slot = h.next.slot;
            let full = h.next.view;
            let (changed, redraw) = h.advance(
                force || self.unsafe_order || h.seen + 1 != self.epoch,
                &changes,
            );
            changes.insert(content, changed);
            let redraw = redraw.clipped(full);
            h.seen = self.epoch;
            self.clips
                .entry(slot)
                .and_modify(|r| *r = r.union(redraw))
                .or_insert(redraw);
            if content.2 == 0 {
                self.changed
                    .entry(slot)
                    .and_modify(|r| *r = r.union(changed))
                    .or_insert(changed);
            }
        }
        self.planned = true;
    }
    pub fn rotatable(&self, slot: u128) -> bool {
        !self.unsafe_order
            && self.histories.iter().any(|(k, h)| {
                k.2 == 0
                    && h.previous.slot == slot
                    && h.previous.independent
                    && h.seen == self.epoch
            })
    }
    pub fn finish(&mut self) {
        for h in self.histories.values_mut() {
            h.next.entries.clear();
            h.next.samples.clear();
        }
        self.reads.clear();
        self.ordinal = 0;
        self.clips.clear();
        self.changed.clear();
        self.unsafe_order = false;
        self.planned = false;
    }
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }
    pub fn forget(&mut self, pointer: u32) {
        self.histories.retain(|k, _| k.3 != pointer);
    }
}

#[cfg(test)]
#[path = "damage_unit_tests.rs"]
mod tests;
