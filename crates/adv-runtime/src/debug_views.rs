//! Read-only views of the runtime for the interactive debugger.
//! They only read guest state, so they are safe at any guest boundary.
use adv_cpu::Engine;
use adv_cpu::backend::{Backend, Guest, GuestExt, Reg};

/// Stack words a scan reads at most.
const STACK_SCAN_WORDS: u32 = 16384;

/// Stack-specific observations used by the shared live debugger. Reads happen
/// on the execution owner at guest boundaries.
pub trait DebugTarget {
    type Backend: Backend;
    fn machine(&self) -> &adv_machine::Machine<Self::Backend>;
    fn guest_place(&self, address: u32) -> String;
    fn modules_report(&self) -> String;
    fn image_ranges(&self) -> Result<Vec<(u32, u32)>, String>;
    /// Whether historical samples can be assigned to this process.
    fn profile_process_known(&self) -> bool {
        true
    }
    fn profile_place(&self, address: u32) -> String {
        self.guest_place(address)
    }
    fn calls_report(&self, count: usize) -> String {
        self.machine().sys.recent_calls_tail(count).join("\n")
    }
    fn debug_counters(&self) -> [u64; 8]
    where
        Self::Backend: adv_cpu::Engine,
    {
        let cpu = self.machine().cpu.counters();
        [
            self.machine().sys.host_calls,
            cpu.native_entries,
            cpu.interpreter_entries,
            cpu.retired_forms.unwrap_or_default()[0],
            cpu.retired_forms.unwrap_or_default()[1],
            cpu.retired_forms.unwrap_or_default()[2],
            cpu.retired_forms.unwrap_or_default()[3],
            cpu.retired_forms.unwrap_or_default()[4],
        ]
    }
    fn contexts_report(&self) -> String;
    fn api_gaps_report(&self) -> String;
    fn report(&self) -> String;

    /// Whether `address` follows a CALL in an image module, so a stack word
    /// holding it is a return address rather than data.
    fn after_call(&self, address: u32, images: &[(u32, u32)]) -> bool {
        let in_image = images
            .iter()
            .any(|&(base, size)| address.wrapping_sub(base) < size);
        if !in_image || address < 7 {
            return false;
        }
        let mut bytes = [0u8; 7];
        if self.machine().cpu.read(address - 7, &mut bytes).is_err() {
            return false;
        }
        // CALL rel32; CALL r/m32 (FF /2) with no, 8-bit or 32-bit displacement,
        // absolute or with a SIB byte.
        let call = |modrm: u8| modrm & 0x38 == 0x10;
        bytes[2] == 0xe8
            || (bytes[5] == 0xff && call(bytes[6]) && bytes[6] >> 6 == 3)
            || (bytes[5] == 0xff
                && call(bytes[6])
                && bytes[6] >> 6 == 0
                && bytes[6] & 7 != 4
                && bytes[6] & 7 != 5)
            || (bytes[4] == 0xff && call(bytes[5]) && bytes[5] >> 6 == 1)
            || (bytes[4] == 0xff && call(bytes[5]) && bytes[5] >> 6 == 0 && bytes[5] & 7 == 4)
            || (bytes[3] == 0xff && call(bytes[4]) && bytes[4] >> 6 == 1 && bytes[4] & 7 == 4)
            || (bytes[1] == 0xff && call(bytes[2]) && (bytes[2] >> 6 == 2 || bytes[2] & 0xc7 == 5))
            || (bytes[0] == 0xff && call(bytes[1]) && bytes[1] >> 6 == 2 && bytes[1] & 7 == 4)
    }

    /// Likely return addresses on guest thread `context`'s stack, innermost
    /// first: words after a CALL in an image module. A blocked thread is read
    /// from its parked host call; the running thread from ESP.
    fn stack_scan(&self, context: u32, frames: usize) -> Result<String, String> {
        let sys = &self.machine().sys;
        let ctx = sys
            .contexts
            .iter()
            .find(|c| c.id == context)
            .ok_or_else(|| format!("no guest context {context}"))?;
        let esp = match ctx.blocked_stack() {
            Some(esp) => esp,
            None if sys.current == context && !ctx.is_done() => self.machine().cpu.reg(Reg::Esp),
            None => return Err(format!("context {context} has no saved stack pointer")),
        };
        let top = if ctx.stack_hi > esp {
            ctx.stack_hi
        } else {
            esp.saturating_add(STACK_SCAN_WORDS * 4)
        };
        let mut rows = vec![format!(
            "context {context} esp {esp:#010x} stack [{:#010x}, {:#010x})",
            ctx.stack_lo, ctx.stack_hi
        )];
        let images = self.image_ranges()?;
        let mut address = esp;
        while address < top && rows.len() <= frames && address - esp < STACK_SCAN_WORDS * 4 {
            let Ok(word) = self.machine().cpu.read_u32(address) else {
                rows.push(format!("  esp+{:#06x} <unmapped>", address - esp));
                break;
            };
            if self.after_call(word, &images) {
                rows.push(format!(
                    "  esp+{:#06x} {word:#010x} {}",
                    address - esp,
                    self.guest_place(word)
                ));
            }
            address += 4;
        }
        Ok(rows.join("\n"))
    }

    /// Hex dump of up to 4 KiB of guest memory.
    fn memory_dump(&self, address: u32, len: u32) -> Result<String, String> {
        if len == 0 || len > 4096 || u64::from(address) + u64::from(len) > 1 << 32 {
            return Err("memory range must be 1..=4096 bytes inside guest address space".into());
        }
        let mut rows = Vec::new();
        for line in (0..len).step_by(16) {
            let at = address + line;
            let mut bytes = vec![0u8; (len - line).min(16) as usize];
            match self.machine().cpu.read(at, &mut bytes) {
                Ok(()) => {
                    let hex: Vec<_> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    let text: String = bytes
                        .iter()
                        .map(|&b| {
                            if (0x20..0x7f).contains(&b) {
                                b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    rows.push(format!("{at:08x}  {:<47}  {text}", hex.join(" ")));
                }
                Err(_) => rows.push(format!("{at:08x}  <unmapped>")),
            }
        }
        Ok(rows.join("\n"))
    }

    /// Up to 64 guest instructions from `address`, by iced form.
    fn disassemble(&self, address: u32, count: usize) -> Result<String, String> {
        let mut bytes = vec![0u8; count.clamp(1, 64) * 15];
        let mut readable = bytes.len();
        while readable > 0
            && self
                .machine()
                .cpu
                .read(address, &mut bytes[..readable])
                .is_err()
        {
            readable = readable.saturating_sub(0x100.min(readable));
        }
        if readable == 0 {
            return Err(format!("{address:#010x} is unmapped"));
        }
        let mut decoder = iced_x86::Decoder::with_ip(
            32,
            &bytes[..readable],
            u64::from(address),
            iced_x86::DecoderOptions::NONE,
        );
        let mut rows = vec![self.guest_place(address)];
        for instruction in decoder.iter().take(count.clamp(1, 64)) {
            let at = instruction.ip() as u32;
            let start = (at - address) as usize;
            let hex: Vec<_> = bytes[start..start + instruction.len()]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let target = match instruction.near_branch_target() {
                0 => String::new(),
                target => format!(" -> {}", self.guest_place(target as u32)),
            };
            rows.push(format!(
                "{at:08x}  {:<30} {:?}{target}",
                hex.join(" "),
                instruction.code()
            ));
        }
        Ok(rows.join("\n"))
    }
}
