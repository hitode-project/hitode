//! Native format operations keep native pointers inside the device adapter.
use super::*;

pub(super) fn execute(object: &Object, operation: Op<'_>) -> Result<Reply, i32> {
    let p = require(object, Kind::Format)?;
    unsafe {
        match operation {
            Op::StringProperty(index) => {
                let mut value = 0;
                status(match index {
                    0 => gp::GdipGetStringFormatFlags(p.cast(), &mut value),
                    1 => {
                        let mut language = 0;
                        let status = gp::GdipGetStringFormatDigitSubstitution(
                            p.cast(),
                            &mut language,
                            std::ptr::null_mut(),
                        );
                        value = i32::from(language);
                        status
                    }
                    2 => gp::GdipGetStringFormatAlign(p.cast(), &mut value),
                    3 => gp::GdipGetStringFormatLineAlign(p.cast(), &mut value),
                    4 => gp::GdipGetStringFormatHotkeyPrefix(p.cast(), &mut value),
                    5 => gp::GdipGetStringFormatTrimming(p.cast(), &mut value),
                    6 => gp::GdipGetStringFormatMeasurableCharacterRangeCount(p.cast(), &mut value),
                    7 => gp::GdipGetStringFormatTabStopCount(p.cast(), &mut value),
                    _ => return Err(2),
                })?;
                return Ok(Reply::Count(value));
            }
            Op::Hotkey(value) => status(gp::GdipSetStringFormatHotkeyPrefix(p.cast(), value))?,
            Op::Trimming(value) => status(gp::GdipSetStringFormatTrimming(p.cast(), value))?,
            Op::Digits(value) => {
                if let Some((language, method)) = value {
                    status(gp::GdipSetStringFormatDigitSubstitution(
                        p.cast(),
                        language,
                        method,
                    ))?;
                }
                let (mut language, mut method) = (0, 0);
                status(gp::GdipGetStringFormatDigitSubstitution(
                    p.cast(),
                    &mut language,
                    &mut method,
                ))?;
                return Ok(Reply::Digits(language, method));
            }
            Op::Tabs(Some((first, tabs))) => status(gp::GdipSetStringFormatTabStops(
                p.cast(),
                first,
                tabs.len() as i32,
                tabs.as_ptr(),
            ))?,
            Op::Tabs(None) => {
                let mut count = 0;
                status(gp::GdipGetStringFormatTabStopCount(p.cast(), &mut count))?;
                let mut tabs = Vec::new();
                tabs.try_reserve_exact(usize::try_from(count).map_err(|_| 1)?)
                    .map_err(|_| 3)?;
                tabs.resize(count as usize, 0.);
                let mut first = 0.;
                status(gp::GdipGetStringFormatTabStops(
                    p.cast(),
                    count,
                    &mut first,
                    tabs.as_mut_ptr(),
                ))?;
                return Ok(Reply::Tabs(first, tabs));
            }
            Op::CharacterRanges(ranges) => {
                let native: Vec<_> = ranges
                    .iter()
                    .map(|range| gp::CharacterRange {
                        First: range[0],
                        Length: range[1],
                    })
                    .collect();
                status(gp::GdipSetStringFormatMeasurableCharacterRanges(
                    p.cast(),
                    native.len() as i32,
                    native.as_ptr(),
                ))?;
            }
            _ => return Err(2),
        }
    }
    Ok(Reply::None)
}
