//! Win32 adapters for native counted-string loader calls.
use super::*;
use adv_cpu::backend::Reg;

fn scratch(env: &mut Env<'_>, bytes: &[u8], wide: bool) -> Result<u32, Outcome> {
    if bytes.len() > 65532 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Err(Outcome::Return(0));
    }
    let size = 16 + bytes.len() as u32 + if wide { 2 } else { 1 };
    let Some(p) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(size + 15)
        .map(|p| p & !15)
        .filter(|&p| env.cpu.accessible(p, size, Prot::RW))
    else {
        set_last_error(env, err::NOT_ENOUGH_MEMORY);
        return Err(Outcome::Return(0));
    };
    let result = (|| -> Result<(), MemFault> {
        env.cpu.write(p, &vec![0; size as usize])?;
        env.cpu.write(p, &(bytes.len() as u16).to_le_bytes())?;
        env.cpu.write(p + 2, &((size - 16) as u16).to_le_bytes())?;
        env.cpu.write_u32(p + 4, p + 16)?;
        env.cpu.write(p + 16, bytes)
    })();
    if result.is_err() {
        set_last_error(env, 998);
        return Err(Outcome::Return(0));
    }
    Ok(p)
}
fn result(env: &mut Env<'_>, p: u32, status: u32) -> Outcome {
    let output = env.cpu.read_u32(p + 8);
    if (status as i32) < 0 {
        return load_io_error(env, LibraryFront::Win32, status);
    }
    match output {
        Ok(value) => Outcome::Return(value),
        Err(_) => load_io_error(env, LibraryFront::Win32, 0xc0000005),
    }
}
pub(super) fn load(env: &mut Env<'_>, target: u32, name: String, altered: bool) -> Outcome {
    let bytes: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let p = match scratch(env, &bytes, true) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    // Native x86 uses tagged LDR_PATH_IS_FLAGS | WITH_ALTERED_SEARCH_PATH.
    let search = if altered { 9 } else { 0 };
    Outcome::call_on_stack(target, [search, 0, p, p + 8], p, move |env, value| {
        result(env, p, value.value())
    })
}

pub(super) fn handle(
    env: &mut Env<'_>,
    target: u32,
    name: String,
    found: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let bytes: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let p = match scratch(env, &bytes, true) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    Outcome::call_on_stack(target, [0, 0, p, p + 8], p, move |env, value| match result(
        env,
        p,
        value.value(),
    ) {
        Outcome::Return(base) if base != 0 => found(env, base),
        outcome => outcome,
    })
}

pub(super) fn address(
    env: &mut Env<'_>,
    target: u32,
    address: u32,
    found: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let p = match scratch(env, &[], false) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    Outcome::call_on_stack(target, [address, p + 8], p, move |env, value| {
        let base = value.value();
        if base == 0 {
            module_handle_result(env, None)
        } else {
            found(env, base)
        }
    })
}
pub(super) fn procedure(
    env: &mut Env<'_>,
    target: u32,
    module: u32,
    symbol: ImportName,
) -> Outcome {
    let (bytes, ordinal) = match symbol {
        ImportName::Name { name, .. } => (name.into_bytes(), 0),
        ImportName::Ordinal(ordinal) => (Vec::new(), u32::from(ordinal)),
    };
    let p = match scratch(env, &bytes, false) {
        Ok(p) => p,
        Err(outcome) => return outcome,
    };
    Outcome::call_on_stack(
        target,
        [module, if ordinal == 0 { p } else { 0 }, ordinal, p + 8],
        p,
        move |env, value| result(env, p, value.value()),
    )
}
