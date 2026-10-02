//! End-to-end guest GDI/DDraw calls, native pixel drawing, and PNG presentation.
#[path = "support/pipeline.rs"]
mod pipeline;
use adv_jit::RunOutcome;
use pipeline::*;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/surface-demo".into());
    std::fs::create_dir_all(&out).unwrap();
    for ddraw in [false, true] {
        let mut f = fixture(ddraw, 640, 360, false);
        assert_eq!(
            f.machine
                .run_with_services(STOP, 2_000_000, &mut f.graphics)
                .unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(f.graphics.presenter.bgra, f.expected);
        let frame = &f.graphics.presenter;
        let mut rgba = frame.bgra.clone();
        for p in rgba.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
            p[3] = 255;
        }
        let name = if ddraw { "ddraw" } else { "gdi" };
        let path = std::path::Path::new(&out).join(format!("{name}.png"));
        let mut encoder = png::Encoder::new(
            std::fs::File::create(&path).unwrap(),
            frame.width,
            frame.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&rgba)
            .unwrap();
        println!(
            "{name}: frame={} API={} native_entries={} interpreter_entries={} output={}",
            frame.frames,
            f.machine.stats.host_calls,
            f.machine.stats.native_entries,
            f.machine.stats.interpreter_entries,
            path.display()
        );
    }
}
