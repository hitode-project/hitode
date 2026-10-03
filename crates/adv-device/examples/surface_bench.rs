//! Headless pixel-store benchmark. No guest program, window, audio or device is opened.
use adv_device::surface::{PixelFormat, Snapshot, SurfaceMemory, Surfaces};
use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Allocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct Memory(Vec<u8>);
impl SurfaceMemory for Memory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        self.0
            .get(addr as usize..addr as usize + len)
            .map(Cow::Borrowed)
            .ok_or_else(|| "read out of bounds".into())
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let dst = self
            .0
            .get_mut(addr as usize..addr as usize + data.len())
            .ok_or("write out of bounds")?;
        dst.copy_from_slice(data);
        Ok(())
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        self.read(addr, len).map(|_| ())
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.writable(src, len)?;
        self.writable(dst, len)?;
        self.0
            .copy_within(src as usize..src as usize + len, dst as usize);
        Ok(())
    }
}

fn measure(mut f: impl FnMut(), frames: usize) -> (f64, u64) {
    for _ in 0..4 {
        f();
    }
    let mut times = Vec::with_capacity(frames);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..frames {
        let start = Instant::now();
        f();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    times.sort_by(f64::total_cmp);
    (times[frames / 2], allocations / frames as u64)
}

fn main() {
    let frames: usize = std::env::args().nth(1).map_or(9, |n| n.parse().unwrap());
    assert!(frames > 0);
    println!("operation,resolution,median_ms,allocations_per_frame");
    for (w, h) in [(640, 480), (1280, 720)] {
        let mut memory = Memory(vec![0; 32 * 1024 * 1024]);
        let mut surfaces =
            Surfaces::new(w, h, 16, (memory.0.len() - 16) as u32, Snapshot::default()).unwrap();
        let src = surfaces
            .create(w, h, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        let dst = surfaces
            .create(w, h, false, PixelFormat::Rgb565, &mut memory)
            .unwrap();
        let rect = surfaces[src].rect();
        let source = surfaces[src].pixels as usize;
        for p in memory.0[source..source + (w * h * 4) as usize].chunks_exact_mut(4) {
            p.copy_from_slice(&[255, 0, 255, 0]);
        }
        let (ms, allocations) = measure(
            || surfaces.copy(src, rect, dst, rect, &mut memory).unwrap(),
            frames,
        );
        assert_eq!(
            memory.read(surfaces[dst].pixels, 2).unwrap().as_ref(),
            &[0x1f, 0xf8]
        );
        println!("bgra_to_565,{w}x{h},{ms:.4},{allocations}");
        let scaled = adv_device::surface::Rect {
            x: 0,
            y: 0,
            w: w / 2,
            h: h / 2,
        };
        let (ms, allocations) = measure(
            || surfaces.copy(src, rect, dst, scaled, &mut memory).unwrap(),
            frames,
        );
        assert_eq!(
            memory.read(surfaces[dst].pixels, 2).unwrap().as_ref(),
            &[0x1f, 0xf8]
        );
        println!("scale_bgra_to_565,{w}x{h},{ms:.4},{allocations}");
        let (ms, allocations) = measure(|| surfaces.present(dst, &memory).unwrap(), frames);
        assert!(
            surfaces
                .presenter
                .bgra
                .chunks_exact(4)
                .all(|p| p == [255, 0, 255, 0])
        );
        println!("present_565,{w}x{h},{ms:.4},{allocations}");
        surfaces.set_deferred_presentation(true).unwrap();
        let before = surfaces.stats.converted_pixels;
        let (ms, allocations) = measure(|| surfaces.present(dst, &memory).unwrap(), frames);
        assert_eq!(surfaces.stats.converted_pixels, before);
        assert!(surfaces.capture_latest().unwrap());
        assert!(!surfaces.capture_latest().unwrap());
        assert!(
            surfaces
                .presenter
                .bgra
                .chunks_exact(4)
                .all(|p| p == [255, 0, 255, 0])
        );
        println!("defer_565,{w}x{h},{ms:.4},{allocations}");
    }
}
