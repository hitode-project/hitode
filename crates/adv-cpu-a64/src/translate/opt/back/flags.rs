//! Tests read lazy producers; only recovery constructs a guest EFLAGS word.
use super::*;
use iced_x86::ConditionCode as CC;

// Positive conditions: O, B, E, BE, S, P, L, LE; their partners invert.
// A cleared mask bit selects a recipe over just the needed guest status bits.
const CONDITIONS: [(u8, [Cond; 8]); 14] = {
    use Cond::*;
    [
        (0xd7, [Vs, Hs, Eq, Eq, Mi, Eq, Lt, Le]), // Add
        (0xd7, [Vs, Hs, Eq, Eq, Mi, Eq, Lt, Le]), // ADC
        (0xdf, [Vs, Lo, Eq, Ls, Mi, Eq, Lt, Le]), // Sub
        (0xdf, [Vs, Lo, Eq, Ls, Mi, Eq, Lt, Le]), // SBB
        (0xdf, [Vs, Hs, Eq, Eq, Mi, Eq, Lt, Le]), // Logic
        (0xd5, [Vs, Eq, Eq, Eq, Mi, Eq, Lt, Le]), // Inc
        (0xd5, [Vs, Eq, Eq, Eq, Mi, Eq, Lt, Le]), // Dec
        (0xdf, [Vs, Lo, Eq, Ls, Mi, Eq, Lt, Le]), // Neg
        (0, [Eq; 8]),
        (0, [Eq; 8]),
        (0, [Eq; 8]),
        (0, [Eq; 8]),
        (0x2a, [Eq, Lt, Eq, Le, Eq, Vs, Eq, Eq]), // UCOMISD
        (0x2a, [Eq, Lt, Eq, Le, Eq, Vs, Eq, Eq]), // COMISD
    ]
};

impl Emitter<'_> {
    fn compare_floats(&mut self, kind: FlagKind, a: Host, b: Host) {
        let a = self.v_at(a, A);
        let b = self.v_at(b, B);
        self.asm.emit(super::compare(kind, a, b));
    }

    pub(super) fn condition(&mut self, flags: Host, cc: CC) -> Cond {
        if let Some(cond) = self.native(flags, cc) {
            return cond;
        }
        let (bits, invert) = match cc {
            CC::o | CC::no => (&[11][..], cc == CC::no),
            CC::b | CC::ae => (&[0][..], cc == CC::ae),
            CC::e | CC::ne => (&[6][..], cc == CC::ne),
            CC::s | CC::ns => (&[7][..], cc == CC::ns),
            CC::p | CC::np => (&[2][..], cc == CC::np),
            CC::be | CC::a => (&[0, 6][..], cc == CC::a),
            CC::l | CC::ge => (&[7, 11][..], cc == CC::ge),
            CC::le | CC::g => (&[7, 11, 6][..], cc == CC::g),
            CC::None => {
                self.asm.emit(cmp(W, Reg::ZR, Reg::ZR, Lsl, 0));
                return Cond::Ne;
            }
        };
        self.bit(flags, bits[0]);
        for (i, &bit) in bits[1..].iter().enumerate() {
            self.asm.emit(fmov_to_v(X, C, T));
            self.bit(flags, bit);
            self.asm.emit(fmov_from_v(X, U, C));
            self.asm.emit(if bits[0] == 7 && i == 0 {
                eor(W, T, T, U, Lsl, 0)
            } else {
                orr(W, T, T, U, Lsl, 0)
            });
        }
        self.asm.emit(cmp_imm(W, T, 0, false));
        if invert { Cond::Eq } else { Cond::Ne }
    }

    fn native(&mut self, flags: Host, cc: CC) -> Option<Cond> {
        let Host::Flags(n) = flags else {
            return None;
        };
        let FlagRecipe {
            kind, width, a, b, ..
        } = self.flags[n as usize]?;
        if cc == CC::None {
            return None;
        }
        let column = (cc as usize - 1) / 2;
        let (mut mask, row) = CONDITIONS[kind as usize];
        if width != 4 && matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow) {
            mask &= 0x53;
        }
        if mask & (1 << column) == 0 {
            return None;
        }
        if kind.floating() {
            self.compare_floats(kind, a, b);
        } else {
            self.arithmetic(flags, Reg::ZR);
        }
        Some(if cc as u8 & 1 == 0 {
            row[column].invert()
        } else {
            row[column]
        })
    }

    fn arithmetic(&mut self, flags: Host, d: Reg) {
        use FlagKind::*;
        let Host::Flags(n) = flags else {
            unreachable!()
        };
        let r = self.flags[n as usize].unwrap();
        let (kind, width, a, b, prior) = (r.kind, r.width, r.a, r.b, r.prior);
        let immediate = r.immediate;
        let subtract = matches!(kind, Sub | SubBorrow | Decrement | Negate);
        let binary = [adds, subs][usize::from(subtract)];
        let imm_op = [adds_imm, subs_imm][usize::from(subtract)];
        if matches!(kind, AddCarry | SubBorrow) {
            self.prior_bit(prior, 0);
            self.asm.emit(if kind == AddCarry {
                cmp_imm(W, T, 1, false)
            } else {
                cmp(W, Reg::ZR, T, Lsl, 0)
            });
        }
        let shift = 32 - u32::from(width) * 8;
        let mut ar = self.gp_at(a, T);
        if kind == Negate {
            self.asm.emit(subs(W, d, Reg::ZR, ar, Lsl, shift));
            if d == T && shift != 0 {
                self.asm.emit(lsr_imm(W, T, T, shift));
            }
            return;
        }
        if shift != 0 {
            self.asm.emit(lsl_imm(W, T, ar, shift));
            ar = T;
        }
        match kind {
            Logic => self.asm.emit(ands(W, d, ar, ar, Lsl, 0)),
            Increment | Decrement => {
                if shift == 0 {
                    self.asm.emit(imm_op(W, d, ar, 1, false));
                } else {
                    self.imm(U, 1 << shift);
                    self.asm.emit(binary(W, d, ar, U, Lsl, 0));
                }
            }
            Add | Sub if shift == 0 && immediate.and_then(add_immediate).is_some() => {
                let (n, shift) = add_immediate(immediate.unwrap()).unwrap();
                self.asm.emit(imm_op(W, d, ar, n, shift));
            }
            _ => {
                let mut br = self.gp_at(b, U);
                if shift != 0 && matches!(kind, AddCarry | SubBorrow) {
                    self.asm.emit(lsl_imm(W, U, br, shift));
                    if kind == AddCarry {
                        self.asm.emit(orr_imm(W, U, U, (1 << shift) - 1));
                    }
                    br = U;
                }
                self.asm.emit(match kind {
                    AddCarry => adcs(W, d, ar, br),
                    SubBorrow => sbcs(W, d, ar, br),
                    _ => binary(W, d, ar, br, Lsl, shift),
                });
            }
        }
        if d == T && shift != 0 {
            self.asm.emit(lsr_imm(W, T, T, shift));
        }
    }

    fn prior_bit(&mut self, prior: Option<Host>, bit: u32) {
        if let Some(p) = prior {
            self.bit(p, bit);
        } else {
            self.asm.emit(movz(W, T, 0, 0));
        }
    }

    fn bit(&mut self, flags: Host, bit: u32) {
        let Host::Flags(n) = flags else {
            let r = self.gp_at(flags, T);
            self.asm.emit(ubfx(W, T, r, bit, 1));
            return;
        };
        let r = self.flags[n as usize].unwrap();
        let (kind, width, a, b, prior) = (r.kind, r.width, r.a, r.b, r.prior);
        use FlagKind::*;
        if matches!(kind, Increment | Decrement) && bit == 0
            || kind == MulSigned && !matches!(bit, 0 | 11)
        {
            self.prior_bit(prior, bit);
            return;
        }
        if matches!(kind, Shl | Shr | Sar) {
            self.shift_bit(r, bit);
            return;
        }
        let cc = match bit {
            0 => CC::b,
            6 => CC::e,
            7 => CC::s,
            11 => CC::o,
            _ => CC::p,
        };
        if let Some(c) = self.native(flags, cc) {
            self.asm.emit(cset(W, T, c));
            return;
        }
        let bits = u32::from(width) * 8;
        if kind == MulSigned {
            let a = self.gp_at(a, T);
            let b = self.gp_at(b, U);
            self.asm.emit_all(&[
                sbfx(X, T, a, 0, bits),
                sbfx(X, U, b, 0, bits),
                mul(X, T, T, U),
                sbfx(X, U, T, 0, bits),
                cmp(X, T, U, Lsl, 0),
                cset(W, T, Cond::Ne),
            ]);
            return;
        }
        if kind.floating() {
            if bit == 6 {
                self.compare_floats(kind, a, b);
                self.asm.emit_all(&[
                    cset(W, T, Cond::Eq),
                    cset(W, U, Cond::Vs),
                    orr(W, T, T, U, Lsl, 0),
                ]);
            } else {
                self.asm.emit(movz(W, T, 0, 0));
            }
            return;
        }
        // Arithmetic places the numeric result in T only for parity or a
        // narrow carrying operation's zero test; no guest flags are assembled.
        self.arithmetic(flags, T);
        self.result_bit(bits, bit);
    }

    fn result_bit(&mut self, bits: u32, bit: u32) {
        match bit {
            6 => self.asm.emit_all(&[
                ubfx(W, T, T, 0, bits),
                cmp_imm(W, T, 0, false),
                cset(W, T, Cond::Eq),
            ]),
            7 => self.asm.emit(ubfx(W, T, T, bits - 1, 1)),
            _ => self.asm.emit_all(&[
                eor(W, T, T, T, Lsr, 4),
                eor(W, T, T, T, Lsr, 2),
                eor(W, T, T, T, Lsr, 1),
                mvn(W, T, T),
                and_imm(W, T, T, 1),
            ]),
        }
    }

    fn shift_bit(&mut self, r: FlagRecipe, bit: u32) {
        let (kind, width, a, b, prior) = (r.kind, r.width, r.a, r.b, r.prior);
        use FlagKind::*;
        let bits = u32::from(width) * 8;
        let old = self.asm.label();
        let done = self.asm.label();
        let count = self.gp_at(b, U);
        self.asm.emit(and_imm(W, U, count, 31));
        self.asm.cbz(W, false, U, old);
        if bit == 11 {
            self.asm.emit(cmp_imm(W, U, 1, false));
            self.asm.b_cond(Cond::Ne, old);
        }
        if bit == 0 && kind != Sar {
            self.asm.emit(cmp_imm(W, U, bits, false));
            self.asm.b_cond(Cond::Hi, old);
        }
        let ar = self.gp_at(a, T);
        self.asm.emit(ubfx(W, T, ar, 0, bits));
        match bit {
            0 => {
                if kind == Sar {
                    let small = self.asm.label();
                    self.asm.emit(cmp_imm(W, U, bits, false));
                    self.asm.b_cond(Cond::Lo, small);
                    self.asm.emit(ubfx(W, T, T, bits - 1, 1));
                    self.asm.b(done);
                    self.asm.bind(small);
                }
                if kind == Shl {
                    self.asm
                        .emit_all(&[sub(W, U, Reg::ZR, U, Lsl, 0), add_imm(W, U, U, bits, false)]);
                } else {
                    self.asm.emit(sub_imm(W, U, U, 1, false));
                }
                self.asm
                    .emit_all(&[shiftv(X, Lsr, T, T, U), and_imm(W, T, T, 1)]);
            }
            11 => match kind {
                Shl => self
                    .asm
                    .emit_all(&[eor(W, T, T, T, Lsr, 1), ubfx(W, T, T, bits - 2, 1)]),
                Shr => self.asm.emit(ubfx(W, T, T, bits - 1, 1)),
                _ => self.asm.emit(movz(W, T, 0, 0)),
            },
            _ => {
                if kind == Sar {
                    self.asm.emit(sbfx(X, T, T, 0, bits));
                }
                self.asm.emit(shiftv(
                    X,
                    match kind {
                        Shl => Lsl,
                        Shr => Lsr,
                        _ => Asr,
                    },
                    T,
                    T,
                    U,
                ));
                self.result_bit(bits, bit);
            }
        }
        self.asm.b(done);
        self.asm.bind(old);
        self.prior_bit(prior, bit);
        self.asm.bind(done);
    }
}
