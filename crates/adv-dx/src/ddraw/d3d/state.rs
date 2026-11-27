//! Device7 state and snapshots over the shared fixed-function slots.
use super::{INVALID, desc};
use crate::fixed::{self, Block, Members, Setting};
use std::collections::BTreeMap;

pub(super) struct State {
    pub fixed: fixed::State,
    recording: Option<Saved>,
    blocks: BTreeMap<u32, Saved>,
    next_block: u32,
    pub scene: bool,
    pub textures: [u32; 8],
    pub clip_status: [u32; 8],
}

struct Saved {
    fixed: Block,
    textures: [Option<u32>; 8],
}

pub(super) type BindingChanges = [(u32, u32); 8];

impl State {
    pub fn new(width: u32, height: u32, depth: bool, device7: Option<super::caps::Device>) -> Self {
        let mut fixed = desc::state::defaults(width, height, depth);
        if let Some(device) = device7 {
            for slot in (1..=153).filter(|&slot| desc::state::device7_render(slot)) {
                let value = desc::state::device7_render_value(&fixed, slot).unwrap();
                fixed.render.insert(slot as i32, value);
            }
            if device != super::caps::Device::Rgb {
                for slot in [2, 40] {
                    fixed.render.insert(slot, u32::MAX);
                }
            }
        }
        Self {
            fixed,
            recording: None,
            blocks: BTreeMap::new(),
            next_block: 1,
            scene: false,
            textures: [0; 8],
            clip_status: if device7.is_some() {
                [
                    2,
                    0x01ff_f000,
                    2048f32.to_bits(),
                    0,
                    2048f32.to_bits(),
                    0,
                    0,
                    0,
                ]
            } else {
                [0; 8]
            },
        }
    }

    pub fn set(&mut self, setting: Setting) {
        if let Some(block) = &mut self.recording {
            block.fixed.record(setting);
        } else {
            self.fixed.apply(setting);
        }
    }

    pub fn texture_for_set(&self, slot: usize) -> Option<u32> {
        let live = *self.textures.get(slot)?;
        Some(
            self.recording
                .as_ref()
                .map_or(live, |block| block.textures[slot].unwrap_or(0)),
        )
    }

    pub fn set_texture(&mut self, slot: usize, texture: u32) {
        if let Some(block) = &mut self.recording {
            block.textures[slot] = Some(texture);
        } else {
            self.textures[slot] = texture;
        }
    }

    pub fn texture_references(&self) -> impl Iterator<Item = u32> + '_ {
        self.textures
            .into_iter()
            .chain(
                self.blocks
                    .values()
                    .chain(self.recording.iter())
                    .flat_map(|block| block.textures.into_iter().flatten()),
            )
            .filter(|&texture| texture != 0)
    }

    pub fn begin(&mut self) -> Result<(), u32> {
        if self.recording.is_some() {
            return Err(0x88760835);
        }
        self.recording = Some(Saved {
            fixed: Block::new(&self.fixed, Members::default()),
            textures: [None; 8],
        });
        Ok(())
    }

    pub fn end(&mut self) -> Result<u32, u32> {
        let block = self.recording.take().ok_or(0x88760836u32)?;
        Ok(self.insert(block))
    }

    pub fn create(&mut self, kind: u32) -> Result<(u32, BindingChanges), u32> {
        let mut members = Members::default();
        let fixed = &self.fixed;
        if !matches!(kind, 1..=3) {
            return Err(INVALID);
        }
        for &slot in fixed.render.keys() {
            if desc::state::device7_render(slot as u32)
                && (kind == 1 || (kind == 3) == vertex_render(slot))
            {
                members.render.insert(slot);
            }
        }
        if kind != 3 {
            members.stage.extend(fixed.stage.keys().copied());
            members.sampler.extend(fixed.sampler.keys().copied());
            for stage in 1..8 {
                members
                    .stage_slots
                    .extend(fixed.stage_slots[stage].keys().map(|&slot| (stage, slot)));
                members
                    .sampler_slots
                    .extend(fixed.sampler_slots[stage].keys().map(|&slot| (stage, slot)));
            }
            members
                .stage_slots
                .extend(fixed.stage_slots[0].keys().map(|&slot| (0, slot)));
        }
        if kind != 2 {
            members.material = true;
            members.viewport = true;
            members.transforms.extend(0..14);
            members.clip_planes.extend(0..6);
            members.lights.extend(fixed.lights.keys().copied());
            members.light_enable.extend(fixed.lights.keys().copied());
        }
        let textures = if kind == 3 {
            [None; 8]
        } else {
            self.textures.map(Some)
        };
        let references = textures.map(|texture| (0, texture.unwrap_or(0)));
        let handle = self.insert(Saved {
            fixed: Block::new(fixed, members),
            textures,
        });
        Ok((handle, references))
    }

    fn insert(&mut self, block: Saved) -> u32 {
        let handle = self.next_block;
        self.next_block += 1;
        self.blocks.insert(handle, block);
        handle
    }

    pub fn apply(&mut self, handle: u32) -> Result<BindingChanges, u32> {
        if self.recording.is_some() {
            return Err(0x88760835);
        }
        let Some(block) = self.blocks.get(&handle) else {
            return Ok([(0, 0); 8]);
        };
        block.fixed.apply(&mut self.fixed, &block.fixed.members);
        Ok(std::array::from_fn(|slot| {
            block.textures[slot].map_or((0, 0), |texture| {
                (
                    std::mem::replace(&mut self.textures[slot], texture),
                    texture,
                )
            })
        }))
    }

    pub fn capture(&mut self, handle: u32) -> Result<BindingChanges, u32> {
        let block = self.blocks.get_mut(&handle).ok_or(0x88760834u32)?;
        block
            .fixed
            .capture(&self.fixed, &block.fixed.members.clone());
        Ok(std::array::from_fn(|slot| {
            let texture = self.textures[slot];
            block.textures[slot].as_mut().map_or((0, 0), |previous| {
                (std::mem::replace(previous, texture), texture)
            })
        }))
    }

    pub fn delete(&mut self, handle: u32) -> Result<BindingChanges, u32> {
        let block = self.blocks.remove(&handle).ok_or(0x88760834u32)?;
        Ok(block.textures.map(|texture| (texture.unwrap_or(0), 0)))
    }
}

fn vertex_render(slot: i32) -> bool {
    matches!(slot, 28..=29 | 34..=38 | 48 | 136..=143 | 145..=148 | 151..=152)
}
