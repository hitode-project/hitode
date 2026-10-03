//! Reconstruct Windows x64 unwind data from finalized Cranelift prologues.
//! The fixed-RSP recipe describes the actual stack allocation and every saved
//! nonvolatile register, avoiding Cranelift 0.125's unscaled frame offset.
//! See https://learn.microsoft.com/en-us/cpp/build/exception-handling-x64
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

fn saved_gpr(register: Register) -> Option<u8> {
    Some(match register {
        Register::RBX => 3,
        Register::RBP => 5,
        Register::RSI => 6,
        Register::RDI => 7,
        Register::R12 => 12,
        Register::R13 => 13,
        Register::R14 => 14,
        Register::R15 => 15,
        _ => return None,
    })
}

fn stack_offset(instruction: &Instruction) -> Option<u32> {
    if instruction.op0_kind() != OpKind::Memory
        || instruction.memory_base() != Register::RSP
        || instruction.memory_index() != Register::None
    {
        return None;
    }
    u32::try_from(instruction.memory_displacement64()).ok()
}

fn word(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend(value.to_le_bytes());
}

/// Requires a complete, fixed-stack prologue; unrecognized setup is rejected.
/// `compiler_end` is the compiler's last setup marker, used only to prove that
/// the decoder did not mistake an unsupported setup instruction for the body.
pub(super) fn recipe(code: &[u8], compiler_end: u8) -> Result<Vec<u8>, String> {
    let mut decoder = Decoder::new(64, code, DecoderOptions::NONE);
    let push = decoder.decode();
    let frame = decoder.decode();
    if push.mnemonic() != Mnemonic::Push
        || push.op0_register() != Register::RBP
        || frame.mnemonic() != Mnemonic::Mov
        || frame.op0_register() != Register::RBP
        || frame.op1_register() != Register::RSP
    {
        return Err("native unwind requires a verified frame pointer prologue".into());
    }
    let mut records = vec![vec![push.next_ip() as u8, 0x50]];
    let mut end = frame.next_ip() as usize;
    let mut stack = 0u32;
    while decoder.can_decode() {
        let instruction = decoder.decode();
        let offset = u8::try_from(instruction.next_ip())
            .map_err(|_| "native unwind prologue exceeds 255 bytes")?;
        let mut record = vec![offset];
        if instruction.mnemonic() == Mnemonic::Sub && instruction.op0_register() == Register::RSP {
            if !matches!(
                instruction.op1_kind(),
                OpKind::Immediate8to64 | OpKind::Immediate32to64
            ) {
                return Err("dynamic native stack allocation is not supported".into());
            }
            let amount = u32::try_from(instruction.immediate(1))
                .map_err(|_| "invalid native stack allocation")?;
            if amount == 0 || amount % 8 != 0 {
                return Err("unaligned native stack allocation".into());
            }
            stack = stack
                .checked_add(amount)
                .ok_or("native stack allocation overflow")?;
            if amount <= 128 {
                record.push(2 | (((amount - 8) / 8) as u8) << 4);
            } else if amount / 8 <= u16::MAX.into() {
                record.push(1);
                word(&mut record, (amount / 8) as u16);
            } else {
                record.push(0x11);
                record.extend(amount.to_le_bytes());
            }
        } else if let Some(at) = stack_offset(&instruction) {
            let register = instruction.op1_register();
            let (op, scale, size, index) = if instruction.mnemonic() == Mnemonic::Mov {
                let Some(index) = saved_gpr(register) else {
                    break;
                };
                (4, 8, 8, index)
            } else if matches!(
                instruction.mnemonic(),
                Mnemonic::Movaps
                    | Mnemonic::Movups
                    | Mnemonic::Movdqa
                    | Mnemonic::Movdqu
                    | Mnemonic::Vmovaps
                    | Mnemonic::Vmovups
                    | Mnemonic::Vmovdqa
                    | Mnemonic::Vmovdqu
            ) && (Register::XMM6 as u32..=Register::XMM15 as u32)
                .contains(&(register as u32))
            {
                (8, 16, 16, (register as u32 - Register::XMM0 as u32) as u8)
            } else {
                break;
            };
            if at % scale != 0 || at.checked_add(size).is_none_or(|limit| limit > stack) {
                return Err("native register save is outside its allocated stack".into());
            }
            if at / scale <= u16::MAX.into() {
                record.push(op | index << 4);
                word(&mut record, (at / scale) as u16);
            } else {
                record.push((op + 1) | index << 4);
                record.extend(at.to_le_bytes());
            }
        } else {
            break;
        };
        end = instruction.next_ip() as usize;
        records.push(record);
    }
    if end < usize::from(compiler_end) {
        return Err("unsupported instruction in native unwind prologue".into());
    }
    let slots: usize = records.iter().map(|record| record.len() / 2).sum();
    let mut result = vec![
        1,
        end as u8,
        u8::try_from(slots).map_err(|_| "native unwind has too many slots")?,
        0,
    ];
    for record in records.into_iter().rev() {
        result.extend(record);
    }
    if !slots.is_multiple_of(2) {
        word(&mut result, 0);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_stack_and_gpr_xmm_saves_use_actual_scaled_offsets() {
        // push rbp; mov rbp,rsp; sub rsp,32; mov [rsp],rsi;
        // movdqu [rsp+16],xmm6; mov eax,1 (body).
        let code = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0x48, 0x89, 0x34, 0x24, 0xf3, 0x0f,
            0x7f, 0x74, 0x24, 0x10, 0xb8, 1, 0, 0, 0,
        ];
        assert_eq!(
            recipe(&code, 18).unwrap(),
            [
                1, 18, 6, 0, 18, 0x68, 1, 0, 12, 0x64, 0, 0, 8, 0x32, 1, 0x50
            ]
        );
        assert!(recipe(&code, 19).is_err());
    }
    #[test]
    fn unsupported_frame_relocation_and_out_of_frame_save_are_rejected() {
        let relocated = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0x48, 0x89, 0xe5,
        ];
        assert!(recipe(&relocated, 11).is_err());
        let outside = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x08, 0x48, 0x89, 0x74, 0x24, 0x10,
        ];
        assert!(recipe(&outside, 13).is_err());
    }
}
