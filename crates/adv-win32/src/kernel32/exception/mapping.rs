//! Win32 codes for precise x86 CPU stops. Faults retain the stopped PC;
//! breakpoint addresses/CONTEXT follow native x86's one-byte PC adjustment.
//! Continuing execution always requires the guest handler's disposition.
//! Codes follow SDK ntstatus.h and Microsoft Learn's GetExceptionCode table.
//! An authored x86 Windows oracle (target/exception-arithmetic-native/probe.c)
//! verifies DIV zero -> 0xc0000094, nonzero unsigned/signed quotient overflow
//! -> 0xc0000095, and UD2 -> 0xc000001d, all at the original PC with flags0 and
//! no parameters. Architectural #DE is not always divide-by-zero.
use super::Record;
use adv_cpu::backend::{ArchitecturalState, FaultKind, Guest, GuestExt, Prot};
use adv_machine::exception::Exception;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind, Register};

const ACCESS_VIOLATION: u32 = 0xc000_0005;
const ILLEGAL_INSTRUCTION: u32 = 0xc000_001d;
const INTEGER_DIVIDE_BY_ZERO: u32 = 0xc000_0094;
const INTEGER_OVERFLOW: u32 = 0xc000_0095;
const BREAKPOINT: u32 = 0x8000_0003;

pub(super) fn map(
    cpu: &dyn Guest,
    event: &Exception,
    state: &ArchitecturalState,
) -> Result<Record, String> {
    let pc = match event {
        Exception::Fault(fault) => fault.pc,
        Exception::Trap { pc, .. } => *pc,
    };
    map_stop(cpu, event, state).map_err(|reason| {
        // Mapping failure is fatal: retain the original CPU stop rather than
        // losing it to the later exception-dispatch thunk PC. The existing
        // warning recorder still bounds distinct reasons and repetitions.
        format!(
            "{reason}; original CPU PC {pc:#010x}, captured EIP {:#010x}",
            state.eip
        )
    })
}

fn map_stop(
    cpu: &dyn Guest,
    event: &Exception,
    state: &ArchitecturalState,
) -> Result<Record, String> {
    let pc = match event {
        Exception::Fault(fault) => fault.pc,
        Exception::Trap { pc, .. } => *pc,
    };
    if pc != state.eip {
        return Err("CPU exception PC disagrees with captured x86 EIP".into());
    }
    let mut record = Record {
        code: 0,
        flags: 0,
        address: pc,
        count: 0,
        information: [0; 15],
    };
    record.code = match event {
        Exception::Fault(fault) => match &fault.kind {
            FaultKind::Read(address) | FaultKind::Write(address) | FaultKind::Fetch(address) => {
                record.information[0] = match fault.kind {
                    FaultKind::Read(_) => 0,
                    FaultKind::Write(_) => 1,
                    _ => 8,
                };
                record.information[1] = *address;
                record.count = 2;
                ACCESS_VIOLATION
            }
            FaultKind::InvalidInstruction => ILLEGAL_INSTRUCTION,
            FaultKind::Other(reason) => return Err(format!("unclassified CPU fault: {reason}")),
        },
        Exception::Trap { vector: 0, .. } => {
            // Windows distinguishes zero divisors from quotient overflow even
            // though both DIV and IDIV raise architectural #DE (vector0).
            // The owned x86 oracle confirms all operands remain uncommitted.
            if divisor(cpu, state)? == 0 {
                INTEGER_DIVIDE_BY_ZERO
            } else {
                INTEGER_OVERFLOW
            }
        }
        Exception::Trap { vector: 6, .. } => ILLEGAL_INSTRUCTION,
        Exception::Trap { vector: 3, .. } => {
            record.address = breakpoint_address(cpu, pc)?;
            record.count = 1; // Owned x86 native CC/CD03 oracle: Information[0]=0.
            BREAKPOINT
        }
        Exception::Trap { vector, .. } => {
            return Err(format!("unmapped x86 CPU exception vector {vector}"));
        }
    };
    Ok(record)
}

pub(super) fn normalize_context(record: &Record, state: &mut ArchitecturalState) {
    if record.code == BREAKPOINT {
        // CPU INT traps report the retired PC. Native Win32 x86 backs it up
        // exactly one byte for both CC and CD03: CD03 therefore points at03.
        // The handler must explicitly advance/edit this saved EIP to continue;
        // returning unchanged repeats CC just as on the owned native oracle.
        state.eip = record.address;
    }
}

fn breakpoint_address(cpu: &dyn Guest, pc: u32) -> Result<u32, String> {
    let last = pc
        .checked_sub(1)
        .ok_or_else(|| "x86 breakpoint PC underflow".to_owned())?;
    let byte = |address| {
        if cpu.accessible(address, 1, Prot::EXEC) {
            cpu.read_u8(address).ok()
        } else {
            None
        }
    };
    match byte(last) {
        Some(0xcc) => Ok(last),
        Some(0x03) if last.checked_sub(1).and_then(byte) == Some(0xcd) => Ok(last),
        _ => {
            Err("x86 breakpoint classification requires an executed CC or CD03 instruction".into())
        }
    }
}

fn register(state: &ArchitecturalState, register: Register) -> Option<u64> {
    let number = register as usize;
    let value = match register {
        _ if (Register::EAX as usize..=Register::EDI as usize).contains(&number) => {
            state.regs[number - Register::EAX as usize]
        }
        _ if (Register::AX as usize..=Register::DI as usize).contains(&number) => {
            state.regs[number - Register::AX as usize] & 0xffff
        }
        _ if (Register::AL as usize..=Register::BL as usize).contains(&number) => {
            state.regs[number - Register::AL as usize] & 0xff
        }
        _ if (Register::AH as usize..=Register::BH as usize).contains(&number) => {
            (state.regs[number - Register::AH as usize] >> 8) & 0xff
        }
        Register::FS => state.fs_base,
        Register::ES | Register::CS | Register::SS | Register::DS => 0,
        // GS and non-flat segment bases are outside the shared CPU profile.
        _ => return None,
    };
    Some(u64::from(value))
}

fn divisor(cpu: &dyn Guest, state: &ArchitecturalState) -> Result<u32, String> {
    // Read only available executable bytes; an instruction may end at a page
    // boundary with the following page unmapped. No 15-byte overread is needed.
    let mut bytes = [0; 15];
    let mut length = 0;
    for (index, byte) in bytes.iter_mut().enumerate() {
        let address = state.eip.wrapping_add(index as u32);
        if !cpu.accessible(address, 1, Prot::EXEC) {
            break;
        }
        let Ok(value) = cpu.read_u8(address) else {
            break;
        };
        *byte = value;
        length += 1;
    }
    let instruction = Decoder::with_ip(
        32,
        &bytes[..length],
        u64::from(state.eip),
        DecoderOptions::NONE,
    )
    .decode();
    if instruction.is_invalid() || !matches!(instruction.mnemonic(), Mnemonic::Div | Mnemonic::Idiv)
    {
        return Err(
            "x86 divide-error classification requires a readable DIV/IDIV instruction".into(),
        );
    }
    match instruction.op0_kind() {
        OpKind::Register => register(state, instruction.op0_register())
            .map(|value| value as u32)
            .ok_or_else(|| "x86 divide-error operand register is unsupported".into()),
        OpKind::Memory => {
            let address = instruction
                .virtual_address(0, 0, |r, _, _| register(state, r))
                .ok_or_else(|| "x86 divide-error operand address is unsupported".to_owned())?
                as u32;
            let size = instruction.memory_size().size();
            if !matches!(size, 1 | 2 | 4) {
                return Err("x86 divide-error operand width is unsupported".into());
            }
            let mut bytes = [0; 4];
            if !cpu.accessible(address, size as u32, Prot::READ)
                || cpu.read(address, &mut bytes[..size]).is_err()
            {
                return Err("x86 divide-error operand became unreadable".into());
            }
            Ok(u32::from_le_bytes(bytes))
        }
        _ => Err("x86 divide-error operand kind is unsupported".into()),
    }
}

#[cfg(test)]
mod breakpoint_tests;
#[cfg(test)]
mod tests;
