//! Opt-in local code capture for offline GIR experiments. The directory must
//! be explicitly supplied; no sample data is printed or embedded in reports.
use crate::Region;
use iced_x86::Instruction;
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    io::Write,
};

/// Writes compiler-owned artifacts only when the independent local diagnostic
/// sink is selected. The regular sample capture never exports native code.
pub(crate) fn native(context: &cranelift_codegen::Context, arch: &str) {
    let Some(directory) = std::env::var_os("ADV_GIR_NATIVE") else {
        return;
    };
    let Some(code) = context.compiled_code() else {
        return;
    };
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let key = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("baseline-{arch}-{key}.bin")),
        code.code_buffer(),
    )
    .unwrap();
    std::fs::write(
        directory.join(format!("baseline-{arch}-{key}.blocks")),
        code.bb_starts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    std::fs::write(
        directory.join(format!("baseline-{arch}-{key}.clif")),
        context.func.display().to_string(),
    )
    .unwrap();
    if let Some(vcode) = &code.vcode {
        std::fs::write(
            directory.join(format!("baseline-{arch}-{key}.vcode")),
            vcode,
        )
        .unwrap();
    }
}

pub(crate) fn record(
    region: &Region,
    origin: u32,
    bytes: &[u8],
    instructions: &BTreeMap<u32, Instruction>,
) {
    let Some(directory) = std::env::var_os("ADV_GIR_CAPTURE") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    let key = format!("{:08x}-{:016x}", region.start, hash.finish());
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&directory)?;
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(format!("{key}.bin")))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error),
        };
        file.write_all(bytes)?;
        let text = format!(
            "{}\t{}\t{}\t{}\n{}\n",
            region.start,
            origin,
            region.len,
            instructions.len(),
            instructions
                .keys()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("\t")
        );
        std::fs::write(directory.join(format!("{key}.tsv")), text)
    };
    if write().is_err() {
        eprintln!("warning: GIR local capture failed");
    }
}
