//! Immutable register snapshots in the existing uniform layout. Sparse captures
//! retain only observed registers; complete blocks are materialized on demand.
use super::ConstantBlock;
use crate::constants::ConstantUsage;
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, OnceLock},
};

#[derive(Clone)]
pub struct ConstantSnapshot(Storage);

#[derive(Clone)]
enum Storage {
    Packed(Arc<Packed>),
    Block(Arc<ConstantBlock>),
}
struct Packed {
    usage: ConstantUsage,
    words: Box<[[u32; 4]]>,
    block: OnceLock<Box<ConstantBlock>>,
}
impl ConstantSnapshot {
    pub fn register(&self, n: usize) -> [u32; 4] {
        match &self.0 {
            Storage::Block(block) => {
                bytemuck::cast_slice::<u8, [u32; 4]>(bytemuck::bytes_of(block.as_ref()))
                    .get(n)
                    .copied()
                    .unwrap_or_default()
            }
            Storage::Packed(p) => {
                let Some(&word) = p.usage.registers.get(n / 64) else {
                    return [0; 4];
                };
                let bit = n % 64;
                if word & (1 << bit) == 0 {
                    return [0; 4];
                }
                let index = p.usage.registers[..n / 64]
                    .iter()
                    .map(|v| v.count_ones() as usize)
                    .sum::<usize>()
                    + (word & ((1 << bit) - 1)).count_ones() as usize;
                p.words[index]
            }
        }
    }
    pub fn capture(usage: ConstantUsage, mut read: impl FnMut(usize) -> [u32; 4]) -> Self {
        let words = usage
            .ranges()
            .flat_map(|r| r.start / 16..r.end / 16)
            .map(&mut read)
            .collect();
        Self(Storage::Packed(Arc::new(Packed {
            usage,
            words,
            block: OnceLock::new(),
        })))
    }

    pub fn ranges(&self, usage: ConstantUsage) -> impl Iterator<Item = (usize, &[u8])> + Clone {
        let packed = match &self.0 {
            Storage::Packed(p) if p.usage == usage => Some(bytemuck::cast_slice(p.words.as_ref())),
            _ => None,
        };
        let block = packed.is_none().then(|| bytemuck::bytes_of(&**self));
        let mut at = 0;
        usage.ranges().map(move |r| {
            let bytes = if let Some(words) = packed {
                let start = at;
                at += r.len();
                &words[start..at]
            } else {
                &block.unwrap()[r.clone()]
            };
            (r.start, bytes)
        })
    }

    pub fn retained_bytes(&self) -> usize {
        match &self.0 {
            Storage::Packed(p) => {
                std::mem::size_of_val(p.words.as_ref())
                    + p.block
                        .get()
                        .map_or(0, |_| std::mem::size_of::<ConstantBlock>())
            }
            Storage::Block(_) => std::mem::size_of::<ConstantBlock>(),
        }
    }
}
impl Default for ConstantSnapshot {
    fn default() -> Self {
        static EMPTY: OnceLock<ConstantSnapshot> = OnceLock::new();
        EMPTY
            .get_or_init(|| Self::capture(ConstantUsage::default(), |_| [0; 4]))
            .clone()
    }
}
impl From<ConstantBlock> for ConstantSnapshot {
    fn from(block: ConstantBlock) -> Self {
        Self(Storage::Block(Arc::new(block)))
    }
}
impl Deref for ConstantSnapshot {
    type Target = ConstantBlock;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Storage::Block(b) => b,
            Storage::Packed(p) => p.block.get_or_init(|| {
                let mut block = ConstantBlock::default();
                let words: &mut [[u32; 4]] =
                    bytemuck::cast_slice_mut(bytemuck::bytes_of_mut(&mut block));
                for (index, value) in p
                    .usage
                    .ranges()
                    .flat_map(|r| r.start / 16..r.end / 16)
                    .zip(&p.words)
                {
                    words[index] = *value;
                }
                Box::new(block)
            }),
        }
    }
}
impl DerefMut for ConstantSnapshot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        if matches!(self.0, Storage::Packed(_)) {
            self.0 = Storage::Block(Arc::new(**self));
        }
        let Storage::Block(block) = &mut self.0 else {
            unreachable!()
        };
        Arc::make_mut(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: GFX-REC-1, GFX-REC-7
    #[test]
    fn sparse_ranges_preserve_bits_and_snapshots_survive_mutation() {
        let mut usage = ConstantUsage::default();
        for n in [0, 63, 64, 255, 256, 272, 288, 303] {
            usage.set(n);
        }
        let mut snapshot =
            ConstantSnapshot::capture(usage, |n| [n as u32, 0x8000_0000, 0x7fc0_1234, 7]);
        let saved = snapshot.clone();
        assert_eq!(snapshot.retained_bytes(), 8 * 16);
        let ranges: Vec<_> = snapshot
            .ranges(usage)
            .map(|(at, bytes)| (at, bytes.to_vec()))
            .collect();
        snapshot.floats[0] = [1.; 4];
        assert_eq!(
            saved.floats[0].map(f32::to_bits),
            [0, 0x8000_0000, 0x7fc0_1234, 7]
        );
        assert_eq!(saved.floats[1], [0.; 4]);
        assert_eq!(
            saved
                .ranges(usage)
                .map(|(at, bytes)| (at, bytes.to_vec()))
                .collect::<Vec<_>>(),
            ranges
        );
        assert_eq!(snapshot.floats[0], [1.; 4]);
        assert_eq!(
            saved.bump_luminance[7].map(f32::to_bits),
            [303, 0x8000_0000, 0x7fc0_1234, 7]
        );
    }
}
