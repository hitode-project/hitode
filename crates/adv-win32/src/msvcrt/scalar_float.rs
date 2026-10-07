//! Legacy VC80/90 decimal-prefix conversion, over the shared CRT locale/errno.
//! Tokenization is checked and bounded before the standard decimal-to-binary64
//! converter runs. A suffix is never decoded or copied as a whole guest string.
use super::*;
use adv_abi::{
    AnsiStr, GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{Guest, MemFault, Prot};

const MAX_PREFIX: usize = 4096;
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        registry.register(adv_abi::Api {
            module,
            ..strtod_ABI
        });
    }
}

enum Error {
    Memory(MemFault),
    Unsupported(&'static str),
}
struct Prefix<'a> {
    cpu: &'a dyn Guest,
    source: GuestPtr<u8>,
    bytes: Vec<u8>,
}
impl Prefix<'_> {
    fn at(&mut self, index: usize) -> Result<u8, Error> {
        if index >= MAX_PREFIX {
            return Err(Error::Unsupported(
                "decimal/whitespace prefix exceeds 4096 bytes",
            ));
        }
        if index == self.bytes.len() {
            let pointer = self.source.offset(index as u32).map_err(Error::Memory)?;
            check_range(self.cpu, pointer.addr(), 1, Prot::READ).map_err(Error::Memory)?;
            let mut byte = [0];
            self.cpu
                .read(pointer.addr(), &mut byte)
                .map_err(Error::Memory)?;
            self.bytes.push(byte[0]);
        }
        Ok(self.bytes[index])
    }
}
struct Parsed {
    value: f64,
    consumed: usize,
    range: bool,
}
fn parse(prefix: &mut Prefix<'_>, spaces: &[bool; 256], decimal: u8) -> Result<Parsed, Error> {
    let mut at = 0;
    while spaces[usize::from(prefix.at(at)?)] {
        at += 1;
    }
    let start = at;
    if matches!(prefix.at(at)?, b'+' | b'-') {
        at += 1;
    }
    let mut digits = 0;
    let mut nonzero = false;
    while prefix.at(at)?.is_ascii_digit() {
        nonzero |= prefix.at(at)? != b'0';
        at += 1;
        digits += 1;
    }
    if prefix.at(at)? == decimal {
        at += 1;
        while prefix.at(at)?.is_ascii_digit() {
            nonzero |= prefix.at(at)? != b'0';
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return Ok(Parsed {
            value: 0.0,
            consumed: 0,
            range: false,
        });
    }
    if matches!(prefix.at(at)?, b'e' | b'E') {
        let exponent = at;
        at += 1;
        if matches!(prefix.at(at)?, b'+' | b'-') {
            at += 1;
        }
        let first = at;
        while prefix.at(at)?.is_ascii_digit() {
            at += 1;
        }
        if first == at {
            at = exponent;
        }
    }
    // This slice contains only the validated decimal token, not leading locale
    // whitespace or a potentially unreadable/encoded suffix. Rust's standard
    // binary64 converter handles rounding and arbitrarily signed exponents.
    let token = std::str::from_utf8(&prefix.bytes[start..at])
        .map_err(|_| Error::Unsupported("non-ASCII decimal separator is unavailable"))?;
    let value = token
        .parse::<f64>()
        .map_err(|_| Error::Unsupported("decimal token conversion is unavailable"))?;
    Ok(Parsed {
        value,
        consumed: at,
        range: value.is_infinite() || value == 0.0 && nonzero,
    })
}
fn store_errno(env: &mut Env<'_>, code: i32) -> Result<(), Outcome> {
    floating::errno_address(env)
        .write(env.cpu, code)
        .map_err(memory_failure)
}
fn unsupported(env: &mut Env<'_>, text: AnsiStr, end: GuestPtr<u32>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    if !end.is_null()
        && let Err(error) = end.write(env.cpu, text.0.addr())
    {
        return memory_failure(error);
    }
    match store_errno(env, 40) {
        // VC ENOSYS; no invalid-parameter callback.
        Ok(()) => Outcome::ReturnF64(0.0),
        Err(outcome) => outcome,
    }
}

#[win32(msvcrt, name = "strtod", level = implement)]
fn strtod(env: &mut Env<'_>, text: AnsiStr, end: GuestPtr<u32>) -> Outcome {
    if !end.is_null()
        && let Err(error) = check_range(env.cpu, end.addr(), 4, Prot::WRITE)
    {
        return memory_failure(error);
    }
    if text.0.is_null() {
        if !end.is_null()
            && let Err(error) = end.write(env.cpu, 0)
        {
            return memory_failure(error);
        }
        return invalid_parameter_then(env, "strtod", 22, |_| Outcome::ReturnF64(0.0));
    }
    let Some((spaces, decimal)) = locale::scalar_float_profile(env) else {
        return unsupported(
            env,
            text,
            end,
            "LC_NUMERIC/LC_CTYPE conversion profile is unavailable",
        );
    };
    let control = match env.cpu.fpu_environment() {
        Ok(control) => control,
        Err(_) => {
            return unsupported(
                env,
                text,
                end,
                "guest floating control state is unavailable",
            );
        }
    };
    if control.x87_control & 0xc00 != 0 || control.mxcsr & 0x6000 != 0 {
        return unsupported(
            env,
            text,
            end,
            "non-nearest floating rounding mode is unavailable",
        );
    }
    let mut prefix = Prefix {
        cpu: env.cpu,
        source: text.0,
        bytes: Vec::new(),
    };
    let parsed = match parse(&mut prefix, &spaces, decimal) {
        Ok(parsed) => parsed,
        Err(Error::Memory(error)) => return memory_failure(error),
        Err(Error::Unsupported(reason)) => return unsupported(env, text, end, reason),
    };
    if !end.is_null() {
        let address = match text.0.offset(parsed.consumed as u32) {
            Ok(pointer) => pointer.addr(),
            Err(error) => return memory_failure(error),
        };
        if let Err(error) = end.write(env.cpu, address) {
            return memory_failure(error);
        }
    }
    if parsed.range
        && let Err(outcome) = store_errno(env, 34)
    {
        return outcome;
    }
    Outcome::ReturnF64(parsed.value)
}
