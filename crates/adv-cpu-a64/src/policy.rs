//! Translation policy shared by the lowerings and the owner arbiter (A64A-ROLE-9).
//! Prices for generated code are AArch64 instruction words. The offload
//! actuator additionally needs nanoseconds to predict a scheduling deadline.

pub(crate) mod tier1 {
    pub const MAX_INSTRUCTIONS: usize = 96;
    pub const LOOP_LIMIT: usize = 1024;
    pub const WIDE_WEIGHT: usize = 4;
    pub const MAX_DEPTH: u8 = 8;
    pub const COLD_MAX_DEPTH: u8 = 1;
    pub const INLINE_ROOM: usize = 24;
    pub const INLINE_BODY_BYTES: u32 = 64;
    pub const NATIVE_REGION_MIN: u32 = 128;
    pub const NATIVE_REGION_MAX: u32 = 1024;
    pub const CHANNEL_BATCH: u32 = 64;

    pub const BLOCK_UNITS: u64 = 256;
    #[cfg(native)]
    pub const CHAIN_LEN: usize = 3;
    #[cfg(native)]
    pub const CACHE_LEN: usize = 64 << 20;
    #[cfg(native)]
    pub const PROMOTION: u8 = 4;
    #[cfg(native)]
    pub const CHECK_AFTER: u32 = 8;
}

pub(crate) mod resident {
    pub const MAX_PLAIN_FACTS: usize = 8;
    pub const MAX_DOMAIN_GUARDS: usize = 4;

    #[cfg(native)]
    pub const HOT: u32 = 64;
    #[cfg(native)]
    pub const VERSIONS: usize = 3;
    #[cfg(native)]
    pub const MISSES: u32 = 4;
    #[cfg(native)]
    pub const DOMAIN_MISS_SAMPLE: u32 = 4;
    #[cfg(native)]
    pub const SAMPLE: u32 = 8;
    #[cfg(native)]
    pub const PROBES: u8 = 8;
    #[cfg(native)]
    pub const PROBATION: u32 = 256;
    #[cfg(native)]
    pub const CHECKED_FAULTS: u64 = 2;

    #[cfg(native)]
    pub fn cut_pays(exits: u64, visits: u64) -> bool {
        // Cutting loses a transfer and resident work on each affected
        // iteration. Recovery pays an owner exit on every cold occurrence.
        const CUT_WORDS: u64 = 32;
        const RECOVERY_WORDS: u64 = 256;
        exits >= 2 && exits * RECOVERY_WORDS > visits * CUT_WORDS
    }

    /// Whether a Block(0) entry domain guard remains cumulatively profitable:
    /// saved work across in-domain iterations must cover the entry guard cost
    /// on all arrivals plus the extra fallback cost on out-of-domain misses.
    #[cfg(native)]
    pub fn domain_guard_pays(
        in_domain_entries: u64,
        in_domain_iterations: u64,
        misses: u64,
        guard_words: u32,
        saved_per_iter: u32,
    ) -> bool {
        const FALLBACK_WORDS: u64 = 32;
        let saved = in_domain_iterations.saturating_mul(u64::from(saved_per_iter));
        let guard_cost = in_domain_entries
            .saturating_add(misses)
            .saturating_mul(u64::from(guard_words));
        let miss_cost = misses.saturating_mul(FALLBACK_WORDS);
        saved >= guard_cost.saturating_add(miss_cost)
    }
}

pub(crate) mod proved {
    // Bounded ordinary-window search and checked indirect call/return.
    pub const SPAN_SEARCH_WORDS: u32 = 84;
    pub const INDIRECT_CALL_WORDS: u32 = 8;

    #[cfg(native)]
    pub const ROW_SAMPLE: u32 = 32;
    #[cfg(native)]
    pub const ROW_PROBE_WORDS: u64 = 8 * 1024;

    /// Row entry failures precede guest effects. A short profitable-work
    /// check is ordinary routing and never contributes to withdrawal.
    #[derive(Clone, Copy, Debug)]
    #[repr(u8)]
    pub(crate) enum RowFailure {
        Target,
        Range,
        Alias,
        Parameter,
        Count,
        Span,
    }

    impl RowFailure {
        // Page bounds and a moving stream's alias can change at the next row.
        // Other failures can wait for a new owner run or an outside arrival.
        #[cfg(native)]
        pub fn defer_backedge(self) -> bool {
            !matches!(self, Self::Range | Self::Alias)
        }
    }

    /// Estimated cost in host instruction words, against the scalar path.
    #[derive(Clone, Copy, Debug)]
    pub struct RowPrice {
        pub guard: u32,
        pub saved: u32,
        pub row_savings: u32,
        pub count_register: Option<u8>,
    }

    pub fn minimum_work(overhead: u32, saved: u32) -> Option<u32> {
        (saved != 0).then(|| overhead.div_ceil(saved))
    }
}

pub(crate) mod worker {
    pub const COMPUTATION_SLICE: u32 = 1 << 16;
    pub const RECALL_GRACE: u32 = 64;
    pub const PROOF_STEPS: usize = 128 * 1024;
    pub const PARAMETER_WORDS: usize = 32;
    pub const GATHERED_WORDS: usize = 4096;

    #[cfg(native)]
    pub const REOFFER: u32 = 64;
    #[cfg(native)]
    pub const RESPECIALIZE: u8 = 2;
    #[cfg(native)]
    pub const PAGE_GRANTS: usize = 256;
    /// A helper's wake latency before sampling its actual claims.
    #[cfg(native)]
    pub const WAKE_NS: u64 = 50_000;
}
