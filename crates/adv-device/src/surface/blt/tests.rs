use super::*;
use MemoryClass::{System, Video};
use PixelFormat::Bgra32;

struct Memory(Vec<u8>);

impl SurfaceMemory for Memory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        let at = addr as usize;
        Ok(Cow::Borrowed(&self.0[at..at + len]))
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let at = addr as usize;
        self.0[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    fn writable(&self, _: u32, _: usize) -> Result<(), String> {
        Ok(())
    }

    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.0
            .copy_within(src as usize..src as usize + len, dst as usize);
        Ok(())
    }
}

#[path = "probe_cases.rs"]
mod probe_cases;
use probe_cases::{Case, EXPECTATIONS, Pixels, rows};

impl Pixels {
    fn load(&self, store: &mut SurfaceStore, m: &mut Memory) -> u32 {
        let (w, h) = self.size;
        let id = store.create(w, h, false, self.format, m).unwrap();
        let layout = &store[id];
        let bytes = self.format.bytes() as usize;
        for y in 0..h {
            for x in 0..w {
                let raw = self.data[(y * w + x) as usize].to_le_bytes();
                m.write(layout.row(x, y), &raw[..bytes]).unwrap();
            }
        }
        id
    }
}

fn hresult(error: BltError) -> u32 {
    match error {
        BltError::InvalidRect => 0x8876_0096,
        BltError::UnsupportedConversion => 0x8000_4001,
        BltError::ConversionFailed => 0x8000_4005,
        BltError::NoColorKeyHardware => 0x8876_00dc,
        BltError::InvalidArgument => 0x8007_0057,
    }
}

fn result_code(result: Result<Rect, BltFailure>) -> u32 {
    match result {
        Ok(_) => 0,
        Err(BltFailure::Rule(error)) => hresult(error),
        Err(BltFailure::Storage(error)) => panic!("{error}"),
    }
}

fn pixels(layout: &Surface, memory: &Memory) -> Vec<u32> {
    let bytes = layout.format.bytes() as usize;
    let mut result = Vec::new();
    for y in 0..layout.height {
        let row = layout.read_pixels(memory, 0, y, layout.width).unwrap();
        for pixel in row.chunks_exact(bytes) {
            let mut extended = [0; 4];
            extended[..bytes].copy_from_slice(pixel);
            if layout.format == Bgra32 {
                extended[3] = 0; // The probe prints X8R8G8B8 under its color mask.
            }
            result.push(u32::from_le_bytes(extended));
        }
    }
    result
}

fn probe(resident: bool) {
    let mut store = SurfaceStore::new(16, 16, 16, 16384, Snapshot::default()).unwrap();
    let mut memory = Memory(vec![0; 16400]);
    let pairs = [
        (Video, Video),
        (System, System),
        (System, Video),
        (Video, System),
    ];
    for (index, (src_class, dst_class)) in pairs.into_iter().enumerate() {
        let rules = BltRules::for_memory(src_class, dst_class);
        let mut key = Some(0x0101);
        for (name, expected, output) in rows(EXPECTATIONS, index) {
            if name == "hal" || name == "hel" {
                assert_eq!(expected, u32::from(rules.supports_key(BltKey::Source)) << 9);
                continue;
            }
            let label = (name, src_class, dst_class, resident);
            if name.starts_with("setcolorkey") {
                let result = match name {
                    "setcolorkey-range" => {
                        rules.set_source_key(&mut key, Some(1), BltKey::SourceRange)
                    }
                    "setcolorkey-0xff000101" => {
                        rules.set_source_key(&mut key, Some(0xff000101), BltKey::Source)
                    }
                    "setcolorkey-destblt" => {
                        rules.set_source_key(&mut key, Some(0x123456), BltKey::Destination)
                    }
                    _ => unreachable!(),
                };
                assert_eq!(result.err().map_or(0, hresult), expected, "{label:?}");
                continue;
            }
            if name.starts_with("getcolorkey") {
                assert_eq!(key, Some(expected), "{label:?}");
                assert_eq!(
                    key,
                    Some(u32::from_str_radix(output.unwrap(), 16).unwrap()),
                    "{label:?}"
                );
                continue;
            }
            let case = Case::from_probe(name, key);
            let dst = case.destination.load(&mut store, &mut memory);
            let src = if case.self_copy {
                dst
            } else {
                case.source.load(&mut store, &mut memory)
            };
            let source = BltSurface {
                id: src,
                rect: case.source_rect,
                memory: if case.self_copy { dst_class } else { src_class },
            };
            let destination = BltSurface {
                id: dst,
                rect: case.destination_rect,
                memory: dst_class,
            };
            let operation = if let Some(fill) = case.fill {
                Blt::Fill(fill)
            } else if case.destination_key {
                Blt::DestinationKey
            } else {
                Blt::Copy {
                    source,
                    mirror: case.mirror,
                    key: case.key,
                }
            };
            #[cfg(feature = "gpu")]
            if resident {
                for id in [src, dst] {
                    let full = store[id].rect();
                    store.gpu_access(id, full, Access::Read, &memory).unwrap();
                }
            }
            let result = result_code(store.blt(destination, operation, &mut memory));
            assert_eq!(result, expected, "{label:?}");
            let full = store[dst].rect();
            let layout = store
                .cpu_access(dst, full, Access::Read, &mut memory)
                .unwrap();
            let expected = output.map_or_else(
                || case.destination.data.clone(),
                |output| {
                    output
                        .split('.')
                        .map(|p| u32::from_str_radix(p, 16).unwrap())
                        .collect()
                },
            );
            assert_eq!(pixels(layout, &memory), expected, "{label:?}");
            store.delete(src);
            if dst != src {
                store.delete(dst);
            }
        }
    }
}

// spec: GFX-BLT-1
#[test]
fn cpu_matches_all_windows_blt_rows() {
    probe(false);
}

// spec: GFX-BLT-1
#[cfg(feature = "gpu")]
#[test]
fn gpu_matches_all_windows_blt_rows() {
    probe(true);
}

// spec: GFX-BLT-1 DEV-FRAME-13
#[cfg(feature = "gpu")]
#[test]
fn a_cpu_write_selects_the_cpu_until_its_cells_are_uploaded() {
    let mut store = SurfaceStore::new(24, 8, 16, 16384, Snapshot::default()).unwrap();
    let mut memory = Memory(vec![0; 16400]);
    let src = Pixels::source(Bgra32, (16, 8)).load(&mut store, &mut memory);
    let dst = Pixels::solid(Bgra32, (24, 8), 0).load(&mut store, &mut memory);
    let source = BltSurface {
        id: src,
        rect: store[src].rect(),
        memory: System,
    };
    let destination = BltSurface {
        id: dst,
        rect: store[dst].rect(),
        memory: System,
    };
    let stretch = Blt::Copy {
        source,
        mirror: [false; 2],
        key: None,
    };
    for blt in [source, destination] {
        store
            .gpu_access(blt.id, blt.rect, Access::Read, &memory)
            .unwrap();
    }
    let pixel = probe_cases::rectangle(0, 0, 1, 1);
    let layout = store
        .cpu_access(src, pixel, Access::Write, &mut memory)
        .unwrap();
    memory.put_u32(layout.pixels, 0xff112233).unwrap();
    let draws = store.gpu.draw_count();
    store.blt(destination, stretch, &mut memory).unwrap();
    assert_eq!(store.gpu.draw_count(), draws);
    let cpu = pixels(&store[dst], &memory);
    assert_eq!(cpu[0], 0x112233);

    for blt in [source, destination] {
        store
            .gpu_access(blt.id, blt.rect, Access::Read, &memory)
            .unwrap();
    }
    store.blt(destination, stretch, &mut memory).unwrap();
    assert_eq!(store.gpu.draw_count(), draws + 1);
    let layout = store
        .cpu_access(dst, destination.rect, Access::Read, &mut memory)
        .unwrap();
    assert_eq!(pixels(layout, &memory), cpu);
}
