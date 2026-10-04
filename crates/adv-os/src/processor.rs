//! The virtual flat x86 process has one logical processor, independent of host topology.
//! Affinity constrains this visible profile; it never changes a host thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct Processors;
impl Processors {
    pub const fn active_mask(&self) -> u32 {
        1
    }
    pub const fn process_mask(&self) -> u32 {
        self.active_mask()
    }
    pub const fn count(&self) -> u32 {
        self.active_mask().count_ones()
    }
    pub const fn current(&self) -> u32 {
        0
    }
    pub const fn accepts_mask(&self, mask: u32) -> bool {
        mask != 0 && mask & !self.process_mask() == 0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_cpu_queries_and_affinity_share_the_same_profile() {
        let p = Processors;
        assert_eq!(p.count(), 1);
        assert_eq!(p.current(), 0);
        assert_eq!(p.process_mask(), p.active_mask());
        assert!(p.accepts_mask(1));
        for mask in [0, 2, 3, u32::MAX] {
            assert!(!p.accepts_mask(mask));
        }
    }
}
