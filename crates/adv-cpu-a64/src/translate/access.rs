//! The access emitter (A64-MEM-7): integer and SIMD code describe each guest data
//! access, and this picks its form. An unchecked access records a site and
//! uses the window or a biased stack register. A checked one tests the page
//! class inline and leaves the line only for pages the window cannot take;
//! a refused access faults as a host fault at its site would, so dirty SIMD
//! registers need no publication first.

use crate::asm::Label;
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{ESP, Mem};
use super::{Cold, RUNTIME, Translator, WINDOW, X3, X4, X5, biased, host};
use crate::runtime::{self, ACCESS_SIGNED, ACCESS_STORE};
use adv_cpu_core::memory::{EXEC, READ, SHARED, WRITE};

pub(super) const PAGE_SHIFT: u32 = 12;

/// An address operand ready for a load or store.
#[derive(Clone, Copy)]
pub(super) enum Addr {
    /// A W register holding the guest address: `[x27, w, uxtw]`.
    Window(Reg),
    /// A biased stack register plus an offset the guards cover.
    Biased(Reg, i32),
    /// The slot below ESP, which a store pushes to.
    Push,
    /// The slot at ESP, which a load pops from.
    Pop,
}

/// What an access does with its register.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Dir {
    /// Zero-extends the bytes read.
    Load,
    /// Sign-extends the bytes read.
    Signed,
    Store,
}

/// The register a checked access transfers through the stub.
pub(super) enum Value {
    Scalar(Reg),
    Vector(VMem, VReg),
}

impl Dir {
    /// The `access` kind bits of a `width`-byte access.
    fn kind(self, width: u8) -> u32 {
        u32::from(width)
            | match self {
                Self::Load => 0,
                Self::Signed => ACCESS_SIGNED,
                Self::Store => ACCESS_STORE,
            }
    }

    /// `[x27, address, uxtw]` for a `width`-byte access of `rt`.
    fn window(self, width: u8, rt: Reg, address: Reg) -> u32 {
        let narrow = |half| match (half, self) {
            (false, Self::Signed) => Narrow::SignedByte,
            (false, _) => Narrow::Byte,
            (true, Self::Signed) => Narrow::SignedHalf,
            (true, _) => Narrow::Half,
        };
        match (width, self) {
            (1 | 2, Self::Store) => str_narrow(narrow(width == 2), rt, WINDOW, address),
            (1 | 2, _) => ldr_narrow(narrow(width == 2), rt, WINDOW, address),
            (4, Self::Store) => str_reg(W, rt, WINDOW, address, false),
            (4, _) => ldr_reg(W, rt, WINDOW, address, false),
            _ => unreachable!("no {width}-byte integer access"),
        }
    }
}

impl Translator<'_> {
    /// An address for a 32-bit access to `mem`; a checked one is always in
    /// a register.
    pub(super) fn address(&mut self, mem: Mem) -> Addr {
        if let (Some(base), None) = (mem.base, mem.index)
            && biased(base)
            && !self.checking
            && !mem.fs
        {
            // Offsets well inside the guards: an access past 0 or 4 GiB
            // faults there and the interpreter wraps it exactly.
            let disp = mem.disp as i32;
            if (0..16384).contains(&disp) && disp % 4 == 0 || (-256..256).contains(&disp) {
                return Addr::Biased(host(base), disp);
            }
        }
        Addr::Window(self.effective(mem))
    }

    pub(super) fn load(&mut self, addr: Addr, rt: Reg) {
        self.access(addr, 4, Dir::Load, rt);
    }

    pub(super) fn store(&mut self, addr: Addr, rt: Reg) {
        self.access(addr, 4, Dir::Store, rt);
    }

    /// A `width`-byte access between `rt` and `addr`. Checked biased
    /// addresses take no offset; pushes and pops move ESP. Uses x3 to x5.
    pub(super) fn access(&mut self, addr: Addr, width: u8, dir: Dir, rt: Reg) {
        let store = dir == Dir::Store;
        match addr {
            Addr::Push => {
                assert!(store && width == 4, "a push stores 4 bytes");
                if self.checking {
                    // ESP wraps like the guest's, and stays biased.
                    self.emit(sub_imm(W, X5, host(ESP), 4, false));
                    self.checked(X5, rt, dir.kind(4), dir.window(4, rt, X5));
                    self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
                } else {
                    self.site();
                    self.emit(str_indexed(W, Pre, rt, host(ESP), -4));
                }
            }
            Addr::Pop => {
                assert!(!store && width == 4, "a pop loads 4 bytes");
                if self.checking {
                    self.checked(host(ESP), rt, dir.kind(4), dir.window(4, rt, host(ESP)));
                    self.emit(add_imm(X, host(ESP), host(ESP), 4, false));
                } else {
                    self.site();
                    self.emit(ldr_indexed(W, Post, rt, host(ESP), 4));
                }
            }
            Addr::Window(address) | Addr::Biased(address, 0) if self.checking => {
                self.checked(address, rt, dir.kind(width), dir.window(width, rt, address));
            }
            Addr::Window(address) => {
                self.site();
                self.emit(dir.window(width, rt, address));
            }
            Addr::Biased(base, disp) => {
                assert!(!self.checking && width == 4, "a biased access is unchecked");
                self.site();
                self.emit(match (store, disp) {
                    (false, d) if d >= 0 && d % 4 == 0 => ldr(W, rt, base, d as u32),
                    (false, d) => ldur(W, rt, base, d),
                    (true, d) if d >= 0 && d % 4 == 0 => str(W, rt, base, d as u32),
                    (true, d) => stur(W, rt, base, d),
                });
            }
        }
    }

    /// `direct`, an access of `width` bytes at the guest address in
    /// `address`, or a branch to `slow` where a checked one cannot take the
    /// window or alias. The caller's slow path completes the instruction.
    pub(super) fn access_or(
        &mut self,
        address: Reg,
        width: u32,
        store: bool,
        direct: u32,
        slow: Label,
    ) {
        if self.checking {
            self.checked_memory(address, width, store, direct, slow);
        } else {
            self.site();
            self.emit(direct);
        }
    }

    /// A checked access (A64-MEM-7) of `kind` at the guest address in `address`,
    /// whose window form is `direct`: the window takes it if it stays in
    /// one page whose class allows it. Ordinary pages blocked by the host
    /// page intersection use the alias after a raw guest permission check.
    /// The remaining cases use `access`. Uses x3 and x4.
    fn checked(&mut self, address: Reg, rt: Reg, kind: u32, direct: u32) {
        let slow = self.asm.label();
        let (site, resume) =
            self.checked_memory(address, kind & 31, kind & ACCESS_STORE != 0, direct, slow);
        self.push_cold(Cold::Access {
            label: slow,
            resume,
            site,
            address,
            value: Value::Scalar(rt),
            kind,
        });
    }

    /// A vector access using the same sites and permission checks as integers.
    pub(super) fn vector_access(&mut self, size: VMem, value: VReg, address: Reg, store: bool) {
        let direct = if store {
            vstr_reg(size, value, WINDOW, address)
        } else {
            vldr_reg(size, value, WINDOW, address)
        };
        if self.checking {
            let slow = self.asm.label();
            let (site, resume) = self.checked_memory(address, size.bytes(), store, direct, slow);
            self.push_cold(Cold::Access {
                label: slow,
                resume,
                site,
                address,
                value: Value::Vector(size, value),
                kind: size.bytes() | if store { ACCESS_STORE } else { 0 },
            });
        } else {
            self.site();
            self.emit(direct);
        }
    }

    /// Emit one scalar or vector access after the same window/alias checks.
    /// The caller owns the slow path.
    pub(super) fn checked_memory(
        &mut self,
        address: Reg,
        width: u32,
        store: bool,
        direct: u32,
        slow: Label,
    ) -> (Label, Label) {
        let alias = self.asm.label();
        self.page_crossing(address, width, slow);
        self.check_class(address, runtime::CLASSES, store, alias, slow);
        let (site, resume) = (self.asm.label(), self.asm.label());
        self.asm.bind(site);
        self.site();
        self.emit(direct);
        self.asm.b(resume);
        self.asm.bind(alias);
        // The window class already granted the guest R/W bit, so this index
        // is inside Memory's raw table even when its base is not zero.
        self.checked_alias(address, store, slow);
        self.emit(ldr(X, X3, RUNTIME, runtime::ALIAS));
        self.site();
        // Keep the access width and extension; only replace its base register.
        self.emit((direct & !(31 << 5)) | (X3.number() << 5));
        self.asm.bind(resume);
        (site, resume)
    }

    /// Branch when the last byte enters another page, preserving NZCV.
    pub(super) fn page_crossing(&mut self, address: Reg, width: u32, crossing: Label) {
        if width > 1 {
            self.emit_all(&[
                add_imm(W, X4, address, width - 1, false),
                eor(W, X4, X4, address, Lsl, 0),
            ]);
            self.asm.tbz(true, X4, PAGE_SHIFT, crossing);
        }
    }

    /// Select the window or alias for one page. Uses x3 and x4, preserving NZCV.
    pub(super) fn checked_base(&mut self, address: Reg, store: bool, base: Reg, slow: Label) {
        let (alias, done) = (self.asm.label(), self.asm.label());
        self.check_class(address, runtime::CLASSES, store, alias, slow);
        self.emit(mov(X, base, WINDOW));
        self.asm.b(done);
        self.asm.bind(alias);
        self.checked_alias(address, store, slow);
        self.emit(ldr(X, base, RUNTIME, runtime::ALIAS));
        self.asm.bind(done);
    }

    /// Verify the alias class after the window class granted the guest permission.
    pub(super) fn checked_alias(&mut self, address: Reg, store: bool, slow: Label) {
        self.check_class(address, runtime::ALIAS_CLASSES, store, slow, slow);
    }

    /// Test guest permission and excluded classes with the same scratch and NZCV contract.
    fn check_class(&mut self, address: Reg, table: u32, store: bool, excluded: Label, slow: Label) {
        self.emit_all(&[
            ldr(X, X3, RUNTIME, table),
            lsr_imm(W, X4, address, PAGE_SHIFT),
            ldr_narrow(Narrow::Byte, X4, X3, X4),
        ]);
        // Permitted, not shared, and for a store not a code barrier.
        let bit = |permission: u8| permission.trailing_zeros();
        let (needed, blocked) = if store {
            (WRITE, &[EXEC, SHARED][..])
        } else {
            (READ, &[SHARED][..])
        };
        self.asm.tbz(false, X4, bit(needed), slow);
        for &permission in blocked {
            self.asm.tbz(true, X4, bit(permission), excluded);
        }
    }
}
