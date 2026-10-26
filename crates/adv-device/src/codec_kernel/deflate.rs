//! A complete zlib fast batch: Huffman symbols, extra bits and history copies.
//! Rejected data may change caller scratch, but never publishes stream state.

struct Bits<'a> {
    input: &'a [u8],
    used: usize,
    hold: u32,
    count: u32,
}
impl Bits<'_> {
    fn byte(&mut self) -> Option<()> {
        self.hold = self
            .hold
            .wrapping_add(u32::from(*self.input.get(self.used)?) << self.count);
        self.used += 1;
        self.count += 8;
        Some(())
    }
    fn pair(&mut self) -> Option<()> {
        if self.count < 15 {
            self.byte()?;
            self.byte()?;
        }
        Some(())
    }
    fn take(&mut self, count: u32) -> Option<u32> {
        while self.count < count {
            self.byte()?;
        }
        let value = self.hold & ((1 << count) - 1);
        self.hold >>= count;
        self.count -= count;
        Some(value)
    }
    fn symbol(&mut self, table: &[u8], root: u32) -> Option<(u8, usize)> {
        let mut index = (self.hold & ((1 << root) - 1)) as usize;
        loop {
            let code = table.get(index * 4..index * 4 + 4)?;
            let (op, count) = (code[0], u32::from(code[1]));
            if count == 0 || count > self.count {
                return None;
            }
            self.take(count)?;
            let value = usize::from(u16::from_le_bytes([code[2], code[3]]));
            if op == 0 || op & 0xf0 != 0 {
                return Some((op, value));
            }
            index = value + (self.hold & ((1 << op) - 1)) as usize;
        }
    }
}

/// Decode until the original fast loop's input/output threshold or block end.
/// History is chronological: two ring segments, then earlier caller output.
/// Tables contain zlib's four-byte code entries and their root bit widths.
/// Reservoir is (hold, bits). Success returns (input used, output used, block end).
/// On rejection only caller-owned output scratch may have changed (DEV-CODEC-2).
pub fn inflate_fast(
    input: &[u8],
    output: &mut [u8],
    history: [&[u8]; 3],
    tables: [(&[u8], u32); 2],
    reservoir: &mut (u32, u32),
) -> Option<(usize, usize, bool)> {
    let [(lcode, lbits), (dcode, dbits)] = tables;
    if input.len() < 6
        || output.len() < 258
        || reservoir.1 >= 8
        || !(1..=15).contains(&lbits)
        || !(1..=15).contains(&dbits)
    {
        return None;
    }
    let mut bits = Bits {
        input,
        used: 0,
        hold: reservoir.0,
        count: reservoir.1,
    };
    let history_len = history.iter().map(|part| part.len()).sum::<usize>();
    let mut used = 0;
    let mut ended = false;
    loop {
        bits.pair()?;
        let (op, value) = bits.symbol(lcode, lbits)?;
        if op == 0 {
            output[used] = value as u8;
            used += 1;
        } else if op & 16 != 0 {
            let length = value + bits.take(u32::from(op & 15))? as usize;
            if !(3..=258).contains(&length) {
                return None;
            }
            bits.pair()?;
            let (op, value) = bits.symbol(dcode, dbits)?;
            if op & 16 == 0 {
                return None;
            }
            let distance = value + bits.take(u32::from(op & 15))? as usize;
            if distance == 0 || distance > 32768 {
                return None;
            }
            let end = used + length;
            if distance > used {
                let back = distance - used;
                let mut at = history_len.checked_sub(back)?;
                for part in history {
                    if at >= part.len() {
                        at -= part.len();
                        continue;
                    }
                    let count = (part.len() - at).min(end - used);
                    output[used..used + count].copy_from_slice(&part[at..at + count]);
                    used += count;
                    at = 0;
                    if used == end {
                        break;
                    }
                }
            }
            if used < end {
                if distance == 1 {
                    let byte = output[used - 1];
                    output[used..end].fill(byte);
                } else {
                    // Seed with existing history, then double the repeated span.
                    let start = used;
                    let count = distance.min(end - used);
                    output.copy_within(used - distance..used - distance + count, used);
                    used += count;
                    while used < end {
                        let count = (used - start).min(end - used);
                        output.copy_within(start..start + count, used);
                        used += count;
                    }
                }
                used = end;
            }
        } else if op & 32 != 0 {
            ended = true;
            break;
        } else {
            return None;
        }
        if bits.used >= input.len() - 5 || used >= output.len() - 257 {
            break;
        }
    }
    // zlib returns speculative whole input bytes to its caller.
    let unused = bits.count / 8;
    bits.used = bits.used.checked_sub(unused as usize)?;
    bits.count -= unused * 8;
    bits.hold &= (1 << bits.count) - 1;
    *reservoir = (bits.hold, bits.count);
    Some((bits.used, used, ended))
}
