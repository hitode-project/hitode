//! Silent production-consumer benchmark. It never creates an Output or opens a DAC.
use adv_device::{AudioControl, ring::RingConfig};
use adv_sync::shared::SharedReadView;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
use std::time::Instant;

struct Allocator;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct View(Vec<AtomicU8>);
impl SharedReadView for View {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn load_u8(&self, index: usize) -> u8 {
        self.0[index].load(Ordering::Relaxed)
    }
}

fn voice(control: &AudioControl) {
    let view = Arc::new(View(
        (0..48_000)
            .flat_map(|frame| {
                (((frame as f64 * std::f64::consts::TAU * 997.0 / 48_000.0).sin() * 12_000.0)
                    .round() as i16)
                    .to_le_bytes()
            })
            .map(AtomicU8::new)
            .collect(),
    ));
    control
        .add_ring(
            1,
            RingConfig {
                bytes: view.len() as u32,
                view,
                channels: 1,
                bits: 16,
                frequency: 48_000,
                output_rate: 44_100,
                notifications: vec![],
            },
        )
        .unwrap();
    assert!(control.play_ring(1, true, 1));
}

fn counts() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::Relaxed),
        DEALLOCATIONS.load(Ordering::Relaxed),
    )
}

fn measure(samples: &mut Vec<u64>, action: impl FnOnce()) -> (u64, u64) {
    let before = counts();
    let started = Instant::now();
    action();
    samples.push(started.elapsed().as_nanos() as u64);
    let after = counts();
    (after.0 - before.0, after.1 - before.1)
}

fn report(rate: u32, name: &str, mut samples: Vec<u64>, allocations: u64, deallocations: u64) {
    samples.sort_unstable();
    let median = samples[samples.len() / 2] as f64 / 1000.0;
    let p99 = samples[samples.len() * 99 / 100] as f64 / 1000.0;
    println!(
        "{rate},{name},{},{median:.3},{p99:.3},{:.1},{:.1}",
        samples.len(),
        allocations as f64 / samples.len() as f64,
        deallocations as f64 / samples.len() as f64,
    );
}

fn run(rate: u32, name: &str, native_epoch: bool, reopen: bool, idle: bool) {
    let control = AudioControl::new(native_epoch);
    if !idle {
        voice(&control);
    }
    let stream = control.stream();
    let mut reader = Some(stream.device_reader(rate, 2).unwrap());
    let mut out = vec![0.0; rate as usize / 100 * 2];
    for _ in 0..8 {
        reader.as_mut().unwrap().read(&mut out, 0);
    }
    let mut times = Vec::with_capacity(400);
    let mut allocations = 0;
    let mut deallocations = 0;
    for _ in 0..400 {
        let measured = if reopen {
            let measured = measure(&mut times, || {
                drop(reader.take());
                reader = Some(stream.device_reader(rate, 2).unwrap());
            });
            reader.as_mut().unwrap().read(&mut out, 0);
            measured
        } else {
            measure(&mut times, || {
                reader.as_mut().unwrap().read(&mut out, 0);
            })
        };
        allocations += measured.0;
        deallocations += measured.1;
        std::hint::black_box(&out);
    }
    assert_eq!(stream.metrics().missing_output_frames, 0);
    report(rate, name, times, allocations, deallocations);
}

fn main() {
    // This is an entirely synthetic clock/consumer. No device feature is required.
    assert_eq!(std::env::var("ADV_AUDIO_MUTE_DEVICE").as_deref(), Ok("1"));
    println!("rate,operation,samples,median_us,p99_us,allocations,deallocations");
    for rate in [44_100, 48_000, 96_000, 192_000] {
        run(rate, "active_10ms_callback", false, false, false);
        run(rate, "idle_10ms_callback", false, false, true);
        run(rate, "continuous_virtual_prepare", false, true, false);
        run(rate, "new_native_epoch_prepare", true, true, false);
    }
}
