//! USER paragraph and tab layout using the font driver's advances and metrics.
use super::*;
use adv_cpu::backend::Prot;
const CALC: u32 = 0x400;
const SINGLE: u32 = 0x20;
const TABS: u32 = 0x40;
const NO_PREFIX: u32 = 0x800;
pub(super) fn register(_registry: &mut Registry) {}
pub(super) fn fail(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, 87);
    Outcome::Return(0)
}
pub(super) fn read_input(
    env: &mut Env<'_>,
    input: u32,
    count: i32,
    ansi: bool,
) -> Result<Vec<u8>, Outcome> {
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

pub(super) fn lines(input: &[u16], flags: u32) -> Vec<Vec<u16>> {
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

fn decoded(env: &mut Env<'_>, pointer: u32, count: i32, ansi: bool) -> Result<Vec<u16>, Outcome> {
    let bytes = read_input(env, pointer, count, ansi)?;
    Ok(if ansi {
        env.ext.get::<Fonts>().codec.to_utf16(0, &bytes)
    } else {
        placement::units(bytes)
    })
}
fn widths(
    env: &mut Env<'_>,
    request: &FontRequest,
    units: &[u16],
    tab: i32,
    expand: bool,
) -> Vec<i32> {
    let mut dx = driver::positions(env, request, units, false, 0, 0, 0)
        .unwrap()
        .0;
    let mut old = 0;
    let mut cursor = 0;
    for (i, &unit) in units.iter().enumerate() {
        let width = dx[i] - old;
        old = dx[i];
        cursor = if expand && unit == 9 {
            (cursor / tab + 1) * tab
        } else {
            cursor + width
        };
        dx[i] = cursor;
    }
    dx
}
fn prefix_positions(input: &[u16], flags: u32) -> Vec<Vec<usize>> {
    let mut output = vec![Vec::new()];
    let mut column = 0;
    let mut i = 0;
    while i < input.len() && input.first() != Some(&0) {
        let c = input[i];
        i += 1;
        if c == 38 && flags & NO_PREFIX == 0 {
            if input.get(i) != Some(&38) {
                output.last_mut().unwrap().push(column);
                continue;
            }
            i += 1;
        }
        if matches!(c, 10 | 13) && flags & SINGLE == 0 {
            if c == 13 && input.get(i) == Some(&10) {
                i += 1;
            }
            output.push(Vec::new());
            column = 0;
        } else {
            column += 1;
        }
    }
    output
}
fn cjk(c: u16) -> bool {
    matches!(c,0x2e80..=0xa4cf|0xac00..=0xd7af|0xf900..=0xfaff|0xff00..=0xffef)
}
pub(super) fn dispatch(env: &mut Env<'_>, name: &str, a: &[u32]) -> Outcome {
    let ansi = name.ends_with('A');
    if name.contains("Tabbed") {
        return tabbed(env, name, a, ansi);
    }
    let Ok(units) = decoded(env, a[1], a[2] as i32, ansi) else {
        return fail(env);
    };
    if reject_dc(env, a[0]) {
        return Outcome::Return(0);
    }
    let Ok(mut rect) = GuestPtr::<[i32; 4]>::new(a[3]).read(env.cpu) else {
        return fail(env);
    };
    let mut flags = a[4];
    let params = if name.contains("Ex") { a[5] } else { 0 };
    if params != 0 && env.cpu.read_u32(params).unwrap_or(0) != 20 {
        return Outcome::Return(0);
    }
    let packed_tab = if params == 0 && flags & 0x80 != 0 {
        let tab = ((flags >> 8) & 255) as i32;
        flags &= 0xffff00ff;
        Some(tab)
    } else {
        None
    };
    if ansi && units.is_empty() {
        return Outcome::Return(0);
    }
    let request = font_request(env, a[0]);
    let Ok(metrics) = env.ext.get_mut::<Fonts>().text.metrics(&request) else {
        return Outcome::Return(0);
    };
    let line_height = metrics.height
        + if flags & 0x200 != 0 {
            metrics.external_leading
        } else {
            0
        };
    if a[2] == 0 && units.is_empty() && env.cpu.read_u16(a[1]).unwrap_or(0) != 0 {
        return Outcome::Return(line_height as u32);
    }
    let (left_margin, right_margin) = if params != 0 {
        (
            env.cpu.read_u32(params + 8).unwrap() as i32,
            env.cpu.read_u32(params + 12).unwrap() as i32,
        )
    } else {
        (0, 0)
    };
    let tab_chars = if params != 0 {
        env.cpu.read_u32(params + 4).unwrap() as i32
    } else {
        packed_tab.unwrap_or(8)
    };
    let tab = metrics.average_width.max(1) * if tab_chars == 0 { 8 } else { tab_chars };
    let max_width = (rect[2] - rect[0] - left_margin - right_margin).max(0);
    let mut formatted = Vec::new();
    let paragraphs = lines(&units, flags);
    let prefixes = prefix_positions(&units, flags);
    let mut prefix_lines = Vec::new();
    for (paragraph, prefixes) in paragraphs.into_iter().zip(prefixes) {
        let mut start = 0;
        loop {
            let tail = &paragraph[start..];
            let dx = widths(env, &request, tail, tab, flags & TABS != 0);
            let mut end = tail.len();
            let mut skip = end;
            if flags & 0x10 != 0
                && flags & SINGLE == 0
                && dx.last().copied().unwrap_or(0) > max_width
            {
                let fit = dx
                    .iter()
                    .take_while(|&&x| x <= max_width)
                    .count()
                    .min(tail.len());
                // Completed tab-separated segments stay on this line. Word
                // breaking applies to the segment after the expanded stop.
                let mut segment = if flags & TABS != 0 {
                    tail[..fit]
                        .iter()
                        .rposition(|&c| c == 9)
                        .map_or(0, |i| i + 1)
                } else {
                    0
                };
                if flags & TABS != 0 && tail.get(fit) == Some(&9) {
                    segment = fit + 1;
                    while tail.get(segment) == Some(&9) {
                        segment += 1;
                    }
                }
                let boundary = (segment + 1..=fit).rev().find(|&i| {
                    tail[i - 1] == 32
                        || (i < tail.len()
                            && flags & 0x80000 == 0
                            && (cjk(tail[i - 1]) || cjk(tail[i]))
                            && !matches!(
                                tail[i - 1],
                                0x3008
                                    | 0x300a
                                    | 0x300c
                                    | 0x300e
                                    | 0x3010
                                    | 0xff08
                                    | 0xff3b
                                    | 0xff5b
                            )
                            && !matches!(
                                tail[i],
                                0x3001
                                    | 0x3002
                                    | 0x3009
                                    | 0x300b
                                    | 0x300d
                                    | 0x300f
                                    | 0x3011
                                    | 0xff09
                                    | 0xff0c
                                    | 0xff0e
                                    | 0xff3d
                                    | 0xff5d
                            ))
                });
                if let Some(boundary) = boundary {
                    end = boundary;
                    if flags & 3 != 0 && tail[end - 1] == 32 {
                        end -= 1;
                    }
                    skip = boundary + usize::from(tail.get(boundary) == Some(&32));
                } else if flags & (0x2000 | 0x40000 | 0x4000) == 0x2000 {
                    end = fit.max(segment + 1).min(tail.len());
                    skip = end;
                } else {
                    let from = fit.max(segment);
                    end = tail[from..]
                        .iter()
                        .position(|&c| c == 32 || (flags & TABS != 0 && c == 9))
                        .map_or(tail.len(), |n| from + n);
                    skip = end + usize::from(tail.get(end) == Some(&32));
                }
                if end > 0
                    && end < tail.len()
                    && (0xd800..=0xdbff).contains(&tail[end - 1])
                    && (0xdc00..=0xdfff).contains(&tail[end])
                {
                    end += 1;
                    skip = skip.max(end);
                }
            }
            let mut line = tail[..end].to_vec();
            let ellipsis = flags & (0x8000 | 0x4000 | 0x40000) != 0;
            if ellipsis
                && widths(env, &request, &line, tab, flags & TABS != 0)
                    .last()
                    .copied()
                    .unwrap_or(0)
                    > max_width
            {
                let dots = [46u16; 3];
                let ew = widths(env, &request, &dots, tab, false)
                    .last()
                    .copied()
                    .unwrap_or(0);
                if flags & 0x4000 != 0 {
                    let last = line.iter().rposition(|&c| c == 92 || c == 47).unwrap_or(0);
                    let suffix = line[last..].to_vec();
                    let sw = widths(env, &request, &suffix, tab, false)
                        .last()
                        .copied()
                        .unwrap_or(0);
                    line.truncate(last);
                    while !line.is_empty()
                        && widths(env, &request, &line, tab, false)
                            .last()
                            .copied()
                            .unwrap_or(0)
                            + ew
                            + sw
                            > max_width
                    {
                        line.pop();
                    }
                    line.extend(dots);
                    line.extend(suffix);
                }
                if flags & (0x8000 | 0x40000) != 0 {
                    while !line.is_empty()
                        && widths(env, &request, &line, tab, false)
                            .last()
                            .copied()
                            .unwrap_or(0)
                            + ew
                            > max_width
                    {
                        line.pop();
                    }
                    line.extend(dots);
                }
            }
            prefix_lines.push(
                prefixes
                    .iter()
                    .copied()
                    .filter(|&p| p >= start && p < start + end)
                    .map(|p| p - start)
                    .collect::<Vec<_>>(),
            );
            formatted.push(line);
            if skip >= tail.len() {
                break;
            }
            start += skip;
        }
    }
    let mut extra_empty = 0;
    if flags & (CALC | SINGLE | 0x100) == 0 {
        let visible = if flags & 0x2000 != 0 {
            ((rect[3] - rect[1]) / line_height).max(0) as usize
        } else {
            ((rect[3] - rect[1] - 1) / line_height).max(0) as usize + 1
        };
        if formatted.len() > visible {
            extra_empty = usize::from(formatted.last().is_some_and(Vec::is_empty));
            formatted.truncate(visible);
            prefix_lines.truncate(visible);
            if flags & (0x8000 | 0x40000) != 0
                && let Some(line) = formatted.last_mut()
            {
                let dots = [46u16; 3];
                let ew = widths(env, &request, &dots, tab, false)
                    .last()
                    .copied()
                    .unwrap_or(0);
                while !line.is_empty()
                    && widths(env, &request, line, tab, flags & TABS != 0)
                        .last()
                        .copied()
                        .unwrap_or(0)
                        + ew
                        > max_width
                {
                    line.pop();
                }
                line.extend(dots);
            }
        }
    }
    let empty = units.first().is_none_or(|&c| c == 0) && flags & SINGLE == 0;
    let height = if empty {
        0
    } else {
        line_height * (formatted.len() + extra_empty) as i32
    };
    let maximum = formatted
        .iter()
        .map(|line| {
            widths(env, &request, line, tab, flags & TABS != 0)
                .last()
                .copied()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0);
    if params != 0 {
        let _ = env.cpu.write_u32(params + 16, units.len() as u32);
    }
    if flags & CALC != 0 {
        rect[2] = rect[0] + maximum + left_margin + right_margin;
        rect[3] = rect[1] + height;
        let _ = GuestPtr::<[i32; 4]>::new(a[3]).write(env.cpu, rect);
        return Outcome::Return(height.max(1) as u32);
    }
    if flags & 0x10000 != 0 {
        let mut modified = Vec::new();
        for (i, line) in formatted.iter().enumerate() {
            if i != 0 {
                modified.extend([13u16, 10]);
            }
            modified.extend(line);
        }
        modified.push(0);
        let bytes = if ansi {
            env.ext.get::<Fonts>().codec.from_utf16(0, &modified).0
        } else {
            modified.into_iter().flat_map(u16::to_le_bytes).collect()
        };
        let _ = env.cpu.write(a[1], &bytes);
    }
    let mut y = rect[1];
    if flags & SINGLE != 0 {
        if flags & 4 != 0 {
            y += (rect[3] - rect[1] - height) / 2;
        } else if flags & 8 != 0 {
            y = rect[3] - height;
        }
    }
    let dc = dc_pointer(env, a[0]).unwrap();
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    let at = attr + layout::attr::text_align;
    let align = env.cpu.read_u16(at).unwrap();
    env.cpu.write(at, &0u16.to_le_bytes()).unwrap();
    let mut drawing = std::collections::VecDeque::new();
    for (line, prefixes) in formatted.into_iter().zip(prefix_lines) {
        if flags & 0x2000 != 0 && y + line_height > rect[3] {
            break;
        }
        let width = widths(env, &request, &line, tab, flags & TABS != 0)
            .last()
            .copied()
            .unwrap_or(0);
        let x = rect[0]
            + left_margin
            + match flags & 3 {
                1 => (max_width - width) / 2,
                2 => max_width - width,
                _ => 0,
            };
        let cum = widths(env, &request, &line, tab, flags & TABS != 0);
        let mut old = 0;
        let dx: Vec<_> = cum
            .iter()
            .copied()
            .map(|v| {
                let d = v - old;
                old = v;
                d
            })
            .collect();
        let text: Vec<_> = line
            .iter()
            .map(|&c| if flags & TABS != 0 && c == 9 { 32 } else { c })
            .collect();
        if flags & 0x200000 == 0 {
            drawing.push_back(paint::Drawing::Text {
                hdc: a[0],
                x,
                y,
                flags: (if flags & 0x100 == 0 { 4 } else { 0 })
                    | (if flags & 0x20000 != 0 { 0x80 } else { 0 }),
                rectangle: Some(rect),
                units: text,
                dx: Some(dx),
            });
        }
        if flags & 0x100000 == 0 {
            let thickness = (metrics.height / 16).max(1);
            for prefix in prefixes.into_iter().filter(|&p| p < line.len()) {
                let left = if prefix == 0 { 0 } else { cum[prefix - 1] };
                let right = cum[prefix];
                drawing.push_back(paint::Drawing::Rule {
                    hdc: a[0],
                    rectangle: [
                        x + left,
                        y + metrics.ascent + thickness,
                        x + right,
                        y + metrics.ascent + thickness * 2,
                    ],
                    clip: (flags & 0x100 == 0).then_some(rect),
                });
            }
        }
        y += line_height;
    }
    paint::Paragraph {
        drawing,
        result: height.max(1) as u32,
        alignment: Some((at, align)),
    }
    .draw(env)
}
fn tabbed(env: &mut Env<'_>, name: &str, a: &[u32], ansi: bool) -> Outcome {
    let output = name.starts_with("Tabbed");
    let (pointer, count, ntabs, tabs) = if output {
        (a[3], a[4], a[5], a[6])
    } else {
        (a[1], a[2], a[3], a[4])
    };
    let Ok(units) = decoded(env, pointer, count as i32, ansi) else {
        return fail(env);
    };
    if reject_dc(env, a[0]) {
        return Outcome::Return(0);
    }
    let request = font_request(env, a[0]);
    let metrics = env.ext.get_mut::<Fonts>().text.metrics(&request).unwrap();
    let stops: Vec<_> = (0..ntabs)
        .map(|i| env.cpu.read_u32(tabs + i * 4).unwrap() as i32)
        .collect();
    let default = if stops.len() == 1 {
        stops[0]
    } else {
        metrics.average_width * 8
    };
    let explicit = if stops.len() == 1 {
        &[][..]
    } else {
        &stops[..]
    };
    let origin = if output { a[7] as i32 } else { 0 };
    let mut start = if output { a[1] as i32 } else { 0 };
    let y = if output { a[2] as i32 } else { 0 };
    let dc = dc_pointer(env, a[0]).unwrap();
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    if output && env.cpu.read_u16(attr + layout::attr::text_align).unwrap() & 1 != 0 {
        start = env.cpu.read_u32(attr + layout::attr::cur_pos).unwrap() as i32;
    }
    let mut cursor = start;
    let mut drawing = std::collections::VecDeque::new();
    let mut at = 0;
    let mut next_stop = 0;
    while at < units.len() {
        let left = cursor;
        let mut n = 0;
        while units.get(at + n) == Some(&9) {
            n += 1;
        }
        let end = units[at + n..]
            .iter()
            .position(|&c| c == 9)
            .map_or(units.len(), |nchars| at + n + nchars);
        let segment = &units[at + n..end];
        let width = driver::positions(env, &request, segment, false, 0, 0, 0)
            .unwrap()
            .1;
        let mut x = cursor;
        if n != 0 {
            while explicit.len().saturating_sub(next_stop) >= n {
                let stop = explicit[next_stop + n - 1];
                if origin + explicit[next_stop].abs() > cursor
                    && (stop >= 0 || origin - stop - width > cursor)
                {
                    x = origin + stop.abs() - if stop < 0 { width } else { 0 };
                    break;
                }
                next_stop += 1;
            }
            if explicit.len().saturating_sub(next_stop) < n {
                if default > 0 {
                    x = origin + ((cursor - origin) / default + n as i32) * default;
                } else if default < 0 {
                    x = origin + ((cursor - origin + width) / -default + n as i32) * -default
                        - width;
                }
            }
        }
        cursor = x + width;
        if output {
            let opaque = env
                .cpu
                .read_u16(attr + layout::attr::background_mode)
                .unwrap()
                == 2;
            drawing.push_back(paint::Drawing::Text {
                hdc: a[0],
                x,
                y,
                flags: if opaque { 2 } else { 0 },
                rectangle: Some([left, y, cursor, y + metrics.height]),
                units: segment.to_vec(),
                dx: None,
            });
        }
        at = end;
    }
    let result = if units.is_empty() {
        0
    } else {
        ((metrics.height as u32 & 65535) << 16) | ((cursor - start) as u32 & 65535)
    };
    if output {
        paint::Paragraph {
            drawing,
            result,
            alignment: None,
        }
        .draw(env)
    } else {
        Outcome::Return(result)
    }
}
