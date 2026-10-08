//! Checked scalar string elements, including restartable REP forms.
use super::*;
use iced_x86::Code;

pub(super) fn supported(i: &Instruction) -> bool {
    !i.has_lock_prefix()
        && matches!(
            i.segment_prefix(),
            R::None | R::CS | R::DS | R::ES | R::SS | R::FS
        )
        && matches!(
            i.code(),
            Code::Movsb_m8_m8
                | Code::Movsw_m16_m16
                | Code::Movsd_m32_m32
                | Code::Stosb_m8_AL
                | Code::Stosw_m16_AX
                | Code::Stosd_m32_EAX
                | Code::Lodsb_AL_m8
                | Code::Lodsw_AX_m16
                | Code::Lodsd_EAX_m32
        )
}

pub(super) fn repeated(i: &Instruction) -> bool {
    supported(i) && (i.has_rep_prefix() || i.has_repne_prefix())
}

impl Emitter<'_> {
    pub(super) fn string_instruction(&mut self, i: &Instruction) -> bool {
        let repeat = repeated(i);
        let word = (0..i.op_count())
            .any(|n| matches!(i.op_kind(n), OpKind::MemorySegSI | OpKind::MemoryESDI));
        let (source, destination, count) = if word {
            (R::SI, R::DI, R::CX)
        } else {
            (R::ESI, R::EDI, R::ECX)
        };
        let done = repeat.then(|| self.b.create_block());
        if let Some(done) = done {
            let value = self.read_register(count);
            let zero = self.b.ins().icmp_imm(IntCC::Equal, value, 0);
            let element = self.b.create_block();
            self.b.ins().brif(zero, done, &[], element, &[]);
            self.b.switch_to_block(element);
            self.b.seal_block(element);
        }
        let (copy, load, width, accumulator) = match i.code() {
            Code::Movsb_m8_m8 => (true, false, 1, R::AL),
            Code::Movsw_m16_m16 => (true, false, 2, R::AX),
            Code::Movsd_m32_m32 => (true, false, 4, R::EAX),
            Code::Stosb_m8_AL => (false, false, 1, R::AL),
            Code::Stosw_m16_AX => (false, false, 2, R::AX),
            Code::Stosd_m32_EAX => (false, false, 4, R::EAX),
            Code::Lodsb_AL_m8 => (false, true, 1, R::AL),
            Code::Lodsw_AX_m16 => (false, true, 2, R::AX),
            Code::Lodsd_EAX_m32 => (false, true, 4, R::EAX),
            _ => unreachable!(),
        };
        let ty = integer_type(width);
        let value = if copy || load {
            let mut address = self.read_register(source);
            if i.segment_prefix() == R::FS {
                let fs = self.b.ins().load(
                    types::I32,
                    MemFlags::trusted(),
                    self.state,
                    offset_of!(CpuState, fs_base) as i32,
                );
                address = self.b.ins().iadd(address, fs);
            }
            self.direct_load(address, ty)
        } else {
            let value = self.read_register(accumulator);
            if width == 4 {
                value
            } else {
                self.b.ins().ireduce(ty, value)
            }
        };
        if !load {
            let address = self.read_register(destination);
            self.direct_store(address, value);
        } else {
            let value = if width == 4 {
                value
            } else {
                self.b.ins().uextend(types::I32, value)
            };
            self.write_register(accumulator, value);
        }
        // All element accesses succeeded before any index/count commits. The
        // checked store retains the ordinary code-write barrier and shared IO.
        let flags = self.b.use_var(self.flags);
        let backwards = self.b.ins().band_imm(flags, 1 << 10);
        let forward = self.constant(width as u32);
        let backward = self.constant((width as u32).wrapping_neg());
        let delta = self.b.ins().select(backwards, backward, forward);
        for register in [source, destination] {
            if register == source && !(copy || load) || register == destination && load {
                continue;
            }
            let old = self.read_register(register);
            let next = self.b.ins().iadd(old, delta);
            self.write_register(register, next);
        }
        if let Some(done) = done {
            let old = self.read_register(count);
            let remaining = self.b.ins().iadd_imm(old, -1);
            self.write_register(count, remaining);
            let zero = self.b.ins().icmp_imm(IntCC::Equal, remaining, 0);
            let more = self.b.create_block();
            self.b.ins().brif(zero, done, &[], more, &[]);
            self.b.switch_to_block(more);
            self.b.seal_block(more);
            // The REP entry is a normal block safepoint: fuel, deadline and
            // interrupts are checked between committed elements on every host.
            self.direct(i.ip32());
            self.b.switch_to_block(done);
            self.b.seal_block(done);
            self.direct(i.next_ip32());
        }
        repeat
    }
}
