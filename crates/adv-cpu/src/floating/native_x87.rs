//! Bounded native x87 batches. All preparation and validation happen before the
//! host FPU is borrowed, and the entire borrow is one inline-assembly statement.
use crate::{CpuState, EXEC, Memory, PAGE_SIZE, READ, WRITE, X87Register, X87State};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register as R};

const MAX_OPERATIONS: usize = 32;

#[derive(Clone, Copy)]
pub(crate) struct Prepared {
    kind: u8,
    width: u8,
    displacement: u32,
    base: Option<usize>,
    index: Option<usize>,
    scale: u32,
    fs: bool,
    pub(crate) pc: u32,
    next: u32,
    opcode: u16,
}

impl Prepared {
    pub(crate) fn decode(i: &Instruction, opcode: Option<u16>) -> Option<Self> {
        if i.op0_kind() != OpKind::Memory || i.has_lock_prefix() || i.segment_prefix() == R::GS {
            return None;
        }
        let width = i.memory_size().size();
        let kind = match (i.mnemonic(), width) {
            (M::Fld, 4) => 0,
            (M::Fld, 8) => 1,
            (M::Fmul, 4) => 2,
            (M::Fmul, 8) => 3,
            (M::Fadd, 4) => 4,
            (M::Fadd, 8) => 5,
            (M::Fst, 4) => 6,
            (M::Fst, 8) => 7,
            (M::Fstp, 4) => 8,
            (M::Fstp, 8) => 9,
            (M::Fistp, 4) => 10,
            _ => return None,
        };
        let register = |r: R| {
            if r == R::None {
                Some(None)
            } else if (R::EAX as u32..=R::EDI as u32).contains(&(r as u32)) {
                Some(Some(r as usize - R::EAX as usize))
            } else {
                None
            }
        };
        Some(Self {
            kind,
            width: width as u8,
            displacement: i.memory_displacement32(),
            base: register(i.memory_base())?,
            index: register(i.memory_index())?,
            scale: i.memory_index_scale(),
            fs: i.segment_prefix() == R::FS,
            pc: i.ip32(),
            next: i.next_ip32(),
            opcode: opcode?,
        })
    }

    fn address(self, state: &CpuState) -> u32 {
        self.displacement
            .wrapping_add(self.base.map_or(0, |slot| state.regs[slot]))
            .wrapping_add(
                self.index
                    .map_or(0, |slot| state.regs[slot].wrapping_mul(self.scale)),
            )
            .wrapping_add(if self.fs { state.fs_base } else { 0 })
    }

    fn writes(self) -> bool {
        self.kind >= 6
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Operation {
    kind: u64,
    address: usize,
}

#[derive(Clone, Copy, Default)]
struct Backup {
    bytes: [u8; 8],
    address: usize,
    len: usize,
    guest_address: u32,
    floating: bool,
}

#[repr(align(16))]
struct Environment([u8; 108]);

impl Environment {
    fn encode(f: &X87State) -> Self {
        let mut bytes = [0; 108];
        bytes[0..2].copy_from_slice(&f.control.to_le_bytes());
        bytes[4..6].copy_from_slice(&f.status.to_le_bytes());
        bytes[8..10].copy_from_slice(&f.tag.to_le_bytes());
        for logical in 0..8 {
            bytes[28 + logical * 10..38 + logical * 10]
                .copy_from_slice(&f.regs[f.physical_index(logical)].0);
        }
        Self(bytes)
    }

    fn status(&self) -> u16 {
        u16::from_le_bytes(self.0[4..6].try_into().unwrap())
    }

    fn decode(&self, original: &X87State) -> X87State {
        let mut out = original.clone();
        out.status = self.status();
        out.tag = u16::from_le_bytes(self.0[8..10].try_into().unwrap());
        for logical in 0..8 {
            let slot = out.physical_index(logical);
            out.regs[slot] = X87Register(
                self.0[28 + logical * 10..38 + logical * 10]
                    .try_into()
                    .unwrap(),
            );
        }
        out
    }
}

fn ordinary(f: &X87State) -> bool {
    (0..8).all(|slot| {
        f.physical_tag(slot) == 3
            || f.physical_tag(slot) == f.regs[slot].tag() && f.regs[slot].tag() <= 1
    })
}

fn ordinary_memory(bytes: &[u8]) -> bool {
    if bytes.len() == 4 {
        let bits = u32::from_le_bytes(bytes.try_into().unwrap());
        let exponent = bits & 0x7f80_0000;
        exponent != 0x7f80_0000 && (exponent != 0 || bits & 0x007f_ffff == 0)
    } else {
        let bits = u64::from_le_bytes(bytes.try_into().unwrap());
        let exponent = bits & 0x7ff0_0000_0000_0000;
        exponent != 0x7ff0_0000_0000_0000 && (exponent != 0 || bits & 0x000f_ffff_ffff_ffff == 0)
    }
}

/// Return None without changing CPU or memory when this prefix needs the
/// portable executor. Precision is the only accepted sticky/new exception.
pub(crate) fn execute(
    instructions: &[Prepared],
    state: &mut CpuState,
    memory: &mut Memory,
    max_units: u32,
    stop: u32,
    boundary: Option<u32>,
) -> Option<u32> {
    let f = &state.x87;
    if state.mmx_mode != 0
        || state.eflags & 0x100 != 0
        || f.control & 0x3f != 0x3f
        || f.control & 0x300 == 0x100
        || f.status & 0x80df != 0
        || !ordinary(f)
    {
        return None;
    }
    let count = instructions
        .iter()
        .take(MAX_OPERATIONS.min(max_units as usize))
        .take_while(|i| i.pc != stop && Some(i.pc) != boundary)
        .count();
    if count < 3 || instructions[0].pc != state.eip {
        return None;
    }
    let mut operations = [Operation::default(); MAX_OPERATIONS];
    let mut backups = [Backup::default(); MAX_OPERATIONS];
    let mut stores = 0;
    let mut last_address = 0;
    let mut top = f.top();
    let mut occupied = std::array::from_fn::<_, 8, _>(|slot| f.physical_tag(slot) != 3);
    for (index, i) in instructions[..count].iter().copied().enumerate() {
        if i.kind <= 1 {
            top = top.wrapping_sub(1) & 7;
            if occupied[top] {
                return None;
            }
            occupied[top] = true;
        } else {
            if !occupied[top] {
                return None;
            }
            if i.kind >= 8 {
                occupied[top] = false;
                top = (top + 1) & 7;
            }
        }
        let address = i.address(state);
        let width = i.width as usize;
        if memory.shared_range(address, width) {
            return None;
        }
        let range = memory.committed(address, width).ok()?;
        let permissions = &memory.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE];
        if permissions
            .iter()
            .any(|p| p & if i.writes() { WRITE } else { READ } == 0 || i.writes() && p & EXEC != 0)
        {
            return None;
        }
        // SAFETY: the entire mapped range was checked above. The exclusive
        // Memory borrow prevents mapping or permission changes until completion.
        let pointer = unsafe { memory.ptr.as_ptr().add(range.start) };
        let bytes = memory.read(address, width).ok()?;
        if i.writes() {
            let mut saved = Backup {
                address: pointer as usize,
                len: width,
                guest_address: address,
                floating: i.kind != 10,
                ..Backup::default()
            };
            saved.bytes[..width].copy_from_slice(bytes);
            backups[stores] = saved;
            stores += 1;
        } else {
            // FSTP temp; FLD temp is a normal decoder idiom. The later FLD reads
            // the actual native store, not an eagerly copied original operand.
            let forwarded = backups[..stores]
                .iter()
                .rev()
                .find(|store| store.guest_address == address && store.len == width);
            if !forwarded.is_some_and(|store| store.floating) && !ordinary_memory(bytes) {
                return None;
            }
        }
        operations[index] = Operation {
            kind: i.kind as u64,
            address: pointer as usize,
        };
        last_address = address;
    }
    let mut guest = Environment::encode(f);
    run(&operations[..count], &mut guest);
    let mut result = guest.decode(f);
    if guest.status() & 0x80df != 0 || !ordinary(&result) {
        for saved in &backups[..stores] {
            // SAFETY: these are the same exclusively borrowed, prevalidated
            // writable non-executable ranges. Restore every speculative store.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    saved.bytes.as_ptr(),
                    saved.address as *mut u8,
                    saved.len,
                )
            };
        }
        return None;
    }
    let last = instructions[count - 1];
    result.ip = last.pc;
    result.dp = last_address;
    result.opcode = last.opcode;
    state.x87 = result;
    state.eip = last.next;
    state.refresh_mmx();
    Some(count as u32)
}

fn run(operations: &[Operation], guest: &mut Environment) {
    #[repr(align(16))]
    struct Host([u8; 512]);
    let mut host = Host([0; 512]);
    #[cfg(test)]
    let mut verified = Host([0; 512]);
    #[cfg(test)]
    let verified_ptr = verified.0.as_mut_ptr();
    #[cfg(not(test))]
    let verified_ptr = host.0.as_mut_ptr();
    // SAFETY: nonempty bounded operations have live, mapped operands, masked
    // exceptions and a valid stack. There are no calls, returns or Rust code
    // while guest x87 state is loaded. FNSAVE records full physical tags/raw80;
    // FXSAVE64/FXRSTOR64 preserve the complete host x87/SSE state and 64-bit FIP.
    // No XMM register or MXCSR is changed inside this single asm statement.
    unsafe {
        std::arch::asm!(
            "fxsave64 [{host}]", "fninit", "frstor [{guest}]",
            "2:", "mov eax, dword ptr [{ops}]", "mov rdx, qword ptr [{ops} + 8]",
            "cmp eax, 0", "je 3f", "cmp eax, 1", "je 4f",
            "cmp eax, 2", "je 5f", "cmp eax, 3", "je 6f",
            "cmp eax, 4", "je 7f", "cmp eax, 5", "je 8f",
            "cmp eax, 6", "je 9f", "cmp eax, 7", "je 20f",
            "cmp eax, 8", "je 21f", "cmp eax, 9", "je 12f",
            "fistp dword ptr [rdx]", "jmp 13f",
            "3:", "fld dword ptr [rdx]", "jmp 13f",
            "4:", "fld qword ptr [rdx]", "jmp 13f",
            "5:", "fmul dword ptr [rdx]", "jmp 13f",
            "6:", "fmul qword ptr [rdx]", "jmp 13f",
            "7:", "fadd dword ptr [rdx]", "jmp 13f",
            "8:", "fadd qword ptr [rdx]", "jmp 13f",
            "9:", "fst dword ptr [rdx]", "jmp 13f",
            "20:", "fst qword ptr [rdx]", "jmp 13f",
            "21:", "fstp dword ptr [rdx]", "jmp 13f",
            "12:", "fstp qword ptr [rdx]",
            "13:", "add {ops}, 16", "dec {count}", "jnz 2b",
            "fnsave [{guest}]", "fxrstor64 [{host}]",
            ".if {check_host}", "fxsave64 [{verified}]", ".endif",
            host = in(reg) host.0.as_mut_ptr(), guest = in(reg) guest.0.as_mut_ptr(),
            verified = in(reg) verified_ptr, check_host = const cfg!(test) as u8,
            ops = inout(reg) operations.as_ptr() => _, count = inout(reg) operations.len() => _,
            out("eax") _, out("rdx") _, options(nostack),
        );
    }
    #[cfg(test)]
    assert_eq!(
        host.0, verified.0,
        "native batch must restore the complete host FP/SIMD image"
    );
}

#[cfg(test)]
mod tests;
