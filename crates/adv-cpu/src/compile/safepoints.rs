//! Post-CFG bounded polling. Between polls, guest block addresses increase
//! strictly inside one group. This also cuts irreducible cycles: no cycle in
//! a finite total order consists entirely of increasing edges.
use super::*;

const MAX_BLOCKS: usize = 16;
const MAX_INSTRUCTIONS: usize = 256;

pub(super) struct Plan {
    groups: BTreeMap<u32, usize>,
    pub bound: u32,
}

impl Plan {
    pub fn new(instructions: &BTreeMap<u32, Instruction>, entries: &BTreeSet<u32>) -> Option<Self> {
        let starts: Vec<_> = entries.iter().copied().collect();
        let mut groups = BTreeMap::new();
        let (mut group, mut blocks, mut count, mut bound) = (0, 0, 0, 0);
        for &start in &starts {
            // Alternate x86 entries can overlap another instruction. Follow
            // actual boundaries instead of treating an address range as a block.
            let mut pc = start;
            let mut length = 0;
            while let Some(i) = instructions.get(&pc) {
                if pc != start && entries.contains(&pc) {
                    break;
                }
                length += 1;
                if i.flow_control() != FlowControl::Next {
                    break;
                }
                if i.next_ip32() <= pc {
                    return None;
                }
                pc = i.next_ip32();
            }
            // Splitting an existing basic block would change the fuel ABI.
            // Retain its checked implementation when it cannot be bounded.
            if length > MAX_INSTRUCTIONS {
                return None;
            }
            if blocks == MAX_BLOCKS || count + length > MAX_INSTRUCTIONS {
                group += 1;
                blocks = 0;
                count = 0;
            }
            groups.insert(start, group);
            blocks += 1;
            count += length;
            bound = bound.max(blocks);
        }
        Some(Self {
            groups,
            bound: bound as u32,
        })
    }

    pub fn crosses(&self, from: u32, to: u32) -> bool {
        let source = self.groups.get(&from);
        let target = self.groups.get(&to);
        to <= from || source != target
    }
}

impl Emitter<'_> {
    /// The cell is pinned with Compiled and never changes after publication.
    /// Loading an absolute pointer avoids a local-symbol ADRP range assumption.
    pub(super) fn poll_checked_address(&mut self) -> Value {
        let (_, cell) = self.poll.expect("poll body owns a checked target cell");
        let cell = self.b.ins().iconst(self.ptr_ty, cell as i64);
        self.b
            .ins()
            .load(self.ptr_ty, MemFlags::trusted().with_readonly(), cell, 0)
    }
    /// Fund the entire next acyclic group, but debit only executed blocks.
    /// A short residual budget uses the original checked body, without a Rust
    /// round trip, instruction replay, extra fuel, or a growing native stack.
    pub(super) fn poll_budget(&mut self, pc: Value) {
        let Some((plan, _)) = self.poll else {
            return;
        };
        let fuel = self.b.use_var(self.fuel);
        let enough = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            fuel,
            i64::from(plan.bound),
        );
        let ready = self.b.create_block();
        let residual = self.b.create_block();
        self.b.set_cold_block(residual);
        self.b.ins().brif(enough, ready, &[], residual, &[]);
        self.b.switch_to_block(residual);
        self.save_state(pc);
        let address = self.poll_checked_address();
        self.tail_to(address, pc);
        self.b.switch_to_block(ready);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "android",
            all(target_os = "macos", target_arch = "aarch64")
        ),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn direct_recovery_keeps_one_registered_body() {
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, dword_ptr(ecx)).unwrap();
        a.inc(eax).unwrap();
        let bytes = a.assemble(0x10000).unwrap();
        let mut owner = crate::Executor::new(0x10000, 0x20000).unwrap();
        owner.set_direct_memory(true).unwrap();
        owner.write(0x10000, &bytes).unwrap();
        owner.protect(0x10000, 4096, READ | EXEC).unwrap();
        let compiled =
            compile(&Region::new(0x10000, bytes.len()), &mut owner.memory, false).unwrap();
        // Run this filter with ADV_SAFEPOINT=poll to exercise the exclusion.
        assert!(compiled._poll_target.is_none());
    }

    #[test]
    fn forward_groups_are_bounded_and_every_nonforward_edge_polls() {
        let mut a = CodeAssembler::new(32).unwrap();
        for _ in 0..40 {
            a.inc(eax).unwrap();
        }
        let bytes = a.assemble(0x10000).unwrap();
        let instructions: BTreeMap<_, _> =
            Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE)
                .into_iter()
                .map(|i| (i.ip32(), i))
                .collect();
        let entries: BTreeSet<_> = instructions.keys().copied().collect();
        let plan = Plan::new(&instructions, &entries).unwrap();
        assert_eq!(plan.bound, 16);
        let starts: Vec<_> = entries.into_iter().collect();
        for (n, &pc) in starts.iter().enumerate() {
            assert!(plan.crosses(pc, pc));
            for &target in &starts[..n] {
                assert!(plan.crosses(pc, target));
            }
            if let Some(&next) = starts.get(n + 1) {
                assert_eq!(plan.crosses(pc, next), (n + 1) % MAX_BLOCKS == 0);
            }
        }
    }

    #[test]
    fn large_existing_block_keeps_its_original_fuel_boundary() {
        let mut a = CodeAssembler::new(32).unwrap();
        for _ in 0..=MAX_INSTRUCTIONS {
            a.inc(eax).unwrap();
        }
        let bytes = a.assemble(0x10000).unwrap();
        let instructions = Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE)
            .into_iter()
            .map(|i| (i.ip32(), i))
            .collect();
        assert!(Plan::new(&instructions, &BTreeSet::from([0x10000])).is_none());
    }
}
