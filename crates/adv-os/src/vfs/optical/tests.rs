use super::*;
use std::io::Write;

const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/optical.iso");

fn fixture(name: &str) -> (Vfs, std::path::PathBuf) {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/optical-vfs-tests")
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let image = directory.join("fixture.iso");
    std::fs::write(&image, FIXTURE).unwrap();
    let cfg = crate::OsConfig::new(directory.join("game"), "game.exe", directory.join("data"));
    let mut vfs = Vfs::new(&cfg);
    vfs.mount_iso('d', &image).unwrap();
    (vfs, directory)
}

#[test]
fn optical_fixture_lists_iso_and_joliet_names_and_rejects_writes() {
    let (mut vfs, directory) = fixture("directory");
    let info = vfs.optical_drive('d').unwrap();
    assert_eq!(info.label, "OPTICAL_TEST");
    assert_eq!(info.total_bytes, FIXTURE.len() as u64);
    assert_eq!(info.bytes_per_sector, 2048);
    assert_eq!(info.filesystem, "CDFS");
    let descriptor = &FIXTURE[17 * 2048..18 * 2048];
    let mut columns = [0u8; 4];
    for (i, byte) in descriptor.iter().enumerate() {
        columns[i % 4] = columns[i % 4].wrapping_add(*byte);
    }
    assert_eq!(info.serial, u32::from_be_bytes(columns));
    assert_eq!(
        vfs.optical_drives()
            .map(|(letter, _)| letter)
            .collect::<Vec<_>>(),
        ['D']
    );
    assert!(vfs.is_dir("D:/")); // privacy: allow authored guest optical path
    assert!(vfs.is_dir("d:/sUB")); // privacy: allow authored guest optical path
    assert!(vfs.is_optical_path("D:/missing/new.bin")); // privacy: allow authored guest optical path
    assert!(vfs.lookup("D:/ReadMe.txt").is_none()); // privacy: allow authored guest optical path
    let names: Vec<_> = vfs
        .list("D:/") // privacy: allow authored guest optical path
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["ReadMe.txt", "Sub"]);
    let names: Vec<_> = vfs
        .list("D:/sub") // privacy: allow authored guest optical path
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["split.bin", "日本.txt"]);
    assert_eq!(vfs.file_attributes("d:/readme.TXT").unwrap(), 0x21); // privacy: allow authored guest optical path
    assert_eq!(vfs.file_attributes("D:/Sub").unwrap(), 0x11); // privacy: allow authored guest optical path
    assert_eq!(
        vfs.file_attributes("D:/missing").unwrap_err().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::NotFound
    );
    assert_eq!(
        vfs.set_file_normal("D:/ReadMe.txt").unwrap_err().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::PermissionDenied
    );
    assert!(vfs.write_path("D:/new.bin").is_none()); // privacy: allow authored guest optical path
    assert!(vfs.mapping_write_path("D:/ReadMe.txt").is_none()); // privacy: allow authored guest optical path
    assert!(vfs.list("D:/ReadMe.txt").is_none()); // privacy: allow authored guest optical path
    assert!(vfs.open_optical_file("D:/").is_err()); // privacy: allow authored guest optical path
    // Primary-only discs retain the conventional name without ;1 or final dot.
    let mut primary = FIXTURE.to_vec();
    primary[17 * 2048..18 * 2048].fill(0);
    primary[17 * 2048..17 * 2048 + 7].copy_from_slice(&[255, b'C', b'D', b'0', b'0', b'1', 1]);
    let primary_path = directory.join("primary.iso");
    std::fs::write(&primary_path, primary).unwrap();
    vfs.mount_iso('e', primary_path).unwrap();
    assert_eq!(vfs.optical_drive('E').unwrap().max_component_length, 221);
    assert!(vfs.open_optical_file("E:/readme.txt").is_ok()); // privacy: allow authored guest optical path
    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn optical_fixture_streams_bound_reads_and_keep_cursors_independent() {
    let (vfs, directory) = fixture("streams");
    let mut first = vfs.open_optical_file("D:/README.txt").unwrap(); // privacy: allow authored guest optical path
    let mut second = vfs.open_optical_file("D:/readme.TXT").unwrap(); // privacy: allow authored guest optical path
    assert_eq!(first.cached_identity(), second.cached_identity());
    assert_eq!(first.metadata().unwrap().len(), 11);
    first.seek(SeekFrom::Start(6)).unwrap();
    let mut bytes = [0xcc; 16];
    assert_eq!(first.read(&mut bytes).unwrap(), 5);
    assert_eq!(&bytes[..5], b"world");
    assert_eq!(&bytes[5..], &[0xcc; 11]);
    assert_eq!(second.stream_position().unwrap(), 0);
    assert_eq!(second.read(&mut bytes[..5]).unwrap(), 5);
    assert_eq!(&bytes[..5], b"hello");
    assert_eq!(first.read(&mut bytes).unwrap(), 0);
    first.seek(SeekFrom::End(10)).unwrap();
    assert_eq!(first.read(&mut bytes).unwrap(), 0);
    assert!(first.seek(SeekFrom::Start(0)).is_ok());
    assert_eq!(
        first.seek(SeekFrom::Current(-1)).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        first.write(b"x").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        first.set_len(5).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut split = vfs.open_optical_file("D:/SUB/SPLIT.BIN").unwrap(); // privacy: allow authored guest optical path
    split.seek(SeekFrom::Start(3)).unwrap();
    assert_eq!(split.read(&mut bytes).unwrap(), 8);
    assert_eq!(&bytes[..8], b"de world");
    let mut unicode = vfs.open_optical_file("D:/Sub/日本.TXT").unwrap(); // privacy: allow authored guest optical path
    assert_eq!(unicode.read(&mut bytes).unwrap(), 5);
    assert_eq!(&bytes[..5], b"abcde");
    let section = vfs.sections.optical(split.optical_file().unwrap().clone());
    let page = section.page(0);
    assert_eq!(&page.snapshot().unwrap()[..11], b"abcde world");
    assert_eq!(split.stream_position().unwrap(), 11);
    let child_cfg =
        crate::OsConfig::new(directory.join("game"), "child.exe", directory.join("child"));
    let mut child = Vfs::new(&child_cfg);
    child.inherit_optical(&vfs);
    drop((vfs, first, second, split, unicode));
    let mut child_file = child.open_optical_file("D:/readme.txt").unwrap(); // privacy: allow authored guest optical path
    assert_eq!(child_file.read(&mut bytes).unwrap(), 11);
    assert_eq!(&bytes[..11], b"hello world");
    drop((child, child_file, section, page));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn optical_fixture_invalid_media_and_extents_fail_without_replacing_drive() {
    let (mut vfs, directory) = fixture("invalid");
    let serial = vfs.optical_drive('D').unwrap().serial;
    let invalid_path = directory.join("invalid.iso");
    std::fs::write(&invalid_path, b"not an image").unwrap();
    assert!(vfs.mount_iso('D', &invalid_path).is_err());
    assert_eq!(vfs.optical_drive('D').unwrap().serial, serial);
    assert!(vfs.mount_iso('C', directory.join("fixture.iso")).is_err());
    assert!(vfs.mount_iso('!', directory.join("fixture.iso")).is_err());
    let mut corrupt = FIXTURE.to_vec();
    // Joliet ReadMe's data extent is the third directory record.
    let record = 21 * 2048 + 68;
    corrupt[record + 2..record + 10].copy_from_slice(&[250, 0, 0, 0, 0, 0, 0, 250]);
    std::fs::write(&invalid_path, corrupt).unwrap();
    vfs.mount_iso('E', &invalid_path).unwrap();
    assert_eq!(
        vfs.open_optical_file("E:/ReadMe.txt").err().unwrap().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::InvalidData
    );
    let mut huge = FIXTURE.to_vec();
    huge[16 * 2048 + 132..16 * 2048 + 140].copy_from_slice(&[0xff; 8]);
    std::fs::write(&invalid_path, huge).unwrap();
    assert_eq!(
        vfs.mount_iso('F', &invalid_path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(vfs.optical_drive('F').is_none());
    let mut truncated_directory = FIXTURE.to_vec();
    truncated_directory[17 * 2048 + 166..17 * 2048 + 174]
        .copy_from_slice(&[40, 0, 0, 0, 0, 0, 0, 40]);
    std::fs::write(&invalid_path, truncated_directory).unwrap();
    vfs.mount_iso('G', &invalid_path).unwrap();
    assert_eq!(
        vfs.open_optical_file("G:/ReadMe.txt").err().unwrap().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::InvalidData
    );
    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "read-only local media probe; set ADV_TEST_ISO explicitly"]
fn optical_local_image_read_only_probe() {
    let path = std::env::var_os("ADV_TEST_ISO").expect("ADV_TEST_ISO");
    let cfg = crate::OsConfig::new("unused", "unused.exe", "unused");
    let mut vfs = Vfs::new(&cfg);
    vfs.mount_iso('D', path).unwrap();
    let info = vfs.optical_drive('D').unwrap();
    println!(
        "ISO label={} serial={:08x} capacity={}",
        info.label, info.serial, info.total_bytes
    );
    let root = vfs.list("D:/").unwrap(); // privacy: allow authored guest optical path
    println!("ISO root entries={}", root.len());
    for entry in &root {
        println!(
            "ISO entry name={:?} directory={} size={} modified={:?}",
            entry.name, entry.is_dir, entry.size, entry.modified
        );
    }
    let entry = root.iter().find(|entry| !entry.is_dir).unwrap();
    let mut file = vfs
        .open_optical_file(&format!("D:/{}", entry.name)) // privacy: allow authored guest optical path
        .unwrap();
    let mut bytes = [0; 4096];
    assert_eq!(
        file.read(&mut bytes).unwrap(),
        entry.size.min(4096) as usize
    );
    file.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(file.read(&mut bytes).unwrap(), 0);
}
