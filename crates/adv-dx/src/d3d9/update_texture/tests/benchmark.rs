//! Bounded synthetic SYSTEMMEM-to-DEFAULT timing, with no sample dependencies.
//! Run: cargo test -p adv-dx --features gpu --release benchmark_update_texture
//! -- --ignored --nocapture --test-threads=1. Output is numeric timings only.
use super::*;
use std::time::{Duration, Instant};

fn measure<T>(total: &mut Duration, action: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let value = action();
    *total += start.elapsed();
    value
}

#[test]
#[ignore = "explicit bounded upload timing"]
fn benchmark_update_texture() {
    const RUNS: u32 = 12;
    let mut f = Fixture::new();
    for (width, height, levels, format, dynamic) in [
        (1280, 720, 1, 21, false),
        (2048, 1024, 1, 21, false),
        (2048, 1024, 1, 22, false),
        (2048, 1024, 1, 26, false),
        (2048, 1024, 1, 28, false),
        (2048, 1024, 0, 21, false),
        (2048, 1024, 1, 21, true),
    ] {
        let source = f.texture(width, height, levels, 2, format);
        let output = f.packet(0u32);
        assert_eq!(
            f.invoke(
                "IDirect3DDevice9::CreateTexture",
                &[
                    f.device,
                    width,
                    height,
                    levels,
                    if dynamic { 0x200 } else { 0 },
                    format,
                    0,
                    output,
                    0
                ]
            ),
            0
        );
        let destination = f.m.cpu.read_u32(output).unwrap();
        let bytes = format::bytes(format).unwrap();
        f.fill(source, 0, &vec![0x73; bytes as usize], 0);
        assert_eq!(f.update(source, destination), 0);
        let mut read = Duration::ZERO;
        let mut copy = Duration::ZERO;
        let mut convert = Duration::ZERO;
        let mut upload = Duration::ZERO;
        let mut write = Duration::ZERO;
        let mut submit = Duration::ZERO;
        let mut wait = Duration::ZERO;
        let mut total = Duration::ZERO;
        let device = resources::resource(&env(&mut f.m), source).unwrap().device;
        let description = resources::resource(&env(&mut f.m), source)
            .unwrap()
            .levels
            .clone();
        let storage = resources::resource(&env(&mut f.m), source).unwrap().storage;
        for _ in 0..RUNS {
            assert_eq!(f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, 0]), 0);
            // The isolated primitives below measure cold allocation/copy costs;
            // the actual API timing uses its own bounded, warmed scratch buffers.
            let transfer = prepare(
                &env(&mut f.m),
                device,
                source,
                destination,
                &mut Scratch::default(),
            )
            .unwrap()
            .unwrap();
            for (level, planned) in description.iter().zip(transfer.remaining) {
                let pitch = format::pitch(format, level.desc.Width).unwrap();
                let length = format::length(format, level.desc.Width, level.desc.Height).unwrap();
                let mut raw = measure(&mut read, || {
                    let mut raw = vec![0; length as usize];
                    adv_abi::guest::check_range(
                        &f.m.cpu,
                        storage + level.offset,
                        length,
                        Prot::READ,
                    )
                    .unwrap();
                    f.m.cpu.read(storage + level.offset, &mut raw).unwrap();
                    raw
                });
                let duplicate = measure(&mut copy, || (format != 21).then(|| raw.clone()));
                let (pixels, pitch) = measure(&mut convert, || match duplicate {
                    Some(duplicate) => format::upload(
                        format,
                        duplicate,
                        pitch,
                        level.desc.Width,
                        level.desc.Height,
                    )
                    .unwrap(),
                    None => (Vec::new(), pitch),
                });
                let bgra = if format == 21 { &raw } else { &pixels };
                measure(&mut upload, || {
                    f.m.ext
                        .get::<State>()
                        .gpu
                        .as_ref()
                        .unwrap()
                        .upload(&planned.target.image, planned.rectangle, pitch, bgra)
                        .unwrap();
                });
                if let Some(address) = planned.commit.address {
                    measure(&mut write, || {
                        adv_abi::guest::check_range(&f.m.cpu, address, length, Prot::WRITE)
                            .unwrap();
                        f.m.cpu.write(address, &raw).unwrap();
                    });
                }
                raw.clear();
            }
            measure(&mut total, || assert_eq!(f.update(source, destination), 0));
            let image = f.image(destination, 0);
            let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
            measure(&mut submit, || {
                gpu.context().submission().unwrap().submit([]);
            });
            measure(&mut wait, || {
                let mut readback = gpu.readback(&image, None).unwrap();
                readback.take_completion_job().unwrap()();
                readback.poll().unwrap().unwrap();
            });
        }
        let us = |d: Duration| d.as_secs_f64() * 1e6 / f64::from(RUNS);
        println!(
            "utx width={width} height={height} levels={} format={format} dynamic={dynamic} runs={RUNS} read_us={:.2} clone_us={:.2} convert_us={:.2} upload_us={:.2} shadow_us={:.2} submit_us={:.2} drain_wait_us={:.2} api_us={:.2}",
            description.len(),
            us(read),
            us(copy),
            us(convert),
            us(upload),
            us(write),
            us(submit),
            us(wait),
            us(total)
        );
        assert_eq!(f.invoke("IDirect3DTexture9::Release", &[source]), 0);
        assert_eq!(f.invoke("IDirect3DTexture9::Release", &[destination]), 0);
    }
}
