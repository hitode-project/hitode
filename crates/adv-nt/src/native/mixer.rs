//! Wine 11's mixer.c and dsound_convert.c, using the pinned PE's own FIR table.
//! Guest scratch allocations must already fit; otherwise Wine grows them.

use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::{Env, Outcome, Sys};

#[path = "mixer_layout.rs"]
mod layout;
use layout::{buffer as b, device as d, volume as v, *};

fn word(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn short(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
fn long(data: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
fn set(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn float(data: &[u8], at: usize) -> f32 {
    f32::from_bits(word(data, at))
}

// All memory is checked and computed in snapshots before the first guest write.
// Reject overlapping mutable regions: Wine's sample-by-sample aliasing then matters.
struct Memory<'a> {
    cpu: &'a mut dyn Guest,
    regions: Vec<(u32, Vec<u8>, bool)>,
}
impl Memory<'_> {
    fn load(&mut self, at: u32, len: u32, writable: bool) -> Option<Vec<u8>> {
        let end = at.checked_add(len)?;
        for (base, bytes, mutable) in &self.regions {
            if !writable && !*mutable && *base == at && bytes.len() == len as usize {
                return Some(bytes.clone());
            }
            if (writable || *mutable) && at < *base + bytes.len() as u32 && *base < end {
                return None;
            }
        }
        let prot = if writable { Prot::RW } else { Prot::READ };
        if !self.cpu.accessible(at, len, prot) {
            return None;
        }
        let mut bytes = vec![0; len as usize];
        self.cpu.read(at, &mut bytes).ok()?;
        self.regions.push((at, bytes.clone(), writable));
        Some(bytes)
    }
    fn save(&mut self, at: u32, bytes: Vec<u8>) {
        self.regions
            .iter_mut()
            .find(|r| r.0 == at && r.2)
            .unwrap()
            .1 = bytes;
    }
}

// Snapshot only the sampled window, rather than copying an entire music buffer.
fn window(
    mem: &mut Memory<'_>,
    at: u32,
    len: u32,
    pos: u32,
    size: u32,
    looping: bool,
) -> Option<Vec<u8>> {
    if pos >= len && !looping {
        return Some(Vec::new());
    }
    let pos = pos % len;
    let first = size.min(len - pos);
    let mut data = mem.load(at.checked_add(pos)?, first, false)?;
    if looping && first < size {
        let rest = mem.load(at, (size - first).min(len), false)?;
        while data.len() < size as usize {
            let take = rest.len().min(size as usize - data.len());
            data.extend_from_slice(&rest[..take]);
        }
    }
    Some(data)
}

/// Marshal GCC regparm(3) into stack arguments without expanding leaf CPU state.
pub(super) fn entry(cpu: &mut dyn Guest, sys: &mut Sys, thunk: u32) -> Result<u32, String> {
    let at = sys
        .vm
        .alloc(cpu, 0x1000, Prot::RX)
        .map_err(|e| e.to_string())?;
    // Save MXCSR and x87 controls, then push all_stopped, ECX, EDX, EAX.
    // The thunk sees four arguments followed by the two control words.
    let mut code = vec![
        0x83, 0xec, 8, 0x0f, 0xae, 0x1c, 0x24, 0xd9, 0x7c, 0x24, 4, 0xff, 0x74, 0x24, 12, 0x51,
        0x52, 0x50, 0xe8,
    ];
    code.extend_from_slice(&thunk.wrapping_sub(at + 23).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, 24, 0xc3]);
    cpu.write(at, &code).map_err(|e| e.to_string())?;
    Ok(at)
}

pub(super) fn resume(env: &mut Env<'_>) -> Outcome {
    // Discard the adapter's call frame, restoring the entry registers and caller.
    let sp = env.cpu.reg(Reg::Esp);
    for (reg, offset) in [(Reg::Eax, 4), (Reg::Edx, 8), (Reg::Ecx, 12)] {
        let Ok(value) = env.cpu.read_u32(sp + offset) else {
            return Outcome::Fail("unreadable mixer adapter frame".into());
        };
        env.cpu.set_reg(reg, value);
    }
    env.cpu.set_reg(Reg::Esp, sp + 28);
    super::resume_original(env)
}

// spec: NAT-BODY-3
pub(super) fn mix(env: &mut Env<'_>) -> Option<u64> {
    let (device, output, frames, stopped) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let module = env
        .ext
        .try_get::<crate::identity::Images>()?
        .0
        .values()
        .find(|m| m.name == "dsound.dll")?
        .clone();
    let table = module.table()?;
    let pe_address = |name: &str| table.named(name).map(|f| module.base + f.rva);
    // Function pointers can be hooked independently of the mixer itself.
    let nt = env
        .cpu
        .read_u32(module.base + 0x3c)
        .ok()?
        .checked_add(module.base)?;
    let reloc_at = env
        .cpu
        .read_u32(nt.checked_add(160)?)
        .ok()?
        .checked_add(module.base)?;
    let reloc_len = env.cpu.read_u32(nt.checked_add(164)?).ok()?;
    if !env.cpu.accessible(reloc_at, reloc_len, Prot::READ) {
        return None;
    }
    let mut reloc = vec![0; reloc_len as usize];
    env.cpu.read(reloc_at, &mut reloc).ok()?;
    let mut relocations = Vec::new();
    let mut at = 0;
    while at < reloc.len() {
        let header = reloc.get(at..at + 8)?;
        let block = word(header, 4) as usize;
        if block < 8 || !block.is_multiple_of(2) || at + block > reloc.len() {
            return None;
        }
        for offset in (at + 8..at + block).step_by(2) {
            let entry = short(&reloc, offset);
            if entry >> 12 == 3 {
                relocations.push(module.base + word(&reloc, at) + u32::from(entry & 0xfff));
            }
        }
        at += block;
    }
    let identify = |cpu: &dyn Guest, address: u32| -> Option<&str> {
        let f = table.at(address.checked_sub(module.base)?)?;
        if module.base + f.rva != address
            || !crate::identity::intact(cpu, address, f, |a| relocations.contains(&a))
        {
            return None;
        }
        Some(&f.name)
    };
    for name in [
        "secondarybuffer_is_audible",
        "mixieee32",
        "__divdi3",
        "__divmoddi4",
        "DSOUND_CheckEvent.part.0",
    ] {
        identify(env.cpu, pe_address(name)?)?;
    }
    // The adapter captures floating controls without expanding the leaf CPU view.
    if env.arg(4) & !0x3f != 0x1f80 || env.arg(5) & 0xc3f != 0x3f {
        return None;
    }
    let mut mem = Memory {
        cpu: env.cpu,
        regions: Vec::new(),
    };
    let dev = mem.load(device, 0x118, false)?;
    let count = word(&dev, d::nrofbuffers) as i32;
    let pointers = mem.load(
        word(&dev, d::buffers),
        (count.max(0) as u32).checked_mul(4)?,
        false,
    )?;
    let fmt = mem.load(word(&dev, d::pwfx), 16, false)?;
    let channels = u32::from(short(&fmt, wave::nChannels));
    if !(1..=2).contains(&channels) {
        return None;
    }
    let size = frames.checked_mul(channels)?.checked_mul(4)?;
    let mut out = mem.load(output, size, true)?;
    let mut all = mem.load(stopped, 4, true)?;
    set(&mut all, 0, 1);
    let mut locks = Vec::new();
    let mut temporary = None;
    let mut cache_buffer = None;
    for ptr in pointers.as_chunks::<4>().0 {
        let address = word(ptr, 0);
        let mut buf = mem.load(address, 0x140, true)?;
        let buflen = word(&buf, b::buflen);
        if buflen == 0 || word(&buf, b::state) == 0 {
            continue;
        }
        let lock = word(&buf, b::lock);
        if lock & 0xffff != 0 || lock >> 16 == 0xffff {
            return None;
        }
        if word(&buf, b::device) != device
            || word(&buf, b::dsbd + desc::dwFlags) & 0x10 != 0
            || word(&buf, b::num_filters) != 0
        {
            return None;
        }
        let get = word(&buf, b::get);
        let mono = Some(get) == pe_address("get_mono");
        let getter = if mono { word(&buf, b::get_aux) } else { get };
        let kind = match identify(mem.cpu, getter)? {
            "get8" => 1,
            "get16" => 2,
            "getieee32" => 4,
            _ => return None,
        };
        if mono {
            identify(mem.cpu, get)?;
        }
        let put = word(&buf, b::put);
        let duplicate = Some(put) == pe_address("put_mono2stereo");
        match identify(mem.cpu, put)? {
            "putieee32" => (),
            "put_mono2stereo"
                if channels == 2 && identify(mem.cpu, word(&buf, b::put_aux))? == "putieee32" => {}
            _ => return None,
        }
        let fmt = mem.load(word(&buf, b::pwfx), 16, false)?;
        let inputs = u32::from(short(&fmt, wave::nChannels));
        let stride = u32::from(short(&fmt, wave::nBlockAlign));
        let mix_channels = word(&buf, b::mix_channels);
        if !(1..=2).contains(&inputs)
            || stride < inputs * kind as u32
            || !buflen.is_multiple_of(stride)
            || mix_channels != if mono || duplicate { 1 } else { channels }
            || (!mono && mix_channels > inputs)
        {
            return None;
        }
        let num = long(&buf, b::freqAdjustNum);
        let den = long(&buf, b::freqAdjustDen);
        let acc = long(&buf, b::freqAccNum);
        if num <= 0 || den <= 0 || acc < 0 {
            return None;
        }
        let resample = num != den;
        let end = acc.checked_add(i64::from(frames).checked_mul(num)?)?;
        let advance = if resample {
            u32::try_from(end / den).ok()?
        } else {
            frames
        };
        let oldpos = word(&buf, b::sec_mixpos);
        let ipos = oldpos.checked_add(advance.checked_mul(stride)?)?;
        let looping = word(&buf, b::playflags) & 1 != 0;
        let newpos = if ipos < buflen {
            ipos
        } else if looping {
            ipos % buflen
        } else {
            0
        };
        let state = if ipos >= buflen && !looping {
            0
        } else if word(&buf, b::state) == 1 {
            2
        } else {
            word(&buf, b::state)
        };
        // DSOUND_CheckEvent: sorted stop notifications, then offsets in the traversed range.
        if word(&buf, b::dsbd + desc::dwFlags) & 0x100 != 0 {
            let n = word(&buf, b::nrofnotifies) as i32;
            let notifies = mem.load(
                word(&buf, b::notifies),
                (n.max(0) as u32).checked_mul(8)?,
                false,
            )?;
            let len = if newpos >= oldpos {
                newpos - oldpos
            } else {
                buflen + newpos - oldpos
            };
            for notify in notifies.as_chunks::<8>().0 {
                let offset = word(notify, notify::dwOffset);
                if (offset == u32::MAX && state == 0)
                    || (offset != u32::MAX
                        && ((offset >= oldpos && offset < oldpos + len)
                            || (oldpos + len > buflen && offset < (oldpos + len) % buflen)))
                {
                    return None;
                }
            }
        }
        let tmp_at = word(&dev, d::tmp_buffer);
        if tmp_at == 0 || word(&dev, d::tmp_buffer_len) < size {
            return None;
        }
        let mut tmp = match temporary.take() {
            Some(bytes) => bytes,
            None => mem.load(tmp_at, word(&dev, d::tmp_buffer_len), true)?,
        };
        let audible = (0..channels)
            .any(|c| word(&buf, b::volpan + v::dwTotalAmpFactor + c as usize * 4) != 0);
        if audible {
            let step = word(&buf, b::firstep) as usize;
            if resample && step == 0 {
                return None;
            }
            let cache = if resample {
                (FIR_LEN + step - 2) / step
            } else {
                0
            };
            let required = if resample {
                (advance as usize).checked_add(cache)?
            } else {
                frames as usize
            };
            let committed = word(&buf, b::use_committed) != 0;
            let commitpos = word(&buf, b::committed_mixpos);
            let lead = word(&buf, b::writelead);
            let committed_samples = if committed {
                (lead.checked_sub(commitpos)? / stride) as usize
            } else {
                0
            }
            .min(required);
            let storage = mem.load(word(&buf, b::buffer), 12, false)?;
            let source = window(
                &mut mem,
                word(&storage, memory::memory),
                buflen,
                oldpos,
                u32::try_from(required).ok()?.checked_mul(stride)?,
                looping,
            )?;
            let committed_data = if committed_samples > 0 {
                mem.load(
                    word(&buf, b::committedbuff).checked_add(commitpos)?,
                    (committed_samples as u32).checked_mul(stride)?,
                    false,
                )?
            } else {
                Vec::new()
            };
            let sample = |i: usize, channel: u32| -> Option<f32> {
                let (data, len, pos) = if i < committed_samples {
                    (
                        &committed_data,
                        lead,
                        commitpos.checked_add((i as u32).checked_mul(stride)?)?,
                    )
                } else {
                    (
                        &source,
                        buflen,
                        oldpos.checked_add((i as u32).checked_mul(stride)?)?,
                    )
                };
                if pos >= len && !looping {
                    return Some(0.0);
                }
                let at = i * stride as usize;
                let get = |c: u32| -> Option<f32> {
                    let at = at + c as usize * kind;
                    let bytes = data.get(at..at + kind)?;
                    let value = match kind {
                        1 => (i32::from(bytes[0]) - 128) as f32 / 128.0,
                        2 => i16::from_le_bytes(bytes.try_into().ok()?) as f32 / 32768.0,
                        _ => f32::from_le_bytes(bytes.try_into().ok()?),
                    };
                    value.is_finite().then_some(value)
                };
                if mono {
                    let mut sum = 0.0;
                    for c in 0..inputs {
                        sum += get(c)?;
                    }
                    Some(sum / inputs as f32)
                } else {
                    get(channel)
                }
            };
            let mut cp = Vec::new();
            let cp_at = word(&dev, d::cp_buffer);
            let fir = if resample {
                let len = (required * mix_channels as usize + cache).checked_mul(4)?;
                if cp_at == 0 || u32::try_from(len).ok()? > word(&dev, d::cp_buffer_len) {
                    return None;
                }
                cp = match cache_buffer.take() {
                    Some(bytes) => bytes,
                    None => mem.load(cp_at, word(&dev, d::cp_buffer_len), true)?,
                };
                for c in 0..mix_channels {
                    for i in 0..required {
                        set(
                            &mut cp,
                            (cache + c as usize * required + i) * 4,
                            sample(i, c)?.to_bits(),
                        );
                    }
                }
                mem.load(module.base + FIR_RVA, FIR_LEN as u32 * 4, false)?
            } else {
                Vec::new()
            };
            for i in 0..frames as usize {
                let mut used = 0;
                let mut input = i;
                if resample {
                    let steps = acc
                        .checked_add((i as i64).checked_mul(num)?)?
                        .checked_mul(step as i64)?;
                    let integer = u32::try_from(steps / den).ok()?;
                    input = integer as usize / step;
                    let total = steps as f32 / den as f32;
                    let rem = (f64::from(integer) + 1.0 - f64::from(total)) as f32;
                    let mut idx = (input + 1) * step - integer as usize - 1;
                    while idx < FIR_LEN - 1 {
                        // C promotes only the first product to double (1.0 is double).
                        let a = f64::from(float(&fir, idx * 4)) * (1.0 - f64::from(rem));
                        let z = float(&fir, (idx + 1) * 4) * rem;
                        set(&mut cp, used * 4, ((a + f64::from(z)) as f32).to_bits());
                        used += 1;
                        idx += step;
                    }
                    if used > cache || input + used > required {
                        return None;
                    }
                }
                for c in 0..mix_channels {
                    let value = if resample {
                        let mut sum = 0.0f32;
                        for j in 0..used {
                            sum += float(&cp, j * 4)
                                * float(&cp, (cache + c as usize * required + input + j) * 4);
                        }
                        sum * float(&buf, b::firgain)
                    } else {
                        sample(i, c)?
                    };
                    if !value.is_finite() {
                        return None;
                    }
                    set(
                        &mut tmp,
                        (i * channels as usize + c as usize) * 4,
                        value.to_bits(),
                    );
                    if duplicate {
                        set(&mut tmp, (i * channels as usize + 1) * 4, value.to_bits());
                    }
                }
            }
            if resample {
                cache_buffer = Some(cp);
            }
        }
        if resample {
            buf[b::freqAccNum..b::freqAccNum + 8].copy_from_slice(&(end % den).to_le_bytes());
        }
        set(&mut buf, b::state, state);
        set(&mut buf, b::sec_mixpos, newpos);
        if word(&buf, b::use_committed) != 0 {
            let pos = word(&buf, b::committed_mixpos).checked_add(advance.checked_mul(stride)?)?;
            set(&mut buf, b::committed_mixpos, pos);
            if pos >= word(&buf, b::writelead) {
                set(&mut buf, b::use_committed, 0);
            }
        }
        if audible {
            let flags = word(&buf, b::dsbd + desc::dwFlags);
            let volume = (flags & 0x80 != 0 && word(&buf, b::volpan + v::lVolume) != 0)
                || (flags & 0x40 != 0 && word(&buf, b::volpan + v::lPan) != 0);
            for i in 0..frames * channels {
                let at = i as usize * 4;
                let mut value = float(&tmp, at);
                if volume {
                    value *= word(
                        &buf,
                        b::volpan + v::dwTotalAmpFactor + (i % channels) as usize * 4,
                    ) as f32
                        / 65535.0;
                }
                set(&mut tmp, at, value.to_bits());
                let mixed = float(&out, at) + value;
                if !mixed.is_finite() {
                    return None;
                }
                set(&mut out, at, mixed.to_bits());
            }
        }
        temporary = Some(tmp);
        locks.push((address, lock));
        mem.save(address, buf);
        set(&mut all, 0, 0);
    }
    if let Some(bytes) = temporary {
        mem.save(word(&dev, d::tmp_buffer), bytes);
    }
    if let Some(bytes) = cache_buffer {
        mem.save(word(&dev, d::cp_buffer), bytes);
    }
    mem.save(output, out);
    mem.save(stopped, all);
    for (at, mut data, writable) in mem.regions {
        if writable {
            let lock = locks.iter().find(|l| l.0 == at);
            if let Some(&(_, value)) = lock {
                mem.cpu
                    .write_u32(at + b::lock as u32, value + 0x10000)
                    .ok()?;
                set(&mut data, b::lock, value + 0x10000);
            }
            mem.cpu.write(at, &data).ok()?;
            if let Some(&(_, value)) = lock {
                mem.cpu.write_u32(at + b::lock as u32, value).ok()?;
            }
        }
    }
    Some(0)
}
