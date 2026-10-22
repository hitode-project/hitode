//! Mutable layout settings belong to the format object and clone with it.
use super::*;

#[derive(Clone)]
pub(in crate::gdiplus) struct Format {
    flags: i32,
    language: u16,
    digits: i32,
    align: i32,
    line_align: i32,
    hotkey: i32,
    trimming: i32,
    first_tab: f32,
    tabs: Vec<f32>,
    ranges: Vec<[i32; 2]>,
}
impl Format {
    pub(super) fn new(flags: i32) -> Self {
        Self {
            flags,
            language: 0,
            digits: 0,
            align: 0,
            line_align: 0,
            hotkey: 0,
            trimming: 1,
            first_tab: 0.,
            tabs: Vec::new(),
            ranges: Vec::new(),
        }
    }
    pub(super) fn generic(typographic: bool) -> Self {
        let mut value = Self::new(if typographic { 0x6004 } else { 0 });
        if typographic {
            value.trimming = 0;
        }
        value
    }
    pub(super) fn execute(&mut self, operation: Op<'_>) -> Result<Reply, i32> {
        match operation {
            Op::Align(value) if (0..=2).contains(&value) => self.align = value,
            Op::LineAlign(value) if (0..=2).contains(&value) => self.line_align = value,
            Op::Hotkey(value) if (0..=2).contains(&value) => self.hotkey = value,
            Op::Trimming(value) if (0..=5).contains(&value) => self.trimming = value,
            Op::StringFlags(value) => self.flags = value,
            Op::StringProperty(index) => {
                return Ok(Reply::Count(match index {
                    0 => self.flags,
                    1 => i32::from(self.language),
                    2 => self.align,
                    3 => self.line_align,
                    4 => self.hotkey,
                    5 => self.trimming,
                    6 => self.ranges.len() as i32,
                    7 => self.tabs.len() as i32,
                    _ => return Err(2),
                }));
            }
            Op::Digits(value) => {
                if let Some((language, method)) = value {
                    self.language = language;
                    self.digits = method;
                }
                return Ok(Reply::Digits(self.language, self.digits));
            }
            Op::Tabs(Some((first, tabs))) => {
                if first < 0. {
                    return Err(2);
                }
                // Native GDI+ does not support backwards tab advances.
                if tabs.iter().any(|value| *value < 0.) {
                    return Err(6);
                }
                let mut next = Vec::new();
                next.try_reserve_exact(tabs.len()).map_err(|_| 3)?;
                next.extend_from_slice(tabs);
                self.first_tab = first;
                self.tabs = next;
            }
            Op::Tabs(None) => return Ok(Reply::Tabs(self.first_tab, self.tabs.clone())),
            Op::CharacterRanges(ranges) => {
                if ranges.len() > 32 {
                    return Err(11);
                }
                self.ranges = ranges.to_vec();
            }
            _ => return Err(2),
        }
        Ok(Reply::None)
    }
}
