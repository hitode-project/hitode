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
    vfs.mount_disc('d', &image).unwrap();
    (vfs, directory)
}

#[test]
// spec: DEV-DISC-4, DEV-DISC-6, DEV-DISC-7
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
    vfs.set_current_directory("D:/Sub").unwrap(); // privacy: allow authored guest optical path
    assert_eq!(
        vfs.open_optical_file("日本.txt")
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        vfs.set_current_directory("日本.txt"),
        Err(super::super::DirectoryError::NotDirectory)
    );
    assert_eq!(
        vfs.set_current_directory("missing"),
        Err(super::super::DirectoryError::NameNotFound)
    );
    assert!(vfs.open_optical_file("日本.txt").is_ok());
    // Primary-only discs retain the conventional name without ;1 or final dot.
    let mut primary = FIXTURE.to_vec();
    primary[17 * 2048..18 * 2048].fill(0);
    primary[17 * 2048..17 * 2048 + 7].copy_from_slice(&[255, b'C', b'D', b'0', b'0', b'1', 1]);
    let primary_path = directory.join("primary.iso");
    std::fs::write(&primary_path, primary).unwrap();
    vfs.mount_disc('e', primary_path).unwrap();
    assert_eq!(vfs.optical_drive('E').unwrap().max_component_length, 221);
    assert!(vfs.open_optical_file("E:/readme.txt").is_ok()); // privacy: allow authored guest optical path
    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
// spec: DEV-DISC-3, DEV-DISC-4
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
// spec: DEV-DISC-1
fn optical_fixture_invalid_media_and_extents_fail_without_replacing_drive() {
    let (mut vfs, directory) = fixture("invalid");
    let serial = vfs.optical_drive('D').unwrap().serial;
    let invalid_path = directory.join("invalid.iso");
    std::fs::write(&invalid_path, b"not an image").unwrap();
    assert!(vfs.mount_disc('D', &invalid_path).is_err());
    assert_eq!(vfs.optical_drive('D').unwrap().serial, serial);
    assert!(vfs.mount_disc('C', directory.join("fixture.iso")).is_err());
    assert!(vfs.mount_disc('!', directory.join("fixture.iso")).is_err());
    let mut corrupt = FIXTURE.to_vec();
    // Joliet ReadMe's data extent is the third directory record.
    let record = 21 * 2048 + 68;
    corrupt[record + 2..record + 10].copy_from_slice(&[250, 0, 0, 0, 0, 0, 0, 250]);
    std::fs::write(&invalid_path, corrupt).unwrap();
    vfs.mount_disc('E', &invalid_path).unwrap();
    assert_eq!(
        vfs.open_optical_file("E:/ReadMe.txt").err().unwrap().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::InvalidData
    );
    let mut huge = FIXTURE.to_vec();
    huge[16 * 2048 + 132..16 * 2048 + 140].copy_from_slice(&[0xff; 8]);
    std::fs::write(&invalid_path, huge).unwrap();
    assert_eq!(
        vfs.mount_disc('F', &invalid_path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(vfs.optical_drive('F').is_none());
    let mut truncated_directory = FIXTURE.to_vec();
    truncated_directory[17 * 2048 + 166..17 * 2048 + 174]
        .copy_from_slice(&[40, 0, 0, 0, 0, 0, 0, 40]);
    std::fs::write(&invalid_path, truncated_directory).unwrap();
    vfs.mount_disc('G', &invalid_path).unwrap();
    assert_eq!(
        vfs.open_optical_file("G:/ReadMe.txt").err().unwrap().kind(), // privacy: allow authored guest optical path
        io::ErrorKind::InvalidData
    );
    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[ignore = "read-only local media probe; set ADV_TEST_DISC explicitly"]
fn optical_local_image_read_only_probe() {
    let path = std::env::var_os("ADV_TEST_DISC").expect("ADV_TEST_DISC");
    let cfg = crate::OsConfig::new("unused", "unused.exe", "unused");
    let mut vfs = Vfs::new(&cfg);
    vfs.mount_disc('D', path).unwrap();
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

/// The fixture's 2048-byte sectors as raw sectors of `stride` bytes.
fn raw(mode: u8, stride: usize) -> Vec<u8> {
    let mut image = Vec::new();
    for (lba, data) in FIXTURE.chunks(2048).enumerate() {
        let mut sector = vec![0; stride];
        sector[..12].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0]);
        let frame = lba + 150;
        let bcd = |value: usize| (value / 10 * 16 + value % 10) as u8;
        sector[12..15].copy_from_slice(&[bcd(frame / 4500), bcd(frame / 75 % 60), bcd(frame % 75)]);
        sector[15] = mode;
        let at = if mode == 2 { 24 } else { 16 };
        sector[at..at + 2048].copy_from_slice(data);
        image.extend(sector);
    }
    image
}

/// Reads the whole logical image through buffers that cross sector and batch bounds.
fn logical(path: &Path) -> Vec<u8> {
    let mut disc = image::Disc::open(path).unwrap();
    let mut bytes = Vec::new();
    for size in [3000, 100_000].into_iter().cycle() {
        let mut buffer = vec![0; size];
        let count = disc.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    disc.seek(SeekFrom::Start(2047)).unwrap();
    let mut pair = [0; 2];
    disc.read_exact(&mut pair).unwrap();
    assert_eq!(pair, FIXTURE[2047..2049]);
    bytes
}

fn be32(value: u64) -> [u8; 4] {
    (value as u32).to_be_bytes()
}

/// A Nero image: track data, then its block list and trailer.
fn nero(wide: bool, dao: bool) -> Vec<u8> {
    let pregap = vec![0; 150 * 2352];
    let track = raw(1, 2352);
    let start = pregap.len() as u64;
    let end = start + track.len() as u64;
    let mut image = [pregap, track].concat();
    let trailer = image.len() as u64;
    let field = |value: u64| {
        if wide {
            value.to_be_bytes().to_vec()
        } else {
            be32(value).to_vec()
        }
    };
    let (id, body): (&[u8], Vec<u8>) = if dao {
        let mut body = vec![0; 22];
        body.extend([0; 12]);
        body.extend(2352u16.to_be_bytes());
        body.extend([0x05, 0, 0, 0]);
        body.extend([field(0), field(start), field(end)].concat());
        (if wide { b"DAOX" } else { b"DAOI" }, body)
    } else {
        let mut body = [field(start), field(end - start)].concat();
        body.extend([0, 0, 0, 0x05]);
        body.extend([0; 4]);
        body.extend(vec![0; if wide { 8 } else { 4 }]);
        (if wide { b"ETN2" } else { b"ETNF" }, body)
    };
    image.extend(b"SINF");
    image.extend(be32(4));
    image.extend(be32(1));
    image.extend(id);
    image.extend(be32(body.len() as u64));
    image.extend(body);
    image.extend(b"END!");
    image.extend(be32(0));
    if wide {
        image.extend(b"NER5");
        image.extend(trailer.to_be_bytes());
    } else {
        image.extend(b"NERO");
        image.extend(be32(trailer));
    }
    image
}

#[test]
// spec: DEV-DISC-4, DEV-DISC-5
fn optical_containers_present_the_first_data_track_as_logical_sectors() {
    let (_, directory) = fixture("containers");
    let write = |name: &str, bytes: &[u8]| {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };
    // Lone raw sectors in Mode 1, Mode 2 Form 1 and with interleaved subchannel.
    assert_eq!(logical(&write("mode1.bin", &raw(1, 2352))), FIXTURE);
    assert_eq!(logical(&write("mode2.img", &raw(2, 2352))), FIXTURE);
    assert_eq!(logical(&write("sub.mdf", &raw(1, 2448))), FIXTURE);
    // Descriptors that name a same-stem sibling.
    write("sub.mds", b"MEDIA DESCRIPTOR");
    assert_eq!(logical(&directory.join("sub.mds")), FIXTURE);
    write("mode2.ccd", b"[CloneCD]\r\nVersion=3\r\n");
    assert_eq!(logical(&directory.join("mode2.ccd")), FIXTURE);
    // A mixed-mode CUE: stored pregap, then an audio track in the same file.
    let mut mixed = vec![0; 150 * 2352];
    mixed.extend(raw(1, 2352));
    mixed.extend(vec![0x55; 75 * 2352]);
    write("ディスク.bin", &mixed);
    let (shift_jis, _, _) = encoding_rs::SHIFT_JIS.encode("ディスク.bin");
    let mut sheet = b"\xef\xbb\xbfREM test\r\nFILE \"".to_vec();
    sheet.extend_from_slice(&shift_jis);
    let tracks = format!(
        "\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:02:00\r\n  TRACK 02 AUDIO\r\n    INDEX 00 00:{:02}:{:02}\r\n    INDEX 01 00:{:02}:{:02}\r\n",
        (150 + FIXTURE.len() / 2048) / 75,
        (150 + FIXTURE.len() / 2048) % 75,
        (152 + FIXTURE.len() / 2048) / 75,
        (152 + FIXTURE.len() / 2048) % 75,
    );
    sheet.extend_from_slice(tracks.as_bytes());
    let cue = write("mixed.cue", &sheet);
    assert_eq!(logical(&cue), FIXTURE);
    // A renamed data file resolves through the sheet's own stem.
    write("cooked.iso", FIXTURE);
    let cooked = write(
        "cooked.cue",
        b"FILE \"old name.iso\" BINARY\nTRACK 1 MODE1/2048\nINDEX 1 0:0:0\n",
    );
    assert_eq!(logical(&cooked), FIXTURE);
    for (name, wide, dao) in [
        ("daox.nrg", true, true),
        ("daoi.nrg", false, true),
        ("etn2.nrg", true, false),
        ("etnf.nrg", false, false),
    ] {
        assert_eq!(logical(&write(name, &nero(wide, dao))), FIXTURE, "{name}");
    }
    let cfg = crate::OsConfig::new(directory.join("game"), "game.exe", directory.join("data"));
    let mut vfs = Vfs::new(&cfg);
    vfs.mount_disc('E', &cue).unwrap();
    vfs.mount_disc('F', directory.join("daox.nrg")).unwrap();
    for letter in ['E', 'F'] {
        let info = vfs.optical_drive(letter).unwrap();
        assert_eq!(info.label, "OPTICAL_TEST");
        assert_eq!(info.total_bytes, FIXTURE.len() as u64);
        let mut split = vfs
            .open_optical_file(&format!("{letter}:/Sub/split.bin")) // privacy: allow authored guest optical path
            .unwrap();
        let mut bytes = [0; 16];
        assert_eq!(split.read(&mut bytes).unwrap(), 11);
        assert_eq!(&bytes[..11], b"abcde world");
    }
    // A disc whose first track is audio is not a data volume.
    let audio = write(
        "audio.cue",
        b"FILE \"mode1.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n",
    );
    assert_eq!(
        vfs.mount_disc('G', audio).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let mut silent = raw(1, 2352);
    silent[..16].fill(0x55);
    let silent = write("silent.bin", &silent);
    let silent_cue = write(
        "silent.cue",
        b"FILE \"silent.bin\" BINARY\nTRACK 01 MODE1/2352\nINDEX 01 00:00:00\n",
    );
    assert!(vfs.mount_disc('G', silent).is_err());
    assert_eq!(
        vfs.mount_disc('G', silent_cue).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert!(vfs.optical_drive('G').is_none());
    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

/// Builds a binary Alcohol 120% v1.3 `.mds` descriptor with one session.
fn build_mds(
    medium_type: u16,
    tracks: &[(u8, u8, u16, u32, u64)], // (point, mode, sector_size, pregap, start_offset)
    filenames: &[(&str, bool)],         // (name, wide_utf16le)
) -> Vec<u8> {
    let mut out = vec![0u8; 88];
    out[..16].copy_from_slice(b"MEDIA DESCRIPTOR");
    out[16] = 1;
    out[17] = 3;
    out[0x12..0x14].copy_from_slice(&medium_type.to_le_bytes());
    out[0x14..0x16].copy_from_slice(&1u16.to_le_bytes());

    let sessions_offset = out.len() as u32;
    out[0x50..0x54].copy_from_slice(&sessions_offset.to_le_bytes());
    let mut session = vec![0u8; 24];
    session[0x0A] = tracks.len() as u8;
    let tracks_offset = sessions_offset + 24;
    session[0x14..0x18].copy_from_slice(&tracks_offset.to_le_bytes());
    out.extend_from_slice(&session);

    let extra_base = out.len() + tracks.len() * 80;
    let footer_base = extra_base + tracks.len() * 8;
    let names_base = footer_base + filenames.len() * 16;

    let mut name_offsets = Vec::new();
    let mut name_blob = Vec::new();
    for &(name, wide) in filenames {
        name_offsets.push((names_base + name_blob.len()) as u32);
        if wide {
            for unit in name.encode_utf16().chain([0]) {
                name_blob.extend_from_slice(&unit.to_le_bytes());
            }
        } else {
            name_blob.extend_from_slice(name.as_bytes());
            name_blob.push(0);
        }
    }

    for (idx, &(point, mode, sector_size, _pregap, start_offset)) in tracks.iter().enumerate() {
        let mut block = vec![0u8; 80];
        block[0x00] = mode;
        block[0x04] = point;
        if medium_type < 0x10 {
            let extra_off = (extra_base + idx * 8) as u32;
            block[0x0C..0x10].copy_from_slice(&extra_off.to_le_bytes());
        }
        block[0x10..0x12].copy_from_slice(&sector_size.to_le_bytes());
        block[0x28..0x30].copy_from_slice(&start_offset.to_le_bytes());
        block[0x30..0x34].copy_from_slice(&(filenames.len() as u32).to_le_bytes());
        block[0x34..0x38].copy_from_slice(&(footer_base as u32).to_le_bytes());
        out.extend_from_slice(&block);
    }
    for &(_, _, _, pregap, _) in tracks {
        out.extend_from_slice(&pregap.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
    }
    for (idx, &(_, wide)) in filenames.iter().enumerate() {
        let mut footer = vec![0u8; 16];
        footer[0..4].copy_from_slice(&name_offsets[idx].to_le_bytes());
        footer[4..8].copy_from_slice(&(u32::from(wide)).to_le_bytes());
        out.extend_from_slice(&footer);
    }
    out.extend_from_slice(&name_blob);
    out
}

#[test]
// spec: DEV-DISC-4, DEV-DISC-5
fn mdf_and_mds_variants_mount_across_sector_layouts_pregaps_and_splits() {
    let (_, directory) = fixture("mdf-variants");
    let write = |name: &str, bytes: &[u8]| {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    };

    // 2368-byte PQ subchannel raw MDF without a descriptor.
    assert_eq!(logical(&write("pq.mdf", &raw(1, 2368))), FIXTURE);

    // 2336-byte Mode 2 Form 1 without sync/header (8-byte subheader + 2048 user data).
    let mut mode2_2336 = Vec::new();
    for data in FIXTURE.chunks(2048) {
        let mut sector = vec![0u8; 2336];
        sector[0..8].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
        sector[8..8 + 2048].copy_from_slice(data);
        mode2_2336.extend_from_slice(&sector);
    }
    assert_eq!(logical(&write("m2_2336.mdf", &mode2_2336)), FIXTURE);

    // Raw MDF with zeroed bad sectors at LBA 0 and LBA 1 (common on copy-protected discs).
    let mut bad_sys = raw(1, 2352);
    bad_sys[..2 * 2352].fill(0);
    let expected_bad = {
        let mut e = FIXTURE.to_vec();
        e[..2 * 2048].fill(0);
        e
    };
    assert_eq!(logical(&write("bad_sys.mdf", &bad_sys)), expected_bad);

    // Raw MDF where LBA 0 is Mode 0 (0x00) and LBA 1 is zeroed, while LBA 16 is Mode 1.
    let mut mode0_lba0 = raw(1, 2352);
    mode0_lba0[15] = 0;
    mode0_lba0[2352..2 * 2352].fill(0);
    let expected_mode0 = {
        let mut e = FIXTURE.to_vec();
        e[2048..2 * 2048].fill(0);
        e
    };
    assert_eq!(logical(&write("mode0.mdf", &mode0_lba0)), expected_mode0);

    // Lone raw MDF with a 150-sector zeroed pregap.
    let mut pregapped = vec![0u8; 150 * 2352];
    pregapped.extend_from_slice(&raw(1, 2352));
    assert_eq!(logical(&write("pregap150.mdf", &pregapped)), FIXTURE);

    // Binary MDS with lead-in point 0xA0, non-zero start_offset, 2368-byte track 1,
    // audio track 2 boundary, and a UTF-16LE non-matching filename.
    let lead = vec![0xAAu8; 4096];
    let track1 = raw(1, 2368);
    let track1_start = lead.len() as u64;
    let track2_start = track1_start + track1.len() as u64;
    let audio_tail = vec![0x55u8; 10 * 2368];
    write("custom_data.mdf", &[lead, track1, audio_tail].concat());
    let mds_bytes = build_mds(
        0x00,
        &[
            (0xA0, 0x00, 0, 0, 0),
            (0x01, 0xAA, 2368, 0, track1_start),
            (0x02, 0xA9, 2352, 0, track2_start),
        ],
        &[("custom_data.mdf", true)],
    );
    assert_eq!(logical(&write("game.mds", &mds_bytes)), FIXTURE);

    // Opening a `.mdf` directly picks up its sibling `.mds` layout (non-zero offset).
    let sibling_lead = vec![0xBBu8; 2048];
    let sibling_start = sibling_lead.len() as u64;
    write("paired.mdf", &[sibling_lead, raw(2, 2352)].concat());
    write(
        "paired.mds",
        &build_mds(
            0x00,
            &[(0x01, 0xAC, 2352, 0, sibling_start)],
            &[("*.mdf", false)],
        ),
    );
    assert_eq!(logical(&directory.join("paired.mdf")), FIXTURE);

    // Multi-part split DVD MDF (`.mdf` + `.md0` + `.md1`) works via both `.mds` and `.mdf`.
    let part1 = FIXTURE.len() / 3;
    let part2 = (FIXTURE.len() * 2) / 3;
    let split_mdf = write("split.mdf", &FIXTURE[..part1]);
    write("split.md0", &FIXTURE[part1..part2]);
    write("split.md1", &FIXTURE[part2..]);
    let split_mds = write(
        "split.mds",
        &build_mds(0x10, &[(0x01, 0x02, 2048, 0, 0)], &[("*.mdf", false)]),
    );
    assert_eq!(logical(&split_mds), FIXTURE);
    assert_eq!(logical(&split_mdf), FIXTURE);

    // MDS whose first track is audio is rejected with Unsupported.
    write("audio_first.mdf", &raw(1, 2352));
    let audio_mds = write(
        "audio_first.mds",
        &build_mds(0x00, &[(0x01, 0xA9, 2352, 0, 0)], &[("*.mdf", false)]),
    );
    let cfg = crate::OsConfig::new(directory.join("game"), "game.exe", directory.join("data"));
    let mut vfs = Vfs::new(&cfg);
    assert_eq!(
        vfs.mount_disc('E', &audio_mds).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    assert_eq!(
        vfs.mount_disc('E', directory.join("audio_first.mdf"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );

    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
// spec: DEV-DISC-4, DEV-DISC-6
fn optical_reader_handles_evd_nonzero_terminator_truncated_tail_and_shift_jis_primary() {
    let (_, directory) = fixture("iso-compat");
    let cfg = crate::OsConfig::new(directory.join("game"), "game.exe", directory.join("data"));
    let mut vfs = Vfs::new(&cfg);

    // 1. Non-zero Volume Descriptor Set Terminator body (spaces in bytes 7..2048).
    let mut nonzero_term = FIXTURE.to_vec();
    // Sector 18 is the Volume Set Terminator in optical.iso.
    assert_eq!(nonzero_term[18 * 2048], 255);
    nonzero_term[18 * 2048 + 7..19 * 2048].fill(b' ');
    let path = directory.join("nonzero_term.iso");
    std::fs::write(&path, &nonzero_term).unwrap();
    vfs.mount_disc('D', &path).unwrap();
    assert!(vfs.open_optical_file("D:/ReadMe.txt").is_ok()); // privacy: allow authored guest optical path

    // 2. ISO 9660:1999 Enhanced Volume Descriptor (type 2, version 2) before terminator.
    let mut with_evd = FIXTURE.to_vec();
    let pvd = with_evd[16 * 2048..17 * 2048].to_vec();
    let mut evd = pvd;
    evd[0] = 2; // Supplementary / Enhanced
    evd[6] = 2; // Version 2 (ISO 9660:1999)
    evd[881] = 2; // file_structure_version = 2
    // Replace Joliet (sector 17) with EVD while keeping terminator at sector 18.
    with_evd[17 * 2048..18 * 2048].copy_from_slice(&evd);
    let evd_path = directory.join("evd.iso");
    std::fs::write(&evd_path, &with_evd).unwrap();
    vfs.mount_disc('E', &evd_path).unwrap();
    assert!(vfs.open_optical_file("E:/readme.txt").is_ok()); // privacy: allow authored guest optical path

    // 3. TAO-style truncated tail: PVD claims 2 more sectors than the file contains.
    let mut truncated = FIXTURE.to_vec();
    let declared_sectors = (FIXTURE.len() as u32 / 2048) + 2;
    truncated[16 * 2048 + 80..16 * 2048 + 84].copy_from_slice(&declared_sectors.to_le_bytes());
    truncated[16 * 2048 + 84..16 * 2048 + 88].copy_from_slice(&declared_sectors.to_be_bytes());
    let trunc_path = directory.join("tao_short.iso");
    std::fs::write(&trunc_path, &truncated).unwrap();
    vfs.mount_disc('F', &trunc_path).unwrap();
    assert_eq!(
        vfs.optical_drive('F').unwrap().total_bytes,
        FIXTURE.len() as u64
    );
    assert!(vfs.open_optical_file("F:/ReadMe.txt").is_ok()); // privacy: allow authored guest optical path

    // 4. Primary-only ISO with Shift_JIS volume label and filename (code page 932).
    let mut sjis = FIXTURE.to_vec();
    // Terminate after PVD (remove Joliet at sector 17).
    sjis[17 * 2048..18 * 2048].fill(0);
    sjis[17 * 2048..17 * 2048 + 7].copy_from_slice(&[255, b'C', b'D', b'0', b'0', b'1', 1]);
    let (label_bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語盤");
    sjis[16 * 2048 + 40..16 * 2048 + 72].fill(b' ');
    sjis[16 * 2048 + 40..16 * 2048 + 40 + label_bytes.len()].copy_from_slice(&label_bytes);
    // Primary root directory is at PVD root extent; replace "README.TXT;1" (12 bytes)
    // with a 12-byte Shift_JIS name ("読込.TXT;1" = 4 + 6 = 10 -> "読込案.TXT;1" = 6 + 6 = 12).
    let (sjis_name, _, _) = encoding_rs::SHIFT_JIS.encode("読込案.TXT;1");
    assert_eq!(sjis_name.len(), 12);
    let root_lba =
        u32::from_le_bytes(sjis[16 * 2048 + 158..16 * 2048 + 162].try_into().unwrap()) as usize;
    let root_slice = &mut sjis[root_lba * 2048..(root_lba + 1) * 2048];
    let pos = root_slice
        .windows(12)
        .position(|w| w == b"README.TXT;1")
        .expect("README.TXT;1 in primary root");
    root_slice[pos..pos + 12].copy_from_slice(&sjis_name);
    let sjis_path = directory.join("sjis.iso");
    std::fs::write(&sjis_path, &sjis).unwrap();
    vfs.mount_disc('G', &sjis_path).unwrap();
    assert_eq!(vfs.optical_drive('G').unwrap().label, "日本語盤");
    assert!(vfs.open_optical_file("G:/読込案.TXT").is_ok()); // privacy: allow authored guest optical path

    drop(vfs);
    std::fs::remove_dir_all(directory).unwrap();
}
