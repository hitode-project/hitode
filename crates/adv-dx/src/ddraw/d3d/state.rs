//! Device7 state and snapshots over the shared fixed-function slots.
use super::{INVALID, desc};
use crate::fixed::{self, Block, Members, Setting};
use std::collections::BTreeMap;

pub(super) struct State {
    pub fixed: fixed::State,
    recording: Option<Block>,
    blocks: BTreeMap<u32, Block>,
    next_block: u32,
    pub clip_status: [u32; 8],
}

impl State {
    pub fn new(width: u32, height: u32, depth: bool) -> Self {
        Self {
            fixed: desc::state::defaults(width, height, depth),
            recording: None,
            blocks: BTreeMap::new(),
            next_block: 1,
            clip_status: [0; 8],
        }
    }

    pub fn set(&mut self, setting: Setting) {
        if let Some(block) = &mut self.recording {
            block.record(setting);
        } else {
            self.fixed.apply(setting);
        }
    }

    pub fn begin(&mut self) -> Result<(), u32> {
        if self.recording.is_some() {
            return Err(0x88760835);
        }
        self.recording = Some(Block::new(&self.fixed, Members::default()));
        Ok(())
    }

    pub fn end(&mut self) -> Result<u32, u32> {
        let block = self.recording.take().ok_or(0x88760836u32)?;
        Ok(self.insert(block))
    }

    pub fn create(&mut self, kind: u32) -> Result<u32, u32> {
        let mut members = Members::default();
        let fixed = &self.fixed;
        if !matches!(kind, 1..=3) {
            return Err(INVALID);
        }
        for &slot in fixed.render.keys() {
            if desc::state::render_value(fixed, slot as u32).is_ok()
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
        Ok(self.insert(Block::new(fixed, members)))
    }

    fn insert(&mut self, block: Block) -> u32 {
        let handle = self.next_block;
        self.next_block += 1;
        self.blocks.insert(handle, block);
        handle
    }

    pub fn apply(&mut self, handle: u32) -> Result<(), u32> {
        let block = self.blocks.get(&handle).ok_or(0x88760834u32)?;
        block.apply(&mut self.fixed, &block.members);
        Ok(())
    }

    pub fn capture(&mut self, handle: u32) -> Result<(), u32> {
        let block = self.blocks.get_mut(&handle).ok_or(0x88760834u32)?;
        block.capture(&self.fixed, &block.members.clone());
        Ok(())
    }

    pub fn delete(&mut self, handle: u32) -> Result<(), u32> {
        self.blocks.remove(&handle).ok_or(0x88760834u32)?;
        Ok(())
    }
}

fn vertex_render(slot: i32) -> bool {
    matches!(slot, 28..=29 | 34..=38 | 48 | 136..=143 | 145..=148 | 151..=152)
}
