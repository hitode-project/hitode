//! Tier-1 guest addresses and access sites.
use crate::asm::{Asm, Label};
use crate::encode::Extend::Uxtw;
use crate::encode::Index::{Post, Pre};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, ACCESS_STORE};

use super::{Cold, RUNTIME, SHADOW, Translator, WINDOW, X3, X5, biased, host};
use crate::translate::front::decode::{ESP, Mem};
use crate::translate::lower::access::{
    Addr, CheckedEmitter, Dir, Value, check_class, checked_alias, page_crossing,
};

impl Translator<'_> {
    #[cfg(native)]
    pub(super) fn load_frame_cache(&mut self, offsets: &[i32]) {
        for (slot, &offset) in offsets.iter().enumerate() {
            let address = self.effective(Mem {
                base: Some(ESP),
                index: None,
                scale: 1,
                address_bits: 32,
                disp: offset as u32,
                fs: false,
            });
            // Admission already granted the original four bytes through
            // this alias. Hoisting does not add a guest fault or effect.
            self.emit(ldr_reg(W, super::X16, self.data_view(), address, false));
            let (reg, lane) = frame_lane(slot as u8);
            self.emit(vins_general(2, reg, lane, super::X16));
        }
    }

    /// The worker's unused shadow register holds its admitted private alias.
    pub(super) fn data_view(&self) -> Reg {
        if self.computation { SHADOW } else { WINDOW }
    }

    /// An address for a 32-bit access to `mem`; a checked one is always in
    /// a register.
    pub(super) fn address(&mut self, mem: Mem) -> Addr {
        #[cfg(native)]
        if let Some(slot) = self
            .computation_facts
            .and_then(|facts| facts.frame_cache)
            .and_then(|cache| cache.read(self.index))
        {
            return Addr::FrameCache(slot);
        }
        if let (Some(base), None) = (mem.base, mem.index)
            && biased(base)
            && !self.checking
            && !self.computation
            && mem.address_bits == 32
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
        #[cfg(native)]
        if let Some(slot) = self
            .computation_facts
            .and_then(|facts| facts.scope_access)
            .and_then(|access| access[self.index])
        {
            let offset = runtime::SCOPE_FRAME + 4 * u32::from(slot);
            self.emit(if store {
                str(W, rt, RUNTIME, offset)
            } else {
                ldr(W, rt, RUNTIME, offset)
            });
            if matches!(addr, Addr::Push | Addr::Pop) {
                let esp = host(ESP);
                self.emit(if matches!(addr, Addr::Push) {
                    sub_imm(W, X5, esp, 4, false)
                } else {
                    add_imm(W, X5, esp, 4, false)
                });
                self.emit(add_ext(X, esp, WINDOW, X5, Uxtw, 0));
            }
            return;
        }
        match addr {
            #[cfg(native)]
            Addr::FrameCache(slot) => {
                let (reg, lane) = frame_lane(slot);
                self.emit(vumov(2, rt, reg, lane));
            }
            Addr::Push => {
                assert!(store && width == 4, "a push stores 4 bytes");
                if self.checking || self.computation {
                    // ESP wraps like the guest's, and stays biased.
                    self.emit(sub_imm(W, X5, host(ESP), 4, false));
                    self.access(Addr::Window(X5), width, dir, rt);
                    self.emit(add_ext(X, host(ESP), WINDOW, X5, Uxtw, 0));
                } else {
                    self.site();
                    self.emit(str_indexed(W, Pre, rt, host(ESP), -4));
                }
            }
            Addr::Pop => {
                assert!(!store && width == 4, "a pop loads 4 bytes");
                if self.checking || self.computation {
                    self.access(Addr::Window(host(ESP)), width, dir, rt);
                    self.emit(add_imm(X, host(ESP), host(ESP), 4, false));
                } else {
                    self.site();
                    self.emit(ldr_indexed(W, Post, rt, host(ESP), 4));
                }
            }
            Addr::Window(address) | Addr::Biased(address, 0) if self.checking => {
                self.checked(
                    address,
                    rt,
                    dir.kind(width),
                    dir.window(width, rt, WINDOW, address),
                );
            }
            Addr::Window(address) => {
                self.site();
                self.emit(dir.window(width, rt, self.data_view(), address));
            }
            Addr::Biased(base, disp) if self.computation => {
                let address = if disp == 0 {
                    base
                } else {
                    self.mov_imm(X3, disp as u32);
                    self.emit(add(W, X3, base, X3, Lsl, 0));
                    X3
                };
                self.access(Addr::Window(address), width, dir, rt);
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

    /// A checked access (A64-MEM-7) of `kind` at the guest address in `address`,
    /// whose window form is `direct`: the window takes it if it stays in
    /// one page whose class allows it. Ordinary pages blocked by the host
    /// page intersection use the alias after a raw guest permission check.
    /// The remaining cases use `access`. Uses x3 and x4.
    fn checked(&mut self, address: Reg, rt: Reg, kind: u32, direct: u32) {
        let slow = self.asm.label();
        let (site, resume) = self.checked_memory(
            address,
            kind & 31,
            kind & ACCESS_STORE != 0,
            direct,
            slow,
            slow,
        );
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
            vstr_reg(size, value, self.data_view(), address)
        } else {
            vldr_reg(size, value, self.data_view(), address)
        };
        if self.checking {
            let slow = self.asm.label();
            let (site, resume) =
                self.checked_memory(address, size.bytes(), store, direct, slow, slow);
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

    /// Branch when the last byte enters another page, preserving NZCV.
    pub(super) fn page_crossing(&mut self, address: Reg, width: u32, crossing: Label) {
        page_crossing(self.asm, address, width, crossing);
    }

    /// Select the window or alias for one page. Uses x3 and x4, preserving NZCV.
    pub(super) fn checked_base(
        &mut self,
        address: Reg,
        store: bool,
        page: bool,
        base: Reg,
        slow: Label,
    ) {
        let (alias, done) = (self.asm.label(), self.asm.label());
        check_class(self.asm, address, runtime::CLASSES, store, alias, slow);
        self.emit(mov(X, base, WINDOW));
        self.asm.b(done);
        self.asm.bind(alias);
        self.checked_alias(address, store, page, slow);
        self.emit(ldr(X, base, RUNTIME, runtime::ALIAS));
        self.asm.bind(done);
    }

    pub(super) fn checked_alias(&mut self, address: Reg, store: bool, page: bool, slow: Label) {
        checked_alias(self.asm, address, store, page, slow);
    }
}

impl CheckedEmitter for Translator<'_> {
    fn access_asm(&mut self) -> &mut Asm {
        self.asm
    }

    fn access_site(&mut self) {
        self.site();
    }
}

#[cfg(native)]
fn frame_lane(slot: u8) -> (VReg, u32) {
    // Detached units exclude MMX instructions. Preserve each low D lane,
    // which the host ABI saves and restores even in this form.
    (VReg::new(8 + slot / 2), 2 + u32::from(slot % 2))
}
