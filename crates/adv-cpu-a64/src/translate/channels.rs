//! Guarded planar-byte interleaving (A64-TR-6). Recognition is over decoded
//! dataflow and loop edges; no addresses, symbols or byte signatures are used.
//! A batch replaces 64 iterations. One following scalar iteration is also
//! proven non-faulting: it restores every temporary and flag before any exit.
use super::decode::{Alu, Cc, EBP, ESP, Mem, Narrow8or16, Op, Part, Src};
use super::trace::{Edge, Mode, Next, Trace};
use super::*;
use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};

const BATCH: u32 = 64;

struct Plan {
    sources: Vec<Mem>,
    destination: Mem,
    bound: Src,
    instructions: u32,
}

impl Plan {
    fn at(trace: &Trace, start: usize, checked: &dyn Fn(u32) -> bool) -> Option<Self> {
        if !trace.merge[start] {
            return None;
        }
        let mut ops = Vec::new();
        let end = (start..trace.len())
            .find(|&index| trace.insts[index].taken == Some(Edge::Internal(start)))?;
        for index in start..=end {
            let inst = trace.insts[index];
            if (index != start && (trace.head(index) || trace.merge[index]))
                || inst.mode != Mode::Plain
                || inst.call.is_some()
                || checked(inst.pc)
                || (index != end && (inst.next != Next::Fall || inst.taken.is_some()))
            {
                return None;
            }
            if inst.op != Op::Nop {
                ops.push(inst.op);
            }
        }
        let Op::Jcc(Cc::Flags(ConditionCode::b | ConditionCode::ne), _) = ops.pop()? else {
            return None;
        };
        let Op::Alu(Alu::Cmp, Src::Reg(destination), bound) = ops.pop()? else {
            return None;
        };
        let Op::Alu(Alu::Add, Src::Reg(step), Src::Imm(channels @ 2..=4)) = ops.pop()? else {
            return None;
        };
        if step != destination
            || matches!(destination, ESP | EBP)
            || ops.len() != channels as usize * 3
        {
            return None;
        }
        let mut pointers = 1u8 << destination;
        let mut temps = 0u8;
        let mut pairs = Vec::new();
        for ops in ops.as_chunks::<3>().0 {
            let (temp, byte, source) = match ops[0] {
                Op::Extend(temp @ 0..=3, Narrow8or16::Mem(source, false), _) => {
                    (temp, temp, source)
                }
                Op::Narrow(None, 1, Part::Reg(byte), Part::Mem(source)) => (byte % 4, byte, source),
                _ => return None,
            };
            let source_reg = source.base?;
            if source.fs
                || source.index.is_some()
                || matches!(source_reg, ESP | EBP)
                || pointers & (1 << source_reg) != 0
            {
                return None;
            }
            let increment = |op| {
                matches!(op, Op::Step(true, r) if r == source_reg)
                    || matches!(op, Op::Alu(Alu::Add, Src::Reg(r), Src::Imm(1)) if r == source_reg)
            };
            let store = if increment(ops[1]) {
                ops[2]
            } else if increment(ops[2]) {
                ops[1]
            } else {
                return None;
            };
            let Op::Narrow(None, 1, Part::Mem(target), Part::Reg(value)) = store else {
                return None;
            };
            if value != byte
                || target.fs
                || target.base != Some(destination)
                || target.index.is_some()
            {
                return None;
            }
            pointers |= 1 << source_reg;
            temps |= 1 << temp;
            pairs.push((target.disp, source));
        }
        if pointers & temps != 0 {
            return None;
        }
        let stable = |r| (pointers | temps) & (1u8 << r) == 0;
        match bound {
            Src::Reg(r) if stable(r) => {}
            Src::Mem(m) if !m.fs && m.base.is_none_or(stable) && m.index.is_none_or(stable) => {}
            Src::Imm(_) => {}
            _ => return None,
        }
        pairs.sort_unstable_by_key(|p| p.0);
        let offset = pairs[0].0;
        if !pairs
            .iter()
            .enumerate()
            .all(|(n, p)| offset.checked_add(n as u32) == Some(p.0))
        {
            return None;
        }
        Some(Self {
            sources: pairs.into_iter().map(|p| p.1).collect(),
            destination: Mem {
                base: Some(destination),
                index: None,
                scale: 1,
                address_bits: 32,
                disp: offset,
                fs: false,
            },
            bound,
            instructions: (end - start + 1) as u32,
        })
    }
}

impl Translator<'_> {
    pub(super) fn channel_batch(&mut self, index: usize) {
        let Some(plan) = Plan::at(self.trace, index, self.checked) else {
            return;
        };
        *self.uses_permissions = true;
        let scalar = self.asm.label();
        let channels = plan.sources.len() as u32;
        let cost = BATCH * plan.instructions;
        // The loop head is canonical. Guards borrow no live guest/flag values.
        self.emit(mrs_nzcv(X15));
        self.mov_imm(X16, cost);
        self.emit(cmp(X, X9, X16, Lsl, 0));
        self.asm.b_cond(Cond::Lo, scalar);
        self.emit(ldr(X, X5, RUNTIME, crate::runtime::CLASSES));
        self.channel_address(plan.destination, X0);
        self.emit(add_imm(X, X1, X0, (BATCH + 1) * channels, false));
        self.channel_range(X0, (BATCH + 1) * channels, true, scalar);
        match plan.bound {
            Src::Reg(r) => self.emit(mov(W, X2, host(r))),
            Src::Imm(value) => self.mov_imm(X2, value),
            Src::Mem(m) => {
                self.channel_address(m, X16);
                self.channel_range(X16, 4, false, scalar);
                self.channel_disjoint(X16, 4, scalar);
                self.emit(ldr_reg(W, X2, WINDOW, X16, false));
            }
        }
        // Require all replaced loop tests to continue, without pointer wrap.
        self.emit_all(&[
            mov(W, X3, host(plan.destination.base.unwrap())),
            add_imm(X, X3, X3, BATCH * channels, false),
            cmp(X, X3, X2, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Hs, scalar);
        for &source in &plan.sources {
            self.channel_address(source, X16);
            self.channel_range(X16, BATCH + 1, false, scalar);
            self.channel_disjoint(X16, BATCH + 1, scalar);
        }
        // No memory access, budget boundary or guest flag observation can fail
        // between here and the scalar tail's final CMP. All stores pass the code-watch barrier.
        self.emit(add(X, X6, WINDOW, X0, Lsl, 0));
        for offset in (0..BATCH).step_by(16) {
            for (lane, &source) in plan.sources.iter().enumerate() {
                self.channel_address(
                    Mem {
                        disp: source.disp.wrapping_add(offset),
                        ..source
                    },
                    X16,
                );
                self.emit(vldr_reg(VMem::Q, VReg::new(lane as u8), WINDOW, X16));
            }
            self.emit(vst_interleaved(channels, VReg::new(0), X6));
            if offset + 16 != BATCH {
                self.emit(add_imm(X, X6, X6, channels * 16, false));
            }
        }
        for source in plan.sources {
            let r = host(source.base.unwrap());
            self.emit(add_imm(W, r, r, BATCH, false));
        }
        let destination = host(plan.destination.base.unwrap());
        self.emit(add_imm(
            W,
            destination,
            destination,
            BATCH * channels,
            false,
        ));
        self.mov_imm(X16, cost);
        self.emit(sub(X, X9, X9, X16, Lsl, 0));
        #[cfg(test)]
        self.emit_all(&[
            ldr(X, X3, RUNTIME, crate::runtime::CHANNEL_BATCHES),
            add_imm(X, X3, X3, 1, false),
            str(X, X3, RUNTIME, crate::runtime::CHANNEL_BATCHES),
        ]);
        self.asm.bind(scalar);
        self.emit(msr_nzcv(X15));
    }

    fn channel_address(&mut self, mem: Mem, out: Reg) {
        self.emit(mov(W, out, mem.base.map(host).unwrap_or(Reg::ZR)));
        if let Some(index) = mem.index {
            self.emit(add(
                W,
                out,
                out,
                host(index),
                Lsl,
                mem.scale.trailing_zeros(),
            ));
        }
        if mem.disp != 0 {
            self.mov_imm(X17, mem.disp);
            self.emit(add(W, out, out, X17, Lsl, 0));
        }
    }

    /// The entire range, including the scalar tail, must stay in one directly
    /// accessible guest page. The snapshot includes host-page intersections.
    fn channel_range(&mut self, address: Reg, bytes: u32, store: bool, scalar: Label) {
        self.emit_all(&[
            add_imm(X, X3, address, bytes - 1, false),
            eor(X, X4, X3, address, Lsl, 0),
        ]);
        self.asm.tbz(true, X4, 12, scalar);
        self.emit_all(&[
            lsr_imm(W, X3, address, 12),
            ldr_narrow(Narrow::Byte, X4, X5, X3),
        ]);
        self.asm.tbz(
            false,
            X4,
            if store { WRITE } else { READ }.trailing_zeros(),
            scalar,
        );
        for bit in [SHARED].into_iter().chain(store.then_some(EXEC)) {
            self.asm.tbz(true, X4, bit.trailing_zeros(), scalar);
        }
    }

    /// Compare full-width half-open ranges, including an end at 4 GiB.
    fn channel_disjoint(&mut self, address: Reg, bytes: u32, scalar: Label) {
        let safe = self.asm.label();
        self.emit_all(&[
            add_imm(X, X3, address, bytes, false),
            cmp(X, X3, X0, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Ls, safe);
        self.emit(cmp(X, address, X1, Lsl, 0));
        self.asm.b_cond(Cond::Lo, scalar);
        self.asm.bind(safe);
    }
}
