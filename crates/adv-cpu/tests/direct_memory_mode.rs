use adv_cpu::Executor;

#[test]
fn direct_memory_selection_reports_platform_and_lifecycle_limits() {
    let mut executor = Executor::new(0x10000, 4096).unwrap();
    let supported = cfg!(all(
        any(target_os = "linux", target_os = "android"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ));
    let selected = executor.set_direct_memory(true);
    if supported {
        selected.unwrap();
        executor.set_direct_memory(true).unwrap();
        assert!(executor.set_direct_memory(false).is_err());
    } else {
        assert!(selected.unwrap_err().contains("Linux/Android"));
        executor.set_direct_memory(false).unwrap();
    }
}
