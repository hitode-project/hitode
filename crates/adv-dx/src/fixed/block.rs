//! Captured fixed-function members and their values.
use super::{Setting, Slot, State, slots::Slots};
use std::collections::BTreeSet;

#[derive(Clone, Default)]
pub(crate) struct Members {
    pub material: bool,
    pub lights: BTreeSet<u32>,
    pub light_enable: BTreeSet<u32>,
    pub fvf: bool,
    pub render: Slots<i32, 4>,
    pub stage: Slots<i32, 1>,
    pub stage_slots: Slots<(usize, i32), 10>,
    pub sampler: Slots<i32, 1>,
    pub sampler_slots: Slots<(usize, i32), 10>,
    pub viewport: bool,
    pub scissor: bool,
    pub transforms: Slots<usize, 1>,
    pub clip_planes: Slots<usize, 1>,
}

impl Members {
    #[cfg(feature = "d3d9-counters")]
    pub(crate) fn len(&self) -> usize {
        [self.material, self.fvf, self.viewport, self.scissor]
            .into_iter()
            .filter(|v| *v)
            .count()
            + self.lights.len()
            + self.light_enable.len()
            + self.render.len()
            + self.stage.len()
            + self.stage_slots.len()
            + self.sampler.len()
            + self.sampler_slots.len()
            + self.transforms.len()
            + self.clip_planes.len()
    }

    pub(crate) fn classes(&self) -> u16 {
        [
            !self.render.is_empty()
                || self.material
                || !self.lights.is_empty()
                || !self.light_enable.is_empty(),
            !(self.stage.is_empty() && self.stage_slots.is_empty()),
            !(self.sampler.is_empty() && self.sampler_slots.is_empty()),
            self.viewport,
            self.scissor,
            !self.transforms.is_empty(),
            !self.clip_planes.is_empty(),
            self.fvf,
        ]
        .into_iter()
        .enumerate()
        .fold(0, |bits, (class, member)| {
            bits | (u16::from(member) << class)
        })
    }

    pub(crate) fn retain_classes(&mut self, classes: u16) {
        let changed = |class: usize| classes & (1 << class) != 0;
        if !changed(0) {
            self.render = Default::default();
            self.material = false;
            self.lights.clear();
            self.light_enable.clear();
        }
        if !changed(1) {
            self.stage = Default::default();
            self.stage_slots = Default::default();
        }
        if !changed(2) {
            self.sampler = Default::default();
            self.sampler_slots = Default::default();
        }
        self.viewport &= changed(3);
        self.scissor &= changed(4);
        if !changed(5) {
            self.transforms = Default::default();
        }
        if !changed(6) {
            self.clip_planes = Default::default();
        }
        self.fvf &= changed(7);
    }

    fn insert(&mut self, key: Slot) {
        match key {
            Slot::Fvf => self.fvf = true,
            Slot::Material => self.material = true,
            Slot::Light(i) => {
                self.lights.insert(i);
            }
            Slot::LightEnable(i) => {
                self.light_enable.insert(i);
            }
            Slot::Render(i) => {
                self.render.insert(i as i32);
            }
            Slot::Stage(i) => {
                self.stage.insert(i as i32);
            }
            Slot::StageRegister(slot, i) => {
                self.stage_slots.insert((slot, i as i32));
            }
            Slot::Sampler(i) => {
                self.sampler.insert(i as i32);
            }
            Slot::SamplerRegister(slot, i) => {
                self.sampler_slots.insert((slot, i as i32));
            }
            Slot::Viewport => self.viewport = true,
            Slot::Scissor => self.scissor = true,
            Slot::Transform(i) => {
                self.transforms.insert(i as usize);
            }
            Slot::ClipPlane(i) => {
                self.clip_planes.insert(i);
            }
        }
    }
}

pub(crate) struct Block {
    pub members: Members,
    values: State,
}
impl Block {
    pub(crate) fn new(source: &State, members: Members) -> Self {
        let values = State {
            key: Default::default(),
            fvf: source.fvf,
            render: source.render.capture(members.render.iter()),
            stage: source.stage.capture(members.stage.iter()),
            stage_slots: std::array::from_fn(|slot| {
                let keys: Slots<i32, 4> = members
                    .stage_slots
                    .iter()
                    .filter(|&(n, _)| n == slot)
                    .map(|(_, i)| i)
                    .collect();
                source.stage_slots[slot].capture(keys.iter())
            }),
            sampler: source.sampler.capture(members.sampler.iter()),
            sampler_slots: std::array::from_fn(|slot| {
                let keys: Slots<i32, 4> = members
                    .sampler_slots
                    .iter()
                    .filter(|&(n, _)| n == slot)
                    .map(|(_, i)| i)
                    .collect();
                source.sampler_slots[slot].capture(keys.iter())
            }),
            viewport: source.viewport,
            scissor: source.scissor,
            transforms: source.transforms,
            clip_planes: source.clip_planes,
            material: source.material,
            default_light: source.default_light,
            lights: source.lights.clone(),
        };
        Self { members, values }
    }

    pub(crate) fn record(&mut self, setting: Setting) {
        self.members.insert(self.values.apply(setting).slot);
    }

    pub(crate) fn capture(&mut self, source: &State, members: &Members) {
        copy(&mut self.values, source, members);
    }

    pub(crate) fn apply(&self, target: &mut State, members: &Members) {
        copy(target, &self.values, members);
    }
}

fn copy(target: &mut State, source: &State, members: &Members) {
    if members.classes() == 0 {
        return;
    }
    target.invalidate_key();
    if members.material {
        target.material = source.material;
    }
    for &i in &members.lights {
        target.light(i).value = source.lights[&i].value;
    }
    for &i in &members.light_enable {
        target.light(i).enabled = source.lights[&i].enabled;
    }
    if members.fvf {
        target.fvf = source.fvf;
    }
    for i in &members.render {
        target.render.insert(i, source.render[&i]);
    }
    for i in &members.stage {
        target.stage.insert(i, source.stage[&i]);
    }
    for (slot, i) in &members.stage_slots {
        target.stage_slots[slot].insert(i, source.stage_slots[slot][&i]);
    }
    for i in &members.sampler {
        target.sampler.insert(i, source.sampler[&i]);
    }
    for (slot, i) in &members.sampler_slots {
        target.sampler_slots[slot].insert(i, source.sampler_slots[slot][&i]);
    }
    if members.viewport {
        target.viewport = source.viewport;
    }
    if members.scissor {
        target.scissor = source.scissor;
    }
    for i in &members.transforms {
        target.transforms[i] = source.transforms[i];
    }
    for i in &members.clip_planes {
        target.clip_planes[i] = source.clip_planes[i];
    }
}
