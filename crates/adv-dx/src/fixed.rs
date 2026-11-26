//! Shared fixed-function state, draw lowering and scalar vertex processing.
use crate::record::key::hash;
use adv_device::surface::Rect;
use std::{cell::Cell, collections::BTreeMap};

mod block;
mod lower;
mod registers;
pub(crate) mod slots;
pub(crate) mod tnl;
mod vertex;

pub(crate) use block::{Block, Members};
pub(crate) use lower::{
    Layout, LowerError, Lowered, Rules, STAGE_ARGUMENTS, compact_stage_ops, features, lower,
    second_stage,
};
pub(crate) use registers::{Render, Sampler, Stage, StateMap};
pub(crate) use vertex::{HELPERS, Input, VertexProgram, inputs};

/// Byte offsets and source streams of the fixed-function vertex inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct VertexLayout {
    pub transformed: bool,
    pub position: u32,
    pub color: Option<u32>,
    pub normal: Option<(u16, u32)>,
    pub specular: Option<(u16, u32)>,
    pub texcoords: [Option<(u16, u32, u8)>; 8],
    /// POSITION/POSITIONT and COLOR0 source stream numbers.
    pub streams: [u16; 2],
    /// Largest per-stream extent, also the vertex size for interleaved inputs.
    pub size: u32,
}
#[derive(Clone, Copy)]
pub(crate) struct Viewport {
    pub rect: Rect,
    pub min_depth: f32,
    pub max_depth: f32,
}
impl Viewport {
    fn key(self) -> (u32, u32, u32, u32, u32, u32) {
        (
            self.rect.x,
            self.rect.y,
            self.rect.w,
            self.rect.h,
            self.min_depth.to_bits(),
            self.max_depth.to_bits(),
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Material {
    pub colors: [[f32; 4]; 4],
    pub power: f32,
}

impl Material {
    fn key(self) -> [u32; 17] {
        std::array::from_fn(|i| {
            if i < 16 {
                self.colors[i / 4][i % 4].to_bits()
            } else {
                self.power.to_bits()
            }
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LightKind {
    Point = 1,
    Spot = 2,
    Directional = 3,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct LightValue {
    pub kind: LightKind,
    pub components: [u32; 25],
}

impl LightValue {
    pub(crate) fn words(self) -> [u32; 26] {
        let mut words = [self.kind as u32; 26];
        words[1..].copy_from_slice(&self.components);
        words
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Light {
    pub value: LightValue,
    pub enabled: bool,
}

impl std::hash::Hash for LightValue {
    fn hash<H: std::hash::Hasher>(&self, hash: &mut H) {
        self.words().hash(hash);
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Transform {
    World,
    View,
    Projection,
}

#[derive(Clone)]
pub(crate) struct State {
    key: Cell<Option<u128>>,
    pub(crate) fvf: u32,
    pub(crate) render: StateMap,
    pub(crate) stage: StateMap,
    pub(crate) stage_slots: [StateMap; 8],
    pub(crate) sampler: StateMap,
    pub(crate) sampler_slots: [StateMap; 20],
    pub(crate) viewport: Viewport,
    pub(crate) scissor: Rect,
    pub(crate) transforms: [[f32; 16]; 3],
    pub(crate) clip_planes: [[f32; 4]; 6],
    pub(crate) material: Material,
    default_light: LightValue,
    pub(crate) lights: BTreeMap<u32, Light>,
}

pub(crate) struct Registers {
    pub render: StateMap,
    pub stage: StateMap,
    pub stage_slots: [StateMap; 8],
    pub sampler: StateMap,
    pub sampler_slots: [StateMap; 20],
}

pub(crate) enum Setting {
    Fvf(u32),
    Render(Render, u32),
    Stage(Stage, u32),
    StageRegister(usize, Stage, u32),
    Sampler(Sampler, u32),
    SamplerRegister(usize, Sampler, u32),
    Viewport(Viewport),
    Scissor(Rect),
    Transform(Transform, [f32; 16]),
    MultiplyTransform(Transform, [f32; 16]),
    ClipPlane(usize, [f32; 4]),
    Material(Material),
    Light(u32, LightValue),
    LightEnable(u32, bool),
}

pub(crate) struct Changed {
    pub slot: Slot,
    pub changed: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum Slot {
    Fvf,
    Material,
    Light(u32),
    LightEnable(u32),
    Render(Render),
    Stage(Stage),
    StageRegister(usize, Stage),
    Sampler(Sampler),
    SamplerRegister(usize, Sampler),
    Viewport,
    Scissor,
    Transform(Transform),
    ClipPlane(usize),
}
impl Slot {
    pub(crate) fn class(&self) -> usize {
        match self {
            Self::Fvf => 7,
            Self::Render(_) | Self::Material | Self::Light(_) | Self::LightEnable(_) => 0,
            Self::Stage(_) | Self::StageRegister(_, _) => 1,
            Self::Sampler(_) | Self::SamplerRegister(_, _) => 2,
            Self::Viewport => 3,
            Self::Scissor => 4,
            Self::Transform(_) => 5,
            Self::ClipPlane(_) => 6,
        }
    }
}

impl State {
    pub(crate) fn new(
        registers: Registers,
        width: u32,
        height: u32,
        material: Material,
        default_light: LightValue,
    ) -> Self {
        let identity = [
            1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
        ];
        let Registers {
            render,
            stage,
            stage_slots,
            sampler,
            sampler_slots,
        } = registers;
        let rect = Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        Self {
            key: Cell::new(None),
            fvf: 0,
            render,
            stage,
            stage_slots,
            sampler,
            sampler_slots,
            viewport: Viewport {
                rect,
                min_depth: 0.,
                max_depth: 1.,
            },
            scissor: rect,
            transforms: [identity; 3],
            clip_planes: [[0.; 4]; 6],
            material,
            default_light,
            lights: BTreeMap::new(),
        }
    }

    pub(crate) fn key(&self) -> u128 {
        if let Some(key) = self.key.get() {
            return key;
        }
        let key = self.fingerprint();
        self.key.set(Some(key));
        key
    }

    #[cfg(test)]
    pub(crate) fn cached_key(&self) -> Option<u128> {
        self.key.get()
    }

    pub(crate) fn invalidate_key(&mut self) {
        self.key.set(None);
    }

    pub(crate) fn fingerprint(&self) -> u128 {
        let Self {
            key: _,
            fvf,
            render,
            stage,
            stage_slots,
            sampler,
            sampler_slots,
            viewport,
            scissor,
            transforms,
            clip_planes,
            material,
            default_light: _,
            lights,
        } = self;
        hash((
            fvf,
            render,
            stage,
            stage_slots,
            sampler,
            sampler_slots,
            (
                viewport.rect.x,
                viewport.rect.y,
                viewport.rect.w,
                viewport.rect.h,
                viewport.min_depth.to_bits(),
                viewport.max_depth.to_bits(),
            ),
            (
                scissor.x as i32,
                scissor.y as i32,
                (scissor.x + scissor.w) as i32,
                (scissor.y + scissor.h) as i32,
            ),
            transforms.map(|m| m.map(f32::to_bits)),
            clip_planes.map(|m| m.map(f32::to_bits)),
            material.key(),
            lights,
        ))
    }

    pub(crate) fn class_key(&self, class: usize) -> u128 {
        match class {
            0 => hash((&self.render, self.material.key(), &self.lights)),
            1 => hash((&self.stage, &self.stage_slots)),
            2 => hash((&self.sampler, &self.sampler_slots)),
            3 => hash(self.viewport.key()),
            4 => hash((
                self.scissor.x as i32,
                self.scissor.y as i32,
                (self.scissor.x + self.scissor.w) as i32,
                (self.scissor.y + self.scissor.h) as i32,
            )),
            5 => hash(self.transforms.map(|v| v.map(f32::to_bits))),
            6 => hash(self.clip_planes.map(|v| v.map(f32::to_bits))),
            _ => unreachable!(),
        }
    }

    pub(crate) fn apply(&mut self, setting: Setting) -> Changed {
        let (slot, changed) = match setting {
            Setting::Fvf(value) => (Slot::Fvf, replace(&mut self.fvf, value)),
            Setting::Render(kind, value) => (
                Slot::Render(kind),
                self.render.insert(kind as i32, value) != Some(value),
            ),
            Setting::Stage(kind, value) => (
                Slot::Stage(kind),
                self.stage.insert(kind as i32, value) != Some(value),
            ),
            Setting::StageRegister(n, kind, value) => (
                Slot::StageRegister(n, kind),
                self.stage_slots[n].insert(kind as i32, value) != Some(value),
            ),
            Setting::Sampler(kind, value) => (
                Slot::Sampler(kind),
                self.sampler.insert(kind as i32, value) != Some(value),
            ),
            Setting::SamplerRegister(n, kind, value) => (
                Slot::SamplerRegister(n, kind),
                self.sampler_slots[n].insert(kind as i32, value) != Some(value),
            ),
            Setting::Viewport(value) => (
                Slot::Viewport,
                replace_bits(&mut self.viewport, value, Viewport::key),
            ),
            Setting::Scissor(value) => (Slot::Scissor, replace(&mut self.scissor, value)),
            Setting::Transform(n, value) => (
                Slot::Transform(n),
                replace_floats(&mut self.transforms[n as usize], value),
            ),
            Setting::MultiplyTransform(n, matrix) => {
                let value = std::array::from_fn(|i| {
                    tnl::row(
                        matrix[i / 4 * 4..i / 4 * 4 + 4].try_into().unwrap(),
                        &self.transforms[n as usize],
                    )[i % 4]
                });
                (
                    Slot::Transform(n),
                    replace_floats(&mut self.transforms[n as usize], value),
                )
            }
            Setting::ClipPlane(n, value) => (
                Slot::ClipPlane(n),
                replace_floats(&mut self.clip_planes[n], value),
            ),
            Setting::Material(value) => (
                Slot::Material,
                replace_bits(&mut self.material, value, Material::key),
            ),
            Setting::Light(index, value) => {
                let created = !self.lights.contains_key(&index);
                let changed = replace(&mut self.light(index).value, value);
                (Slot::Light(index), created || changed)
            }
            Setting::LightEnable(index, enabled) => {
                let created = !self.lights.contains_key(&index);
                let changed = replace(&mut self.light(index).enabled, enabled);
                (Slot::LightEnable(index), created || changed)
            }
        };
        if changed {
            self.invalidate_key();
        }
        Changed { slot, changed }
    }

    fn light(&mut self, index: u32) -> &mut Light {
        self.lights.entry(index).or_insert(Light {
            value: self.default_light,
            enabled: false,
        })
    }

    pub(crate) fn viewport_rect(&self) -> Rect {
        self.viewport.rect
    }

    pub(crate) fn target_changed(&mut self, width: u32, height: u32) -> [bool; 2] {
        let rect = Rect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        [
            self.apply(Setting::Viewport(Viewport {
                rect,
                min_depth: 0.,
                max_depth: 1.,
            }))
            .changed,
            self.apply(Setting::Scissor(rect)).changed,
        ]
    }

    pub(crate) fn clear_rect(&self) -> Rect {
        let viewport = self.viewport_rect();
        if self.render[&Render::ScissorEnable] == 0 {
            return viewport;
        }
        let left = viewport.x.max(self.scissor.x);
        let top = viewport.y.max(self.scissor.y);
        let right = (viewport.x + viewport.w).min(self.scissor.x + self.scissor.w);
        let bottom = (viewport.y + viewport.h).min(self.scissor.y + self.scissor.h);
        if left >= right || top >= bottom {
            return Rect {
                w: 0,
                h: 0,
                ..viewport
            };
        }
        Rect {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }
}

fn replace<T: Copy + PartialEq>(old: &mut T, value: T) -> bool {
    replace_bits(old, value, |v| v)
}
fn replace_bits<T: Copy, B: PartialEq>(old: &mut T, value: T, bits: impl Fn(T) -> B) -> bool {
    bits(std::mem::replace(old, value)) != bits(value)
}
fn replace_floats<const N: usize>(old: &mut [f32; N], value: [f32; N]) -> bool {
    replace_bits(old, value, |v| v.map(f32::to_bits))
}
