//! Authored range-protection contracts and short, externally bounded controls.
//! Run contracts: cargo test -p adv-unicorn --test protect_ranges.
//! Run a size control from the built test executable with --ignored --exact
//! sixteen_mib_protect_and_unmap or thirty_two_mib_protect_and_unmap; supervise
//! each control with a five-second process deadline. Output is timing only.
use adv_cpu::backend::{Guest, Prot};
use adv_unicorn::UnicornCpu;
use std::time::Instant;

const BASE: u32 = 0x1000_0000;

fn control(size: u32) {
    let mut cpu = UnicornCpu::new(0x8000).unwrap();
    cpu.map(BASE, size, Prot::RW).unwrap();
    cpu.write(BASE, &[0x17]).unwrap();
    cpu.write(BASE + size - 1, &[0x39]).unwrap();
    let start = Instant::now();
    cpu.protect(BASE, size, Prot::READ).unwrap();
    println!(
        "size_bytes={size} protect_us={}",
        start.elapsed().as_micros()
    );
    assert!(cpu.accessible(BASE, size, Prot::READ));
    assert!(!cpu.accessible(BASE, size, Prot::WRITE));
    let mut byte = [0];
    cpu.read(BASE, &mut byte).unwrap();
    assert_eq!(byte, [0x17]);
    cpu.read(BASE + size - 1, &mut byte).unwrap();
    assert_eq!(byte, [0x39]);
    let start = Instant::now();
    cpu.unmap(BASE, size).unwrap();
    println!("size_bytes={size} unmap_us={}", start.elapsed().as_micros());
    assert!(!cpu.accessible(BASE, 1, Prot::default()));
}

#[test]
#[ignore = "short timing control; requires external five-second deadline"]
fn sixteen_mib_protect_and_unmap() {
    control(16 * 1024 * 1024);
}

#[test]
#[ignore = "short timing control; requires external five-second deadline"]
fn thirty_two_mib_protect_and_unmap() {
    control(32 * 1024 * 1024);
}

#[test]
#[ignore = "short timing control; requires external five-second deadline"]
fn sixteen_mib_unmap() {
    let mut cpu = UnicornCpu::new(0x8000).unwrap();
    let size = 16 * 1024 * 1024;
    cpu.map(BASE, size, Prot::RW).unwrap();
    let start = Instant::now();
    cpu.unmap(BASE, size).unwrap();
    println!("size_bytes={size} unmap_us={}", start.elapsed().as_micros());
    assert!(!cpu.accessible(BASE, 1, Prot::default()));
}

#[test]
fn adjacent_regions_keep_contents_and_outside_permissions() {
    let mut cpu = UnicornCpu::new(0x8000).unwrap();
    for page in 0..6 {
        cpu.map(BASE + page * 4096, 4096, Prot::RW).unwrap();
        cpu.write(BASE + page * 4096, &[page as u8]).unwrap();
    }
    cpu.protect(BASE + 4096, 4 * 4096, Prot::READ).unwrap();
    assert!(cpu.accessible(BASE, 4096, Prot::RW));
    assert!(cpu.accessible(BASE + 5 * 4096, 4096, Prot::RW));
    assert!(cpu.accessible(BASE + 4096, 4 * 4096, Prot::READ));
    assert!(!cpu.accessible(BASE + 4096, 4 * 4096, Prot::WRITE));
    for page in 0..6 {
        let mut byte = [0];
        cpu.read(BASE + page * 4096, &mut byte).unwrap();
        assert_eq!(byte, [page as u8]);
    }
    cpu.unmap(BASE + 4096, 4 * 4096).unwrap();
    assert!(cpu.accessible(BASE, 4096, Prot::RW));
    assert!(cpu.accessible(BASE + 5 * 4096, 4096, Prot::RW));
    assert!(!cpu.accessible(BASE + 4096, 1, Prot::default()));
}

#[test]
fn invalid_ranges_preserve_permissions_and_copy_metadata() {
    let mut cpu = UnicornCpu::new(0x8000).unwrap();
    cpu.map_private_copy_pages(BASE, &[0x52; 8192], Prot::RX)
        .unwrap();
    cpu.map(BASE + 3 * 4096, 4096, Prot::RW).unwrap();
    assert!(cpu.protect(BASE, 4 * 4096, Prot::RW).is_err());
    assert!(cpu.unmap(BASE, 4 * 4096).is_err());
    assert!(cpu.protect(BASE + 1, 4096, Prot::RW).is_err());
    assert!(cpu.unmap(BASE, 0).is_err());
    assert!(cpu.accessible(BASE, 8192, Prot::RX));
    assert!(!cpu.accessible(BASE, 8192, Prot::WRITE));
    assert_eq!(cpu.file_page_copy_on_write(BASE), Some(true));
    assert_eq!(cpu.file_page_copy_on_write(BASE + 4096), Some(true));
    cpu.unmap(BASE, 8192).unwrap();
    assert_eq!(cpu.file_page_copy_on_write(BASE), None);
    assert_eq!(cpu.file_page_copy_on_write(BASE + 4096), None);
    cpu.map(BASE, 8192, Prot::RW).unwrap();
    assert_eq!(cpu.file_page_copy_on_write(BASE), None);
}

#[test]
fn final_two_pages_do_not_wrap_the_range_end() {
    let mut cpu = UnicornCpu::new(0x8000).unwrap();
    cpu.map(0xffff_e000, 8192, Prot::RW).unwrap();
    cpu.protect(0xffff_e000, 8192, Prot::READ).unwrap();
    assert!(cpu.accessible(0xffff_e000, 8192, Prot::READ));
    assert!(!cpu.accessible(0xffff_e000, 8192, Prot::WRITE));
    cpu.unmap(0xffff_e000, 8192).unwrap();
    assert!(!cpu.accessible(0xffff_f000, 4096, Prot::default()));
}
