//! Logical pens share GDI selection and deletion ownership.
use super::*;

pub(super) fn create(env: &mut Env<'_>, style: u32, width: i32, color: u32) -> Outcome {
    let style = if style <= 6 { style } else { 0 };
    if style == 5 {
        return Outcome::Return(stock_object(8));
    }
    let Some(width) = width.checked_abs() else {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    };
    Outcome::Return(add(
        env,
        GdiObj::Pen {
            style,
            width: width as u32,
            color,
        },
    ))
}

#[win32(gdi32, name = "CreatePenIndirect", level = state)]
fn create_indirect(env: &mut Env<'_>, pointer: GuestPtr<[u8; 16]>) -> Outcome {
    let Ok(bytes) = pointer.read(env.cpu) else {
        return Outcome::Return(0);
    };
    let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    // LOGPEN's POINT.y is ignored by Windows.
    create(env, word(0), word(4) as i32, word(12))
}

pub(super) fn describe(env: &Env<'_>, handle: u32) -> Option<[u8; 16]> {
    let (style, width, color) = match handle {
        h if h == stock_object(6) => (0, 0, 0xffffff),
        h if h == stock_object(7) || h == stock_object(19) => (0, 0, 0),
        h if h == stock_object(8) => (5, 1, 0),
        h => match get(env, h)? {
            GdiObj::Pen {
                style,
                width,
                color,
            } => (*style, *width, *color),
            _ => return None,
        },
    };
    let mut bytes = [0; 16];
    for (offset, value) in [(0, style), (4, width), (12, color)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    Some(bytes)
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(create_indirect_ABI);
}

#[cfg(test)]
mod tests;
