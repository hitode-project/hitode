use super::*;
use crate::ddraw::creation;
use adv_device::surface::Rect;

#[path = "../../../../../../adv-device/src/surface/blt/probe_cases.rs"]
mod probe_cases;
use probe_cases::{Case, EXPECTATIONS, Pixels, rows};

const DESC: u32 = DATA + 64;
const SRC_RECT: u32 = DATA + 256;
const DST_RECT: u32 = DATA + 272;
const FX: u32 = DATA + 320;

struct BltGuest {
    machine: Machine<JitCpu>,
    device: u32,
    done: u32,
}

impl BltGuest {
    fn invoke(&mut self, slot: u32, args: &[u32]) -> u32 {
        let m = &mut self.machine;
        let table = m.cpu.read_u32(args[0]).unwrap();
        let entry = m.cpu.read_u32(table + 4 * slot).unwrap();
        let mut code = Asm::new(CODE);
        for &arg in args.iter().rev() {
            code.push_imm(arg);
        }
        code.call(entry).mov_mem_eax(DATA).call(self.done).hlt();
        m.cpu.write(CODE, &code.bytes).unwrap();
        m.spawn("ddraw blt", CODE, &[], 65536).unwrap();
        assert!(matches!(m.run(), RunResult::Exited(0)));
        m.cpu.read_u32(DATA).unwrap()
    }

    fn word(&mut self, at: u32, value: u32) {
        self.machine.cpu.write_u32(at, value).unwrap();
    }

    fn read(&self, at: u32) -> u32 {
        self.machine.cpu.read_u32(at).unwrap()
    }

    fn load(&mut self, pixels: &Pixels, class: u32) -> u32 {
        let mut desc = [0; 124];
        let (w, h) = pixels.size;
        for (offset, word) in [(0, 124), (4, 0x1007), (8, h), (12, w), (104, 0x40 | class)] {
            desc[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        desc[72..104].copy_from_slice(&creation::pixel_format(pixels.format));
        self.machine.cpu.write(DESC, &desc).unwrap();
        assert_eq!(self.invoke(6, &[self.device, DESC, DATA + 4, 0]), 0);
        let surface = self.read(DATA + 4);
        let (addr, pitch) = self.lock(surface, 1);
        let bytes = pixels.format.bytes() as usize;
        let mut data = Vec::new();
        for (y, row) in pixels.data.chunks_exact(w as usize).enumerate() {
            data.clear();
            for pixel in row {
                data.extend_from_slice(&pixel.to_le_bytes()[..bytes]);
            }
            self.machine
                .cpu
                .write(addr + y as u32 * pitch, &data)
                .unwrap();
        }
        assert_eq!(self.invoke(32, &[surface, 0]), 0);
        surface
    }

    fn lock(&mut self, surface: u32, flags: u32) -> (u32, u32) {
        self.word(DESC, 124);
        assert_eq!(self.invoke(25, &[surface, 0, DESC, flags, 0]), 0);
        (self.read(DESC + 36), self.read(DESC + 16))
    }

    fn pixels(&self, addr: u32, pitch: u32, pixels: &Pixels) -> Vec<u32> {
        let (width, height) = pixels.size;
        let bytes = pixels.format.bytes();
        let mut values = Vec::new();
        let mut row = vec![0; (width * bytes) as usize];
        for y in 0..height {
            self.machine.cpu.read(addr + y * pitch, &mut row).unwrap();
            for p in row.chunks_exact(bytes as usize) {
                let mut raw = [0; 4];
                raw[..p.len()].copy_from_slice(p);
                if pixels.format == PixelFormat::Bgra32 {
                    raw[3] = 0;
                }
                values.push(u32::from_le_bytes(raw));
            }
        }
        values
    }

    fn set_key(&mut self, surface: u32, flags: u32, low: u32, high: u32) -> u32 {
        self.word(FX + 92, low);
        self.word(FX + 96, high);
        self.invoke(29, &[surface, flags, FX + 92])
    }

    fn rect(&mut self, at: u32, rect: Rect, full: (u32, u32)) -> u32 {
        if (rect.x, rect.y, rect.w, rect.h) == (0, 0, full.0, full.1) {
            return 0;
        }
        let right = if rect.w == 0 { 1 } else { rect.x + rect.w };
        for (index, value) in [rect.x, rect.y, right, rect.y + rect.h]
            .into_iter()
            .enumerate()
        {
            self.word(at + index as u32 * 4, value);
        }
        at
    }

    #[cfg(feature = "gpu")]
    fn upload(&mut self, ptr: u32) -> (u32, u32) {
        let m = &mut self.machine;
        let sid = m
            .ext
            .get::<front::State>()
            .draw
            .surface_id(&m.sys.objects, ptr)
            .unwrap();
        let store = m.ext.get_mut::<SurfaceStore>();
        let layout = &store[sid];
        let address = (layout.pixels, layout.pitch);
        store
            .gpu_access(
                sid,
                layout.rect(),
                surface::Access::Read,
                &memory(&mut m.cpu),
            )
            .unwrap();
        address
    }

    fn blt(&mut self, name: &str, case: &Case, src: u32, dst: u32) -> u32 {
        let sr = self.rect(SRC_RECT, case.source_rect, case.source.size);
        let dr = if name.starts_with("clipper-") {
            for (i, value) in [(-2i32) as u32, 0, 5, 2].into_iter().enumerate() {
                self.word(DST_RECT + i as u32 * 4, value);
            }
            DST_RECT
        } else {
            self.rect(DST_RECT, case.destination_rect, case.destination.size)
        };
        self.word(FX, 100);
        if name.starts_with("bltfast-") {
            let flags = if name.contains("-srccolorkey-") {
                17
            } else {
                16
            };
            return self.invoke(
                7,
                &[
                    dst,
                    case.destination_rect.x,
                    case.destination_rect.y,
                    src,
                    sr,
                    flags,
                ],
            );
        }
        let (flags, fx) = if let Some(color) = case.fill {
            self.word(FX + 80, color);
            (0x400, FX)
        } else if case.destination_key {
            (0x2000, 0)
        } else if name.starts_with("keysrcoverride") {
            self.word(FX + 92, case.key.unwrap());
            let high = if name == "keysrcoverride-distinct-bounds" {
                2
            } else {
                case.key.unwrap()
            };
            self.word(FX + 96, high);
            (0x10000, FX)
        } else if case.key.is_some() {
            (0x8000, 0)
        } else if case.mirror != [false; 2] {
            self.word(
                FX + 4,
                u32::from(case.mirror[0]) * 2 + u32::from(case.mirror[1]) * 4,
            );
            (0x800, FX)
        } else {
            (0, 0)
        };
        self.invoke(5, &[dst, dr, src, sr, flags | 0x01000000, fx])
    }
}

fn extra_case(name: &str) -> Case {
    use PixelFormat::{Bgr24, Bgra32, Indexed8, Rgb555, Rgb565};

    let format = |bits| match bits {
        "8" => Indexed8,
        "15" => Rgb555,
        "16" => Rgb565,
        "24" => Bgr24,
        "32" => Bgra32,
        _ => panic!("unknown format {bits}"),
    };
    let source = |format| Pixels {
        format,
        size: (4, 1),
        data: if format == Indexed8 {
            vec![1, 1, 0x81, 0xff]
        } else {
            vec![1, 0x80008001, 0xb8c4df, 0x7fffff]
        },
    };
    if let Some(bits) = name.strip_prefix("key-mask-") {
        let format = format(bits);
        let mut case = Case::copy(source(format), Pixels::solid(format, (4, 1), 0));
        case.key = Some(match format {
            Rgb555 => 0xffff8001,
            Bgr24 => 0xff008001,
            _ => 0xff000001,
        });
        return case;
    }
    if let Some((from, to)) = name
        .strip_prefix("supp-convert-")
        .and_then(|pair| pair.split_once("-to-"))
    {
        return Case::copy(source(format(from)), Pixels::solid(format(to), (4, 1), 0));
    }
    if name == "copy-555-raw" || name == "fill-555-raw" {
        let mut case = Case::copy(source(Rgb555), Pixels::solid(Rgb555, (4, 1), 0));
        case.fill = (name == "fill-555-raw").then_some(0x8123);
        return case;
    }

    let mut case = Case::stretch((4, 2), (4, 2));
    match name {
        "keysrcoverride-distinct-bounds" => case.key = Some(1),
        "bltfast-inverted-source" => case.source_rect = probe_cases::rectangle(3, 0, 0, 2),
        "bltfast-out-of-bounds-source" => case.source_rect.w = 5,
        "clipper-dest-past-edge" => {}
        "clipper-mirror-past-edge" => case.mirror[0] = true,
        "clipper-fill-past-edge" => case.fill = Some(0x123456),
        _ => panic!("unknown probe case {name}"),
    }
    case
}

fn set_clip_list(guest: &mut BltGuest, dst: u32) {
    const REGION: u32 = DATA + 1536;
    for (i, value) in [32, 1, 1, 16, 0, 0, 4, 2, 0, 0, 4, 2]
        .into_iter()
        .enumerate()
    {
        guest.word(REGION + i as u32 * 4, value);
    }
    assert_eq!(guest.invoke(4, &[guest.device, 0, DATA + 4, 0]), 0);
    let clipper = guest.read(DATA + 4);
    assert_eq!(guest.invoke(7, &[clipper, REGION, 0]), 0);
    assert_eq!(guest.invoke(28, &[dst, clipper]), 0);
    assert_eq!(guest.invoke(2, &[clipper]), 1);
}

fn overlay_key(guest: &mut BltGuest, name: &str, ptr: u32, expected: u32, output: &str) {
    let flags = match name.trim_end_matches("-clear").trim_end_matches("-range") {
        "source-overlay" => 0x10,
        "dest-overlay" => 4,
        "dest-blt" => 2,
        "source-blt" | "source" => 8,
        _ => panic!("unknown key case {name}"),
    };
    let set_flags = flags | u32::from(name.contains("-range"));
    let result = if name.ends_with("-clear") {
        guest.invoke(29, &[ptr, set_flags, 0])
    } else {
        guest.set_key(ptr, set_flags, 1, 2)
    };
    assert_eq!(result, expected, "{name}");
    guest.word(FX + 92, 0xdead);
    guest.word(FX + 96, 0xbeef);
    let get = guest.invoke(16, &[ptr, flags, FX + 92]);
    let actual = [get, guest.read(FX + 92), guest.read(FX + 96)];
    assert_eq!(actual.as_slice(), parse_pixels(output), "{name}");
}

fn parse_pixels(output: &str) -> Vec<u32> {
    output
        .split('.')
        .map(|pixel| u32::from_str_radix(pixel, 16).unwrap())
        .collect()
}

fn guest_probe(resident: bool) {
    let mut registry = Registry::default();
    registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    let (mut machine, device) = setup(false, false, registry);
    let done = machine.resolve_import("test", "done").unwrap();
    let mut guest = BltGuest {
        machine,
        device,
        done,
    };
    let (hal, hel) = (DATA + 512, DATA + 1024);
    guest.word(hal, 380);
    guest.word(hel, 380);
    assert_eq!(guest.invoke(11, &[device, hal, hel]), 0);
    for (index, (src_class, dst_class)) in [
        (0x4000, 0x4000),
        (0x800, 0x800),
        (0x800, 0x4000),
        (0x4000, 0x800),
    ]
    .into_iter()
    .enumerate()
    {
        let keyed = guest.load(&Pixels::source(PixelFormat::Bgra32, (4, 2)), src_class);
        assert_eq!(guest.set_key(keyed, 8, 0x101, 0x101), 0);
        let key_destination = guest.load(&Pixels::source(PixelFormat::Bgra32, (4, 2)), dst_class);
        let cases = rows(EXPECTATIONS, index)
            .map(|row| (row, false))
            .chain(rows(include_str!("blt_extra.tsv"), index).map(|row| (row, true)));
        for ((name, expected, output), extra) in cases {
            let label = (name, src_class, dst_class, resident);
            match name {
                "hal" | "hel" => {
                    assert_eq!(
                        guest.read(if name == "hal" { hal + 12 } else { hel + 12 }),
                        expected
                    );
                    continue;
                }
                "setcolorkey-range" | "setcolorkey-0xff000101" | "setcolorkey-destblt" => {
                    let result = match name {
                        "setcolorkey-range" => guest.set_key(keyed, 9, 1, 0x102),
                        "setcolorkey-0xff000101" => guest.set_key(keyed, 8, 0xff000101, 0xff000101),
                        _ => {
                            let dst =
                                guest.load(&Pixels::source(PixelFormat::Bgra32, (8, 4)), dst_class);
                            let result = guest.set_key(dst, 2, 0x123456, 0x123456);
                            assert_eq!(guest.invoke(2, &[dst]), 0);
                            result
                        }
                    };
                    assert_eq!(result, expected, "{label:?}");
                    continue;
                }
                "getcolorkey-after-0xff000101" => {
                    assert_eq!(guest.invoke(16, &[keyed, 8, FX + 92]), 0);
                    assert_eq!(guest.read(FX + 92), expected);
                    assert_eq!(
                        guest.read(FX + 96),
                        u32::from_str_radix(output.unwrap(), 16).unwrap()
                    );
                    continue;
                }
                _ => {}
            }
            if let Some(name) = name.strip_prefix("key-api-") {
                let ptr = if name.starts_with("dest-") {
                    key_destination
                } else {
                    keyed
                };
                overlay_key(&mut guest, name, ptr, expected, output.unwrap());
                continue;
            }
            let case = if extra {
                extra_case(name)
            } else {
                Case::from_probe(name, Some(0x101))
            };
            let dst = guest.load(&case.destination, dst_class);
            let src = if case.fill.is_some() {
                0
            } else if case.self_copy {
                dst
            } else if name.starts_with("keysrc-") {
                keyed
            } else {
                guest.load(&case.source, src_class)
            };
            if name.starts_with("clipper-") {
                set_clip_list(&mut guest, dst);
            }
            if name.starts_with("key-mask-") {
                let key = case.key.unwrap();
                assert_eq!(guest.set_key(src, 8, key, key), 0);
            }
            if name.starts_with("bltfast-srccolorkey-") {
                assert_eq!(guest.set_key(src, 8, 1, 1), 0);
            }
            #[cfg(feature = "gpu")]
            let old = resident.then(|| {
                if src != 0 && src != dst {
                    guest.upload(src);
                }
                guest.upload(dst)
            });

            assert_eq!(guest.blt(name, &case, src, dst), expected, "{label:?}");
            #[cfg(feature = "gpu")]
            if let Some((addr, pitch)) = old {
                let initial: Vec<_> = case
                    .destination
                    .data
                    .iter()
                    .map(|&pixel| {
                        if case.destination.format == PixelFormat::Bgra32 {
                            pixel & 0xffffff
                        } else {
                            pixel
                        }
                    })
                    .collect();
                // Until Lock synchronizes the CPU copy, only GPU storage has changed.
                assert_eq!(
                    guest.pixels(addr, pitch, &case.destination),
                    initial,
                    "{label:?}"
                );
            }
            let (addr, pitch) = guest.lock(dst, 0x11);
            let observed = guest.pixels(addr, pitch, &case.destination);
            assert_eq!(guest.invoke(32, &[dst, 0]), 0);
            let expected = output.map_or_else(|| case.destination.data.clone(), parse_pixels);
            assert_eq!(observed, expected, "{label:?}");
            if src != 0 && src != dst && src != keyed {
                assert_eq!(guest.invoke(2, &[src]), 0);
            }
            assert_eq!(guest.invoke(2, &[dst]), 0);
        }
        assert_eq!(guest.invoke(2, &[keyed]), 0);
        assert_eq!(guest.invoke(2, &[key_destination]), 0);
    }
}

// spec: GFX-BLT-1 GFX-DDRAW-4
#[test]
fn guest_blt_matches_windows_on_cpu() {
    guest_probe(false);
}

// spec: GFX-BLT-1 GFX-DDRAW-4
#[cfg(feature = "gpu")]
#[test]
fn guest_blt_matches_windows_on_gpu() {
    guest_probe(true);
}
