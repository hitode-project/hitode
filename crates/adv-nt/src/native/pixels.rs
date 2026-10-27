//! Wine 11's normalized integer D3DX pixels, one row per guest write.
//! Float/compressed formats and aliases retain Wine's own body.

use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Env, Ext};

use crate::identity::{self, Images};
use std::collections::HashMap;

#[derive(Default)]
struct Prepared(HashMap<String, Binding>);
struct Binding {
    base: u32,
    memcpy: u32,
    helpers: Vec<(u32, Vec<u8>)>,
    key_format: Option<u32>,
}

pub(super) fn prepare(cpu: &dyn Guest, ext: &mut Ext, base: u32, pe: &adv_pe::Pe, relocs: &[u32]) {
    let Some(module) = ext.try_get::<Images>().and_then(|i| i.0.get(&base)) else {
        return;
    };
    if !module.name.starts_with("d3dx9_") {
        return;
    }
    let name = module.name.clone();
    let binding = (|| {
        let table = module.table()?;
        let mut helpers = Vec::new();
        for name in [
            "init_argb_conversion_info",
            "convert_argb_pixels",
            "convert_argb_pixel",
            "get_relevant_argb_components",
            "make_argb_color",
            "get_d3dx_pixel_format_info",
        ] {
            let f = table.named(name)?;
            let at = base + f.rva;
            if !identity::intact(cpu, at, f, |a| {
                relocs.binary_search(&a.wrapping_sub(base)).is_ok()
            }) {
                return None;
            }
            helpers.push((at, cpu.shared_bytes(at, f.size as usize)?.to_vec()));
        }
        let memcpy = pe
            .imports()
            .ok()?
            .into_iter()
            .flat_map(|m| m.imports)
            .find_map(|i| {
                matches!(i.name, adv_pe::ImportName::Name { ref name, .. } if name == "memcpy")
                    .then_some(base + i.iat_rva)
            })?;
        let getter = table.named("get_d3dx_pixel_format_info")?;
        let key_format = relocs
            .iter()
            .filter(|&&r| r >= getter.rva && r < getter.rva + getter.size)
            .find_map(|&r| {
                let at = cpu.read_u32(base + r).ok()?;
                let f = format(cpu, at)?;
                (f.id == 0 && f.bytes == 3 && f.bits == [0, 8, 8, 8] && f.shifts == [0, 16, 8, 0])
                    .then(|| at.checked_add(40))
                    .flatten()
            });
        Some(Binding {
            base,
            memcpy,
            helpers,
            key_format,
        })
    })();
    let prepared = &mut ext.get_or_default::<Prepared>().0;
    if let Some(binding) = binding {
        prepared.insert(name, binding);
    } else {
        prepared.remove(&name);
    }
}

fn binding<'a>(env: &'a Env<'_>) -> Option<&'a Binding> {
    let module = env.sys.thunks.get(env.thunk).name.split_once('!')?.0;
    env.ext.try_get::<Prepared>()?.0.get(module)
}
fn unchanged_helpers(env: &Env<'_>) -> Option<()> {
    let binding = binding(env)?;
    let module = env.ext.try_get::<Images>()?.0.get(&binding.base)?;
    if module.name != env.sys.thunks.get(env.thunk).name.split_once('!')?.0 {
        return None;
    }
    for (at, code) in &binding.helpers {
        if env.cpu.shared_bytes(*at, code.len())? != code {
            return None;
        }
    }
    let mut target = env.cpu.read_u32(binding.memcpy).ok()?;
    let module = env.ext.get::<Images>().containing(target)?;
    let table = module.table()?;
    let f = table.at(target - module.base)?;
    if f.rva != target - module.base {
        return None;
    }
    if f.name.as_ref() == "memcpy" && !env.sys.redirects.contains_key(&target) {
        // Wine's CRT memcpy is a direct jump to its admitted memmove.
        if identity::fnv1a64(env.cpu.shared_bytes(target, f.size as usize)?) as u32 != f.hash {
            return None;
        }
        let code = env.cpu.shared_bytes(target, 5)?;
        if code[0] != 0xe9 {
            return None;
        }
        target = target
            .wrapping_add(5)
            .wrapping_add(u32::from_le_bytes(code[1..5].try_into().ok()?));
        if table.at(target - module.base)?.name.as_ref() != "memmove" {
            return None;
        }
    } else if !matches!(f.name.as_ref(), "memcpy" | "memmove") {
        return None;
    }
    let (_, code) = env.sys.redirects.get(&target)?;
    (env.cpu.shared_bytes(target, code.len())? == code.as_ref()).then_some(())
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
// i386 d3dx_helpers.h; sizes and member offsets checked against Wine's DWARF.
struct Format {
    id: u32,
    bytes: u32,
    bits: [u8; 4],
    shifts: [u8; 4],
    alpha_type: u32,
    rgb_type: u32,
}
fn format(cpu: &dyn Guest, at: u32) -> Option<Format> {
    let mut b = [0; 40];
    cpu.read(at, &mut b).ok()?;
    let bytes = word(&b, 12);
    if word(&b, 0) >= 58
        || !(1..=4).contains(&bytes)
        || word(&b, 16) != 1
        || word(&b, 20) != 1
        || word(&b, 24) != bytes
        || word(&b, 28) > 1
        || word(&b, 32) > 1
        || word(&b, 36) != 0
    {
        return None;
    }
    let bits: [u8; 4] = b[4..8].try_into().ok()?;
    let shifts: [u8; 4] = b[8..12].try_into().ok()?;
    for i in 0..4 {
        if bits[i] > 16 || (bits[i] != 0 && u32::from(shifts[i]) + u32::from(bits[i]) > bytes * 8) {
            return None;
        }
    }
    Some(Format {
        id: word(&b, 0),
        bytes,
        bits,
        shifts,
        alpha_type: word(&b, 28),
        rgb_type: word(&b, 32),
    })
}
#[derive(Clone, Copy, Default)]
struct Channel {
    mask: u32,
    from: u32,
    to: u32,
    repeat: u32,
    trim: u32,
}
struct Conversion {
    channels: [Channel; 4],
    count: usize,
    fill: u32,
}
impl Conversion {
    fn new(src: &Format, dst: &Format) -> Self {
        let mut result = Self {
            channels: [Channel::default(); 4],
            count: 0,
            fill: 0,
        };
        for i in 0..4 {
            let (sb, db) = (u32::from(src.bits[i]), u32::from(dst.bits[i]));
            if db == 0 {
                continue;
            }
            if sb == 0 {
                result.fill |= ((1 << db) - 1) << dst.shifts[i];
                continue;
            }
            let bits = sb.min(db);
            let copies = db.div_ceil(bits);
            result.channels[result.count] = Channel {
                mask: (1 << bits) - 1,
                from: u32::from(src.shifts[i]) + sb.saturating_sub(db),
                to: u32::from(dst.shifts[i]),
                repeat: (0..copies).fold(0, |n, k| n | (1 << (k * bits))),
                trim: copies * bits - db,
            };
            result.count += 1;
        }
        result
    }
    fn pixel(&self, bytes: &[u8]) -> u32 {
        let mut input = [0; 4];
        input[..bytes.len()].copy_from_slice(bytes);
        let input = u32::from_le_bytes(input);
        self.channels[..self.count]
            .iter()
            .fold(self.fill, |out, c| {
                out | (((((input >> c.from) & c.mask) * c.repeat) >> c.trim) << c.to)
            })
    }
}
struct Pixels {
    at: u32,
    pitch: u32,
    width: u32,
    height: u32,
}
fn pixels(cpu: &dyn Guest, at: u32) -> Option<Pixels> {
    let mut b = [0; 44];
    cpu.read(at, &mut b).ok()?;
    if word(&b, 24) != 1 {
        return None;
    }
    Some(Pixels {
        at: word(&b, 0),
        pitch: word(&b, 4),
        width: word(&b, 16),
        height: word(&b, 20),
    })
}
fn span(p: &Pixels, bytes: u32) -> Option<u32> {
    let row = p.width.checked_mul(bytes)?;
    if row == 0 || row > p.pitch {
        return None;
    }
    let len = p
        .height
        .checked_sub(1)?
        .checked_mul(p.pitch)?
        .checked_add(row)?;
    p.at.checked_add(len)?;
    Some(len)
}
fn overlaps(a: u32, len: u32, b: u32, size: u32) -> bool {
    u64::from(a) < u64::from(b) + u64::from(size) && u64::from(b) < u64::from(a) + u64::from(len)
}

// spec: NAT-BODY-1, NAT-BODY-8
pub(super) fn load(env: &mut Env<'_>) -> Option<u64> {
    let args = [
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
    ];
    let filter = args[4] & 15;
    if !(1..=4).contains(&filter) {
        return None;
    }
    let (dst, src) = (pixels(env.cpu, args[0])?, pixels(env.cpu, args[2])?);
    let (df, sf) = (format(env.cpu, args[1])?, format(env.cpu, args[3])?);
    if sf.rgb_type != df.rgb_type && sf.alpha_type != df.alpha_type {
        return None;
    }
    let same_size = (dst.width, dst.height) == (src.width, src.height);
    let copy = same_size && df.id == sf.id && args[5] == 0;
    if copy && df.bytes != sf.bytes {
        return None;
    }
    if args[5] != 0 && sf.bits.iter().any(|&n| n != 0 && n != 8) {
        return None;
    }
    let row = dst.width.checked_mul(df.bytes)? as usize;
    let black_rows = filter == 1 && dst.height > src.height;
    let dspan = span(&dst, df.bytes)?;
    let (dlen, slen) = (
        if black_rows {
            dst.pitch.checked_mul(dst.height)?
        } else {
            dspan
        },
        span(&src, sf.bytes)?,
    );
    dst.at.checked_add(dlen)?;
    if !env.cpu.accessible(src.at, slen, Prot::READ)
        || !env.cpu.accessible(dst.at, dlen, Prot::WRITE)
        || overlaps(dst.at, dlen, src.at, slen)
        || [(args[0], 44), (args[1], 40), (args[2], 44), (args[3], 40)]
            .iter()
            .any(|&(at, len)| overlaps(dst.at, dlen, at, len))
    {
        return None;
    }
    unchanged_helpers(env)?;
    let key_format = if args[5] == 0 {
        None
    } else {
        let at = binding(env)?.key_format?;
        let f = format(env.cpu, at)?;
        if f.id != 1
            || f.bytes != 4
            || f.bits != [8; 4]
            || f.shifts != [24, 16, 8, 0]
            || overlaps(dst.at, dlen, at, 40)
        {
            return None;
        }
        Some(f)
    };
    env.cpu.shared_bytes(src.at, slen as usize)?;
    let mut stack = [0u8; 8192];
    let mut heap = Vec::new();
    let capacity = if black_rows { dst.pitch as usize } else { row };
    let output = if capacity <= stack.len() {
        &mut stack[..capacity]
    } else {
        heap.resize(capacity, 0);
        &mut heap[..]
    };
    let conversion = Conversion::new(&sf, &df);
    let key_conversion = key_format.as_ref().map(|f| Conversion::new(&sf, f));
    let key_mask = (0..4)
        .filter(|&i| sf.bits[i] != 0)
        .fold(0, |mask, i| mask | (255 << (24 - i * 8)));
    for y in 0..dst.height {
        if black_rows && y >= src.height {
            output.fill(0);
            env.cpu.write(dst.at + y * dst.pitch, output).ok()?;
            continue;
        }
        let sy = if filter == 1 || same_size {
            y
        } else {
            y.wrapping_mul(src.height) / dst.height
        };
        let source = env
            .cpu
            .shared_bytes(src.at + sy * src.pitch, (src.width * sf.bytes) as usize)?;
        if copy {
            output[..row].copy_from_slice(source);
        } else {
            for x in 0..dst.width {
                let out = &mut output
                    [x as usize * df.bytes as usize..(x + 1) as usize * df.bytes as usize];
                if filter == 1 && x >= src.width {
                    out.fill(0);
                    continue;
                }
                let sx = if filter == 1 || same_size {
                    x
                } else {
                    x.wrapping_mul(src.width) / dst.width
                };
                let at = (sx * sf.bytes) as usize;
                let input = &source[at..at + sf.bytes as usize];
                let mut value = conversion.pixel(input);
                if let Some(key) = &key_conversion
                    && key.pixel(input) & key_mask == args[5] & key_mask
                {
                    value = 0;
                }
                out.copy_from_slice(&value.to_le_bytes()[..df.bytes as usize]);
            }
        }
        env.cpu.write(dst.at + y * dst.pitch, &output[..row]).ok()?;
    }
    Some(0)
}
