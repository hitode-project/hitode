//! Scalar Automation conversions reuse the guest locale and BSTR allocation owners.
use super::*;
use crate::kernel32::{Locale, locale_by_id};
use adv_abi::WideStr;
use adv_os::com::alloc_bstr;
mod date;
mod number;
use number::Number;
// CY is the pinned eight-byte by-value union, whose integer member is scaled by 10,000.
struct Currency(i64);
impl adv_abi::GuestArg for Currency {
    const KIND: adv_abi::AbiKind = adv_abi::AbiKind::Aggregate;
    const WORDS: u8 = 2;
    fn decode(args: &mut adv_abi::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> {
        Ok(Self(<i64 as adv_abi::GuestArg>::decode(args)?))
    }
}
const MISMATCH: u32 = 0x80020005;
const OVERFLOW: u32 = 0x8002000a;

pub(super) fn register(registry: &mut Registry) {
    for api in [
        bool_from_ABI,
        bool_text_ABI,
        currency_text_ABI,
        date_text_ABI,
        currency_from_ABI,
        date_from_ABI,
        integer_from_ABI,
        unsigned_from_ABI,
        single_from_ABI,
        double_from_ABI,
    ] {
        registry.register(api);
    }
}
fn locale(
    env: &mut Env<'_>,
    id: u32,
    flags: u32,
    api: &'static str,
    allowed: u32,
) -> Result<Locale, u32> {
    if flags & !(allowed | 0x80000000) != 0 {
        return Err(warn(
            env,
            api,
            "requested Automation calendar or conversion flags are unavailable",
            NOT_IMPLEMENTED,
        ));
    }
    locale_by_id(env, id).map_err(|_| {
        warn(
            env,
            api,
            "requested locale is outside the guest locale data",
            0x8002000c,
        )
    })
}
fn string(env: &Env<'_>, pointer: u32) -> Result<String, u32> {
    if pointer == 0 {
        return Err(INVALID);
    }
    let units = WideStr(GuestPtr::new(pointer))
        .read(env.cpu, (MAX_BSTR_BYTES / 2) as usize)
        .map_err(|_| INVALID)?;
    String::from_utf16(&units).map_err(|_| MISMATCH)
}
fn input(
    env: &mut Env<'_>,
    pointer: u32,
    id: u32,
    flags: u32,
    api: &'static str,
) -> Result<(String, Locale), u32> {
    let locale = locale(env, id, flags, api, 0x10)?;
    Ok((string(env, pointer)?, locale))
}
fn output(env: &mut Env<'_>, address: u32, bytes: &[u8]) -> u32 {
    if check_range(env.cpu, address, bytes.len() as u32, Prot::WRITE).is_err() {
        return INVALID;
    }
    match env.cpu.write(address, bytes) {
        Ok(()) => 0,
        Err(_) => INVALID,
    }
}
fn bstr(env: &mut Env<'_>, address: u32, text: &str) -> u32 {
    if check_range(env.cpu, address, 4, Prot::WRITE).is_err() {
        return INVALID;
    }
    let pointer = alloc_bstr(env, &text.encode_utf16().collect::<Vec<_>>());
    if pointer == 0 {
        return OUT_OF_MEMORY;
    }
    let result = output(env, address, &pointer.to_le_bytes());
    if result != 0 {
        free_bstr(env, pointer);
    }
    result
}
fn numeric(env: &mut Env<'_>, p: u32, l: u32, f: u32, api: &'static str) -> Result<Number, u32> {
    let (text, locale) = input(env, p, l, f, api)?;
    Number::parse(&text, locale)
}
fn boolean(text: &str, locale: Locale) -> Result<i16, u32> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("true") {
        return Ok(-1);
    }
    if text.eq_ignore_ascii_case("false") {
        return Ok(0);
    }
    // Keep exact mantissas, including nonzero values below the floating-point range.
    Ok(if Number::parse(text, locale)?.nonzero() {
        -1
    } else {
        0
    })
}
#[win32(oleaut32,name="VarBoolFromStr",level=implement)]
fn bool_from(env: &mut Env<'_>, text: WideStr, lcid: u32, flags: u32, out: GuestPtr<i16>) -> u32 {
    let result = input(
        env,
        text.0.addr(),
        lcid,
        flags,
        "oleaut32.dll!VarBoolFromStr",
    )
    .and_then(|(text, l)| boolean(&text, l));
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarBstrFromBool",level=implement)]
fn bool_text(env: &mut Env<'_>, value: i32, lcid: u32, flags: u32, out: GuestPtr<u32>) -> u32 {
    match locale(env, lcid, flags, "oleaut32.dll!VarBstrFromBool", 0x10) {
        Ok(_) => bstr(
            env,
            out.addr(),
            if value as i16 == 0 { "False" } else { "True" },
        ),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarBstrFromCy",level=implement)]
fn currency_text(
    env: &mut Env<'_>,
    value: Currency,
    lcid: u32,
    flags: u32,
    out: GuestPtr<u32>,
) -> u32 {
    match locale(env, lcid, flags, "oleaut32.dll!VarBstrFromCy", 0) {
        Ok(_) => bstr(env, out.addr(), &Number::from_currency(value.0).text()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarBstrFromDate",level=implement)]
fn date_text(env: &mut Env<'_>, value: f64, lcid: u32, flags: u32, out: GuestPtr<u32>) -> u32 {
    let result = locale(env, lcid, flags, "oleaut32.dll!VarBstrFromDate", 0x43)
        .and_then(|locale| date::format(value, locale, flags));
    match result {
        Ok(value) => bstr(env, out.addr(), &value),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarCyFromStr",level=implement)]
fn currency_from(
    env: &mut Env<'_>,
    text: WideStr,
    lcid: u32,
    flags: u32,
    out: GuestPtr<i64>,
) -> u32 {
    let result = numeric(env, text.0.addr(), lcid, flags, "oleaut32.dll!VarCyFromStr")
        .and_then(|n| i64::try_from(n.fixed(4)?).map_err(|_| OVERFLOW));
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarDateFromStr",level=implement)]
fn date_from(env: &mut Env<'_>, text: WideStr, lcid: u32, flags: u32, out: GuestPtr<f64>) -> u32 {
    let result = locale(env, lcid, flags, "oleaut32.dll!VarDateFromStr", 0x43)
        .and_then(|locale| date::parse(env, &string(env, text.0.addr())?, locale, flags));
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarI4FromStr",level=implement)]
fn integer_from(
    env: &mut Env<'_>,
    text: WideStr,
    lcid: u32,
    flags: u32,
    out: GuestPtr<i32>,
) -> u32 {
    let result = numeric(env, text.0.addr(), lcid, flags, "oleaut32.dll!VarI4FromStr")
        .and_then(|n| i32::try_from(n.fixed(0)?).map_err(|_| OVERFLOW));
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarUI4FromStr",level=implement)]
fn unsigned_from(
    env: &mut Env<'_>,
    text: WideStr,
    lcid: u32,
    flags: u32,
    out: GuestPtr<u32>,
) -> u32 {
    let result = numeric(
        env,
        text.0.addr(),
        lcid,
        flags,
        "oleaut32.dll!VarUI4FromStr",
    )
    .and_then(|n| u32::try_from(n.fixed(0)?).map_err(|_| OVERFLOW));
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarR4FromStr",level=implement)]
fn single_from(env: &mut Env<'_>, text: WideStr, lcid: u32, flags: u32, out: GuestPtr<f32>) -> u32 {
    let result =
        numeric(env, text.0.addr(), lcid, flags, "oleaut32.dll!VarR4FromStr").and_then(|n| {
            let value = n.floating()? as f32;
            if value.is_finite() {
                Ok(value)
            } else {
                Err(OVERFLOW)
            }
        });
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
#[win32(oleaut32,name="VarR8FromStr",level=implement)]
fn double_from(env: &mut Env<'_>, text: WideStr, lcid: u32, flags: u32, out: GuestPtr<f64>) -> u32 {
    let result = numeric(env, text.0.addr(), lcid, flags, "oleaut32.dll!VarR8FromStr")
        .and_then(|n| n.floating());
    match result {
        Ok(value) => output(env, out.addr(), &value.to_le_bytes()),
        Err(error) => error,
    }
}
