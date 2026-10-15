// Run the authored fixture in isolated processes so backend selection never
// mutates the environment of concurrently running tests. Compare every byte.
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
fn emit_metal_pixels(pixels: &[u8]) {
    let bytes: String = pixels.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("\nMETAL_PIXELS:{bytes}");
}
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
fn compare_metal_fixture(fixture: &str) {
    let run = |wgpu: bool| {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([fixture, "--nocapture", "--test-threads=1"])
            .env("ADV_TEST_GPU_BACKEND", "metal")
            .env("MTL_DEBUG_LAYER", "1");
        if wgpu {
            command.env("ADV_GPU_BACKEND", "wgpu");
        } else {
            command.env_remove("ADV_GPU_BACKEND");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let pixels: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("METAL_PIXELS:").map(str::to_owned))
            .collect();
        assert!(!pixels.is_empty());
        pixels
    };
    assert_eq!(
        run(false),
        run(true),
        "native Metal and wgpu fixture bytes differ"
    );
}
