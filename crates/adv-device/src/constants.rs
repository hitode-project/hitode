//! Register usage in the unchanged 16-byte uniform layout, shared by frontends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConstantUsage {
    /// Float registers, then int, bool, bump matrices and bump luminance.
    pub registers: [u64; 5],
    pub relative: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: GFX-REC-7
    fn sparse_runs_cover_word_boundaries_and_the_exact_block_size() {
        assert_eq!(ConstantUsage::default().ranges().count(), 0);
        assert_eq!(
            ConstantUsage::ALL.ranges().map(|r| r.len()).sum::<usize>(),
            4864
        );
        let mut usage = ConstantUsage::default();
        for n in [0, 1, 63, 64, 255, 256, 271, 272, 287, 288, 295, 296, 303] {
            usage.set(n);
        }
        assert_eq!(
            usage
                .ranges()
                .flat_map(|r| (r.start / 16)..(r.end / 16))
                .collect::<Vec<_>>(),
            [0, 1, 63, 64, 255, 256, 271, 272, 287, 288, 295, 296, 303]
        );
    }
}
impl ConstantUsage {
    pub const ALL: Self = Self {
        registers: [u64::MAX, u64::MAX, u64::MAX, u64::MAX, (1 << 48) - 1],
        relative: true,
    };
    pub fn set(&mut self, register: usize) {
        self.registers[register / 64] |= 1 << (register % 64);
    }
    pub fn ranges(&self) -> impl Iterator<Item = std::ops::Range<usize>> + Clone {
        self.registers
            .into_iter()
            .enumerate()
            .flat_map(|(word, mut bits)| {
                std::iter::from_fn(move || {
                    if bits == 0 {
                        return None;
                    }
                    let first = bits.trailing_zeros();
                    let end = first + (bits >> first).trailing_ones();
                    bits = if end == 64 {
                        0
                    } else {
                        bits & (u64::MAX << end)
                    };
                    Some((word * 64 + first as usize) * 16..(word * 64 + end as usize) * 16)
                })
            })
    }
}
