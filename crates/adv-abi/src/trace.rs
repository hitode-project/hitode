//! Read-only, bounded formatting with the same permission checks as typed pointers.
use crate::{AbiKind, AnsiStr, Arguments, GuestPtr, Signature, WideStr};
use adv_cpu::backend::Guest;

pub fn format_arguments(cpu: &dyn Guest, signature: &Signature) -> Vec<String> {
    let mut args = Arguments::new(cpu);
    signature
        .parameters
        .iter()
        .map(|p| {
            let value = match p.kind {
                AbiKind::U64 | AbiKind::I64 | AbiKind::F64 => {
                    args.next::<u64>().map(|v| format!("{v:#018x}"))
                }
                _ if p.words == 1 => args.next::<u32>().map(|v| match p.kind {
                    AbiKind::AnsiString if v != 0 => AnsiStr(GuestPtr::new(v))
                        .read(cpu, 256)
                        .map(|b| format!("{v:#010x} b\"{}\"", b.escape_ascii()))
                        .unwrap_or_else(|_| format!("{v:#010x} <unreadable>")),
                    AbiKind::WideString if v != 0 => WideStr(GuestPtr::new(v))
                        .read(cpu, 256)
                        .map(|u| format!("{v:#010x} {:?}", String::from_utf16_lossy(&u)))
                        .unwrap_or_else(|_| format!("{v:#010x} <unreadable>")),
                    AbiKind::Handle => format!("handle({v:#010x})"),
                    AbiKind::I32 => (v as i32).to_string(),
                    AbiKind::F32 => format!("{}", f32::from_bits(v)),
                    _ => crate::constants::format(p.type_name, u64::from(v))
                        .map(|name| format!("{v:#010x} {name}"))
                        .unwrap_or_else(|| format!("{v:#010x}")),
                }),
                _ => {
                    let words: Result<Vec<_>, _> =
                        (0..p.words).map(|_| args.next::<u32>()).collect();
                    words.map(|w| format!("{w:x?}"))
                }
            };
            format!(
                "{}={}",
                p.name,
                value.unwrap_or_else(|_| "<unreadable>".to_owned())
            )
        })
        .collect()
}
