#[global_allocator]
static ALLOCATOR: adv_shell::CountingAllocator = adv_shell::CountingAllocator;

fn main() -> std::process::ExitCode {
    adv_shell::run_cli()
}
