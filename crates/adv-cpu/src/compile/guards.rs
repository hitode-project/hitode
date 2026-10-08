//! Share compatible cold guard footers while retaining each SSA state prefix.
use super::*;

pub(super) type Key = (u32, bool, Option<(u32, u32)>);

impl Emitter<'_> {
    pub(super) fn shared_guard(&mut self, bad: Value, reason: Exit, addr: Option<Value>) -> bool {
        // Resident lanes, deferred MMX effects and kernel fuel settlement need
        // their site-specific recovery. Ordinary guards can merge their SSA
        // prefixes directly, without a forwarding block for every check.
        if self.fast.is_some()
            || self.kernel.is_some()
            || self.mmx_effects.is_some()
            || self.lanes_active()
            || self.xmm.iter().any(Option::is_some)
            || !self.x87.is_empty()
        {
            return false;
        }
        // Vector/FP regions keep local footers: merging their long-lived state
        // increases register-allocation spills and final code size.
        let recipe = if self.optimizing {
            let kind = self.b.use_var(self.lazy[3]);
            let carry = self.b.use_var(self.lazy[6]);
            self.known(kind).zip(self.known(carry))
        } else {
            None
        };
        let key = (reason as u32, addr.is_some(), recipe);
        let block = if let Some(&(_, block)) = self.guard_exits.get(&key) {
            block
        } else {
            let block = self.b.create_block();
            self.b.set_cold_block(block);
            self.b.append_block_param(block, types::I32);
            if addr.is_some() {
                self.b.append_block_param(block, types::I32);
            }
            self.guard_exits.insert(key, (reason, block));
            block
        };
        let pc = self.constant(self.pc);
        let mut args = vec![pc.into()];
        if let Some(addr) = addr {
            args.push(addr.into());
        }
        let ok = self.b.create_block();
        self.b.ins().brif(bad, block, &args, ok, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
        true
    }

    pub(super) fn emit_guard_exits(&mut self) {
        for ((_, address, recipe), (reason, block)) in std::mem::take(&mut self.guard_exits) {
            self.b.switch_to_block(block);
            self.mark_source(diagnostics::overhead_source(diagnostics::Overhead::Exit));
            self.mmx_effects = None;
            if let Some((kind, carry)) = recipe {
                let kind = self.constant(kind);
                let carry = self.constant(carry);
                self.b.def_var(self.lazy[3], kind);
                self.b.def_var(self.lazy[6], carry);
            }
            let pc = self.b.block_params(block)[0];
            if address {
                let addr = self.b.block_params(block)[1];
                self.b.ins().store(
                    MemFlags::trusted(),
                    addr,
                    self.state,
                    offset_of!(CpuState, fault_addr) as i32,
                );
            }
            self.exit(reason, pc);
        }
    }
}
