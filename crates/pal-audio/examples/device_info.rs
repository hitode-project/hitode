//! Read-only default device configuration; does not build or play a stream.
#[cfg(feature = "device")]
fn main() {
    use cpal::traits::{DeviceTrait, HostTrait};
    let device = cpal::default_host().default_output_device().expect("default output device");
    let config = device.default_output_config().expect("default output configuration");
    println!(
        "name={:?} rate={} channels={} sample={:?} buffer={:?}",
        device.name(),
        config.sample_rate().0,
        config.channels(),
        config.sample_format(),
        config.buffer_size()
    );
}
#[cfg(not(feature = "device"))]
fn main() {
    eprintln!("enable --features device to query configuration");
}
