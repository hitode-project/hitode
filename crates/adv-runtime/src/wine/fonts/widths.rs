//! Unicode character advances with complete guest output validation.
use super::*;
use adv_cpu::backend::Prot;

pub(super) fn register(registry: &mut Registry) {
    registry.register(widths_w_ABI);
    registry.register(widths_a_ABI);
}

#[win32(gdi32, name = "GetCharWidth32W", level = implement)]
pub(super) fn widths_w(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<i32>,
) -> Outcome {
    widths(env, dc, first, last, output, true)
}

#[win32(gdi32, name = "GetCharWidth32A", level = implement)]
pub(super) fn widths_a(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<i32>,
) -> Outcome {
    widths(env, dc, first, last, output, false)
}

pub(super) fn widths(
    env: &mut Env<'_>,
    dc: Handle,
    first: u32,
    last: u32,
    output: GuestPtr<i32>,
    wide: bool,
) -> Outcome {
    if reject_dc(env, dc.0) {
        return Outcome::Return(0);
    }
    let invalid = |env: &mut Env<'_>| {
        set_last_error(env, 87);
        Outcome::Return(0)
    };
    if first > last || last > u32::from(u16::MAX) {
        return invalid(env);
    }
    let bytes = (last - first + 1) * 4;
    if output.is_null()
        || adv_abi::guest::check_range(env.cpu, output.addr(), bytes, Prot::WRITE).is_err()
    {
        return invalid(env);
    }
    let request = font_request(env, dc.0);
    let result = if wide {
        env.ext
            .get_mut::<Fonts>()
            .text
            .character_widths(&request, first as u16, last as u16)
    } else {
        let characters: Vec<_> = (first..=last)
            .map(|value| ansi_character(env, &request, value))
            .collect();
        let provider = &mut env.ext.get_mut::<Fonts>().text;
        let mut result = Ok(Vec::with_capacity(characters.len()));
        let mut at = 0;
        while at < characters.len() {
            let values = match characters[at] {
                Character::Encoded(code) => provider
                    .encoded_character_width(&request, code as u16)
                    .map(|width| vec![width]),
                Character::Unicode(first) => {
                    let mut end = at + 1;
                    while end < characters.len()
                        && characters[end] == Character::Unicode(first + (end - at) as u32)
                    {
                        end += 1;
                    }
                    let values = provider.character_widths(
                        &request,
                        first as u16,
                        (first + (end - at) as u32 - 1) as u16,
                    );
                    at = end - 1;
                    values
                }
                Character::GlyphIndex(_) => {
                    unreachable!("ANSI decoding cannot produce a glyph index")
                }
            };
            match values {
                Ok(values) => result.as_mut().unwrap().extend(values),
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
            at += 1;
        }
        result
    };
    match result {
        Ok(widths) => {
            let packed: Vec<_> = widths.into_iter().flat_map(i32::to_le_bytes).collect();
            if env.cpu.write(output.addr(), &packed).is_ok() {
                Outcome::Return(1)
            } else {
                invalid(env)
            }
        }
        Err(error) => {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "gdi32.dll!GetCharWidth32W"
                } else {
                    "gdi32.dll!GetCharWidth32A"
                },
                &error.to_string(),
            );
            set_last_error(env, 50);
            Outcome::Return(0)
        }
    }
}
