//! USER DrawText measurement and paragraph painting share the selected GDI
//! font, guest ACP and CPU text surface. Advanced paragraph layout is explicit.
use super::*;
use adv_cpu::backend::Prot;
use adv_device::text::TextInput;

const CALC: u32 = 0x400;
const SINGLE: u32 = 0x20;
const TABS: u32 = 0x40;
const NO_PREFIX: u32 = 0x800;
const LEADING: u32 = 0x200;
const SUPPORTED: u32 = CALC | SINGLE | TABS | NO_PREFIX | LEADING | 0x100 | 0x100000;

fn fail(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, 87);
    Outcome::Return(0)
}
fn unavailable(env: &mut Env<'_>, reason: &str) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
    set_last_error(env, 120);
    Outcome::Return(0)
}

fn read_input(env: &mut Env<'_>, input: u32, count: i32, ansi: bool) -> Result<Vec<u8>, Outcome> {
    if count == -1 {
        if ansi {
            AnsiStr(GuestPtr::new(input))
                .read(env.cpu, 1024 * 1024)
                .map_err(|_| fail(env))
        } else {
            WideStr(GuestPtr::new(input))
                .read(env.cpu, 512 * 1024)
                .map(|units| units.into_iter().flat_map(u16::to_le_bytes).collect())
                .map_err(|_| fail(env))
        }
    } else {
        let Some(length) = u32::try_from(count)
            .ok()
            .and_then(|n| n.checked_mul(if ansi { 1 } else { 2 }))
            .filter(|&n| n <= 1024 * 1024)
        else {
            return Err(fail(env));
        };
        if adv_abi::guest::check_range(env.cpu, input, length, Prot::READ).is_err() {
            return Err(fail(env));
        }
        let mut bytes = vec![0; length as usize];
        if length > 0 && env.cpu.read(input, &mut bytes).is_err() {
            return Err(fail(env));
        }
        Ok(bytes)
    }
}

#[allow(clippy::too_many_arguments)]
fn render(
    env: &mut Env<'_>,
    dc: u32,
    input: u32,
    count: i32,
    rect: GuestPtr<[i32; 4]>,
    flags: u32,
    ansi: bool,
) -> Outcome {
    if flags & !(SINGLE | NO_PREFIX | LEADING | 0x100) != 0 {
        return unavailable(
            env,
            "painting supports left/top lines; wrapping, tabs, alignment and ellipses are unavailable",
        );
    }
    if adv_abi::guest::check_range(env.cpu, rect.addr(), 16, Prot::READ).is_err() {
        return fail(env);
    }
    let Ok(rectangle) = rect.read(env.cpu) else {
        return fail(env);
    };
    if modes(env, dc).text_align != 0 {
        return unavailable(
            env,
            "DrawText painting requires TA_LEFT, TA_TOP and TA_NOUPDATECP",
        );
    }
    if ansi && !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        return unavailable(env, "painting ANSI code page is unavailable");
    }
    if count == 0 {
        if ansi {
            return Outcome::Return(0);
        }
        return match GuestPtr::<u16>::new(input).read(env.cpu) {
            Ok(first) if first != 0 => Outcome::Return(1),
            Ok(_) => {
                let request = font_request(env, dc);
                match env.ext.get_mut::<Fonts>().text.metrics(&request) {
                    Ok(metrics) => Outcome::Return(metrics.height.max(1) as u32),
                    Err(_) => fail(env),
                }
            }
            Err(_) => fail(env),
        };
    }
    let bytes = match read_input(env, input, count, ansi) {
        Ok(bytes) => bytes,
        Err(outcome) => return outcome,
    };
    if ansi && bytes.is_empty() {
        return Outcome::Return(0);
    }
    let units: Vec<u16> = if ansi {
        // USER converts the guest ACP, regardless of the selected LOGFONT charset.
        env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
    } else {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect()
    };
    if units.len() > text::MAX_PAINT_UNITS {
        return unavailable(env, "painting exceeds 8192 UTF-16 units");
    }
    if units
        .iter()
        .any(|&unit| unit < 32 && (flags & SINGLE != 0 || !matches!(unit, 10 | 13)))
    {
        return unavailable(env, "control-character layout is unavailable");
    }
    if flags & NO_PREFIX == 0 {
        let mut at = 0;
        while at < units.len() {
            if units[at] == 38 {
                if units.get(at + 1) != Some(&38) {
                    return unavailable(
                        env,
                        "mnemonic prefix underlining is unavailable; literal && and DT_NOPREFIX are supported",
                    );
                }
                at += 1;
            }
            at += 1;
        }
    }
    let mut formatted = lines(&units, flags);
    let request = font_request(env, dc);
    let line_height = match env.ext.get_mut::<Fonts>().text.metrics(&request) {
        Ok(metrics) => {
            metrics.height.max(1)
                + if flags & LEADING != 0 {
                    metrics.external_leading
                } else {
                    0
                }
        }
        Err(TextError::Unavailable) => {
            return unavailable(env, "selected font metrics are unavailable");
        }
        Err(_) => return fail(env),
    };
    if units.first().is_none_or(|&unit| unit == 0) && flags & SINGLE == 0 {
        return Outcome::Return(1);
    }
    let mut extra_empty = 0;
    if flags & (SINGLE | 0x100) == 0 {
        let visible = ((i64::from(rectangle[3]) - i64::from(rectangle[1]) - 1)
            / i64::from(line_height))
        .max(0) as usize
            + 1;
        if formatted.len() > visible {
            // USER counts one terminal empty line even when bottom clipping
            // stops paragraph output earlier. It does not paint that line.
            extra_empty = usize::from(formatted.last().is_some_and(Vec::is_empty));
            formatted.truncate(visible);
        }
    }
    let Some(height) = line_height.checked_mul((formatted.len() + extra_empty) as i32) else {
        return fail(env);
    };
    if rectangle[1].checked_add(height).is_none() {
        return fail(env);
    }
    super::paint::formatted(
        env,
        dc,
        rectangle,
        &formatted,
        line_height,
        flags & 0x100 == 0,
        height as u32,
    )
}

fn lines(input: &[u16], flags: u32) -> Vec<Vec<u16>> {
    let mut result = vec![Vec::new()];
    let mut i = 0;
    while i < input.len() && input.first() != Some(&0) {
        // Counted control characters retain their selected-font advances.
        // The -1 path was already terminated while reading the guest string.
        let c = input[i];
        i += 1;
        if c == 38 && flags & NO_PREFIX == 0 {
            if input.get(i) != Some(&38) {
                continue;
            }
            i += 1;
        }
        if matches!(c, 10 | 13) {
            if flags & SINGLE != 0 {
                result.last_mut().unwrap().push(c);
            } else {
                if c == 13 && input.get(i) == Some(&10) {
                    i += 1;
                }
                result.push(Vec::new());
            }
        } else {
            result.last_mut().unwrap().push(c);
        }
    }
    result
}

fn measure(
    env: &mut Env<'_>,
    dc: u32,
    input: u32,
    count: i32,
    rect: GuestPtr<[i32; 4]>,
    flags: u32,
    ansi: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    if flags & CALC == 0 {
        return render(env, dc, input, count, rect, flags, ansi);
    }
    if flags & !SUPPORTED != 0 {
        return unavailable(
            env,
            "DT_CALCRECT does not support word wrapping, ellipses or custom tab stops",
        );
    }
    let Ok([left, top, _, _]) = rect.read(env.cpu) else {
        return fail(env);
    };
    if adv_abi::guest::check_range(env.cpu, rect.addr(), 16, Prot::WRITE).is_err() {
        return fail(env);
    }
    if count == 0 {
        if ansi {
            return Outcome::Return(0);
        }
        let Ok(first) = GuestPtr::<u16>::new(input).read(env.cpu) else {
            return fail(env);
        };
        if first != 0 {
            return Outcome::Return(1);
        }
    }
    let bytes = match read_input(env, input, count, ansi) {
        Ok(bytes) => bytes,
        Err(outcome) => return outcome,
    };
    // The ANSI wrapper's zero-length conversion returns failure and leaves
    // RECT unchanged; Unicode's empty-string rectangle rules differ.
    if ansi && bytes.is_empty() {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc);
    // USER's ANSI wrapper converts through the guest ACP before formatting,
    // independently of the selected font's GDI byte-string charset rules.
    let units = if ansi {
        env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
    } else {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|u| u16::from_le_bytes(*u))
            .collect()
    };
    let layout = lines(&units, flags);
    let text = &mut env.ext.get_mut::<Fonts>().text;
    let measured = (|| -> Result<(i32, i32), TextError> {
        let metrics = text.metrics(&request)?;
        if units.first().is_none_or(|&u| u == 0) && flags & SINGLE == 0 {
            return Ok((0, 0));
        }
        let tab = i64::from(metrics.average_width).max(1) * 8;
        let mut maximum = 0i64;
        for line in &layout {
            let mut width = 0i64;
            let mut pieces = line.split(|&u| flags & TABS != 0 && u == 9).peekable();
            while let Some(piece) = pieces.next() {
                let input = TextInput::Unicode(piece);
                width += i64::from(text.extent(&request, input)?.width);
                if pieces.peek().is_some() {
                    width = (width / tab + 1) * tab;
                }
            }
            maximum = maximum.max(width);
        }
        let line_height = i64::from(metrics.height)
            + if flags & LEADING != 0 {
                i64::from(metrics.external_leading)
            } else {
                0
            };
        Ok((
            i32::try_from(maximum).map_err(|_| TextError::Unavailable)?,
            i32::try_from(line_height * layout.len() as i64).map_err(|_| TextError::Unavailable)?,
        ))
    })();
    let (width, height) = match measured {
        Ok(size) => size,
        Err(TextError::Unavailable) => return Outcome::Fail(TextError::Unavailable.to_string()),
        Err(_) => return fail(env),
    };
    let (Some(right), Some(bottom)) = (left.checked_add(width), top.checked_add(height)) else {
        return fail(env);
    };
    if rect.write(env.cpu, [left, top, right, bottom]).is_err() {
        return fail(env);
    }
    Outcome::Return(height.max(1) as u32)
}

#[win32(user32, name = "DrawTextA", level = state)]
fn measure_a(
    env: &mut Env<'_>,
    dc: Handle,
    input: AnsiStr,
    count: i32,
    rect: GuestPtr<[i32; 4]>,
    flags: u32,
) -> Outcome {
    measure(env, dc.0, input.0.addr(), count, rect, flags, true)
}
#[win32(user32, name = "DrawTextW", level = state)]
fn measure_w(
    env: &mut Env<'_>,
    dc: Handle,
    input: WideStr,
    count: i32,
    rect: GuestPtr<[i32; 4]>,
    flags: u32,
) -> Outcome {
    measure(env, dc.0, input.0.addr(), count, rect, flags, false)
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(measure_a_ABI);
    registry.register(measure_w_ABI);
}

#[cfg(test)]
mod tests;
