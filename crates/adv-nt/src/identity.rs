//! Code identity of the pinned Wine modules.
//!
//! `tools/wine/identity.py` writes a table for every module of the Wine
//! distribution to `share/hitode/identity/<module>.tsv`: a hash of the
//! image's headers, then each function's RVA, size, a hash of its bytes
//! with relocations masked, and its name. The RVA and name of each data
//! symbol with a unique name follow, so native code finds Wine's data by
//! name instead of pinning addresses. A mapped image whose
//! headers hash to the table's is that build, so its addresses name
//! functions and its functions and data can be found by name. Tables load on
//! first use and are shared by every process.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use adv_cpu::backend::Guest;
use adv_machine::Ext;

#[derive(Debug)]
pub struct Function {
    pub rva: u32,
    pub size: u32,
    /// The low half of the FNV-1a 64 hash of the bytes, with every dword a
    /// relocation patches zeroed.
    pub hash: u32,
    pub name: Box<str>,
}

#[derive(Debug)]
pub struct Table {
    pub header: u64,
    /// By RVA.
    pub functions: Vec<Function>,
    /// Data symbol RVAs, by name.
    pub variables: HashMap<Box<str>, u32>,
}

impl Table {
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let header = u64::from_str_radix(lines.next()?, 16).ok()?;
        let (mut functions, mut variables) = (Vec::new(), HashMap::new());
        for line in lines {
            let fields: Vec<_> = line.splitn(4, '\t').collect();
            let hex = |field: &str| u32::from_str_radix(field, 16).ok();
            match fields[..] {
                [rva, size, hash, name] => functions.push(Function {
                    rva: hex(rva)?,
                    size: hex(size)?,
                    hash: hex(hash)?,
                    name: name.into(),
                }),
                [rva, name] => {
                    variables.insert(name.into(), hex(rva)?);
                }
                _ => return None,
            }
        }
        Some(Self {
            header,
            functions,
            variables,
        })
    }

    /// The function containing `rva`.
    pub fn at(&self, rva: u32) -> Option<&Function> {
        let index = self.functions.partition_point(|f| f.rva <= rva);
        let function = &self.functions[index.checked_sub(1)?];
        (rva - function.rva < function.size).then_some(function)
    }

    pub fn named(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| &*f.name == name)
    }

    /// The RVA of the data symbol `name`.
    pub fn variable(&self, name: &str) -> Option<u32> {
        self.variables.get(name).copied()
    }
}

/// The table of each table file, once read, or `None` if it is unreadable.
static TABLES: LazyLock<Mutex<HashMap<PathBuf, Option<Arc<Table>>>>> =
    LazyLock::new(Default::default);

fn load(path: &Path) -> Option<Arc<Table>> {
    let mut tables = TABLES.lock().unwrap_or_else(|p| p.into_inner());
    tables
        .entry(path.to_owned())
        .or_insert_with(|| {
            let text = std::fs::read_to_string(path).ok()?;
            Table::parse(&text).map(Arc::new)
        })
        .clone()
}

/// A mapped image and its optional distribution identity.
#[derive(Clone, Debug)]
pub struct Module {
    pub base: u32,
    pub size: u32,
    /// The image file's name, lowercase.
    pub name: String,
    header: u64,
    pub path: Option<PathBuf>,
    table: Option<PathBuf>,
}

impl Module {
    /// Whether its source path belongs to a distribution module.
    pub fn is_distribution(&self) -> bool {
        self.table.is_some()
    }

    /// The image's table, if it describes this build.
    pub fn table(&self) -> Option<Arc<Table>> {
        load(self.table.as_ref()?).filter(|table| table.header == self.header)
    }
}

/// The process's mapped images, by base.
#[derive(Default)]
pub struct Images(pub BTreeMap<u32, Module>);

impl Images {
    pub fn containing(&self, address: u32) -> Option<&Module> {
        let (_, module) = self.0.range(..=address).next_back()?;
        (address - module.base < module.size).then_some(module)
    }
}

/// `<install>/share/hitode/identity/<name>.tsv` for
/// `<install>/lib/wine/i386-windows/<name>` or its Unix counterpart.
fn table_of(dll: &Path) -> Option<(String, PathBuf)> {
    let directory = dll.parent()?;
    if !matches!(
        directory.file_name()?.to_str()?,
        "i386-windows" | "i386-unix"
    ) {
        return None;
    }
    let install = directory.parent()?.parent()?.parent()?;
    let name = dll.file_name()?.to_str()?.to_ascii_lowercase();
    let table = install.join(format!("share/hitode/identity/{name}.tsv"));
    Some((name, table))
}

/// Records the mapped source and, for distribution modules, its code identity.
pub(crate) fn note_image(ext: &mut Ext, base: u32, size: u32, dll: Option<&Path>, headers: &[u8]) {
    let images = ext.get_or_default::<Images>();
    let distribution = dll.and_then(table_of);
    let header = if distribution.is_some() {
        fnv1a64(headers)
    } else {
        0
    };
    let (name, table) = distribution.map_or_else(
        || (String::new(), None),
        |(name, table)| (name, Some(table)),
    );
    images.0.insert(
        base,
        Module {
            base,
            size,
            name,
            header,
            table,
            path: dll.map(Path::to_owned),
        },
    );
}

/// Records ntdll.dll, which a process maps from `image`, read from `dll`,
/// at its preferred base when it starts, and binds its native bodies.
pub fn note_ntdll<B: adv_cpu::backend::Backend>(
    machine: &mut adv_machine::Machine<B>,
    dll: &Path,
    image: &[u8],
) {
    if let Ok(pe) = adv_pe::Pe::parse(image) {
        let headers = &image[..(pe.size_of_headers as usize).min(image.len())];
        let base = pe.image_base;
        note_image(&mut machine.ext, base, pe.size_of_image, Some(dll), headers);
        let (cpu, sys, ext) = (&mut machine.cpu, &mut machine.sys, &mut machine.ext);
        crate::native::bind(cpu, sys, ext, base, &pe);
    }
}

/// Records and binds an i386 Unix image after relocation, using sorted RVAs.
pub fn note_unix(
    env: &mut adv_machine::Env<'_>,
    path: &Path,
    base: u32,
    image: &[u8],
    relocations: &[u32],
) {
    let phoff = u32::from_le_bytes(image[28..32].try_into().unwrap()) as usize;
    let phsize = u16::from_le_bytes(image[42..44].try_into().unwrap()) as usize;
    let phnum = u16::from_le_bytes(image[44..46].try_into().unwrap()) as usize;
    note_image(
        env.ext,
        base,
        image.len() as u32,
        Some(path),
        &image[..52.max(phoff + phsize * phnum)],
    );
    crate::native::bind_relocated(env.cpu, env.sys, env.ext, base, relocations);
}

pub(crate) fn forget_image(ext: &mut Ext, base: u32) {
    ext.get_or_default::<Images>().0.remove(&base);
}

/// `module!function+offset` for an address in a known function.
pub fn place(ext: &Ext, address: u32) -> Option<String> {
    let module = ext.try_get::<Images>()?.containing(address)?;
    let rva = address - module.base;
    let table = module.table()?;
    let function = table.at(rva)?;
    Some(format!(
        "{}!{}+{:#x}",
        module.name,
        function.name,
        rva - function.rva
    ))
}

/// Whether the function at `address` still holds the bytes the table
/// describes, with the dwords at `relocated` addresses zeroed.
pub fn intact(
    cpu: &dyn Guest,
    address: u32,
    function: &Function,
    relocated: impl Fn(u32) -> bool,
) -> bool {
    let mut code = vec![0; function.size as usize];
    if cpu.read(address, &mut code).is_err() {
        return false;
    }
    for at in address.wrapping_sub(3)..address.wrapping_add(function.size) {
        if relocated(at) {
            for byte in at.max(address)..(at + 4).min(address + function.size) {
                code[(byte - address) as usize] = 0;
            }
        }
    }
    fnv1a64(&code) as u32 == function.hash
}

/// The functions every guest file read passes through, by module.
const READ_PATH: &[(&str, &str)] = &[("kernelbase.dll", "ReadFile"), ("ntdll.dll", "NtReadFile")];
/// The bytes an inline hook rewrites: a jump at the entry, or a hot patch's
/// jump in the five bytes before it.
const HOOK_SPAN: u32 = 10;

/// The entry bytes of the read path as the loader mapped them, before any
/// guest code ran.
#[derive(Default)]
struct ReadPath(Vec<(u32, [u8; HOOK_SPAN as usize])>);

/// Records the read path's entries in `module`, a distribution image just mapped.
pub(crate) fn note_read_path(cpu: &dyn Guest, ext: &mut Ext, module: &Module) {
    let Some(table) = module.table() else {
        return;
    };
    let path = ext.get_or_default::<ReadPath>();
    for &(_, name) in READ_PATH.iter().filter(|(owner, _)| *owner == module.name) {
        let Some(function) = table.named(name) else {
            continue;
        };
        let at = module.base + function.rva - 5;
        let mut bytes = [0; HOOK_SPAN as usize];
        if cpu.read(at, &mut bytes).is_ok() {
            path.0.push((at, bytes));
        }
    }
}

/// Whether guest code hooked the file read path, so that file contents may
/// come from the guest (a file virtualizer) rather than the host descriptor.
pub fn reads_hooked(cpu: &dyn Guest, ext: &Ext) -> bool {
    ext.try_get::<ReadPath>().is_some_and(|path| {
        path.0.iter().any(|(at, mapped)| {
            let mut bytes = [0; HOOK_SPAN as usize];
            cpu.read(*at, &mut bytes).is_err() || bytes != *mapped
        })
    })
}

pub fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: NAT-ID-1
    #[test]
    fn tables_name_addresses_inside_functions() {
        let table =
            Table::parse("00000000000000ff\n1000\t20\t1\tfirst\n1040\t8\t2\tsecond\n2000\tdatum\n")
                .unwrap();
        assert_eq!(table.variable("datum"), Some(0x2000));
        assert_eq!(table.variable("first"), None);
        assert!(table.at(0x2000).is_none());
        assert_eq!(table.header, 0xff);
        assert_eq!(table.at(0x101f).map(|f| &*f.name), Some("first"));
        assert!(table.at(0x1020).is_none());
        assert!(table.at(0xfff).is_none());
        assert_eq!(table.at(0x1047).map(|f| f.hash), Some(2));
        assert_eq!(table.named("second").map(|f| f.rva), Some(0x1040));
    }

    // spec: NAT-ID-1
    #[test]
    fn functions_and_tables_match_only_their_build() {
        use adv_cpu::backend::{Guest, Prot};
        const AT: u32 = 0x40_1000;
        // push imm32 with a relocated operand; ret.
        let code = [0x68, 0x00, 0x20, 0x40, 0x00, 0xc3];
        let mut masked = code;
        masked[1..5].fill(0);
        let function = Function {
            rva: 0x1000,
            size: code.len() as u32,
            hash: fnv1a64(&masked) as u32,
            name: "f".into(),
        };
        let mut cpu = adv_cpu::Cpu::new(0x1000, false).unwrap();
        cpu.map(AT, 0x1000, Prot::RX).unwrap();
        let relocated = |address| address == AT + 1;
        cpu.write(AT, &[0x68, 0x00, 0x20, 0x80, 0x10, 0xc3])
            .unwrap();
        assert!(intact(&cpu, AT, &function, relocated));
        cpu.write(AT + 5, &[0xcc]).unwrap();
        assert!(!intact(&cpu, AT, &function, relocated));

        let directory = std::env::temp_dir().join(format!("identity-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let table = directory.join("x.dll.tsv");
        std::fs::write(&table, format!("{:016x}\n1000\t6\t0\tf\n", fnv1a64(b"MZ"))).unwrap();
        let module = |headers: &[u8]| Module {
            base: 0x40_0000,
            size: 0x2000,
            name: "x.dll".into(),
            header: fnv1a64(headers),
            path: None,
            table: Some(table.clone()),
        };
        assert!(module(b"MZ").table().is_some());
        assert!(module(b"MZ\0").table().is_none());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    // spec: MEDIA-GRAPH-8
    #[test]
    fn hooks_on_the_read_path_are_seen() {
        use adv_cpu::backend::{Guest, Prot};
        const BASE: u32 = 0x7bc0_0000;
        let directory = std::env::temp_dir().join(format!("read-path-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let table = directory.join("ntdll.dll.tsv");
        std::fs::write(
            &table,
            format!("{:016x}\n1010\t10\t0\tNtReadFile\n", fnv1a64(b"MZ")),
        )
        .unwrap();
        let module = Module {
            base: BASE,
            size: 0x2000,
            name: "ntdll.dll".into(),
            header: fnv1a64(b"MZ"),
            path: None,
            table: Some(table),
        };
        let mut cpu = adv_cpu::Cpu::new(0x1000, false).unwrap();
        cpu.map(BASE, 0x2000, Prot::RWX).unwrap();
        // mov eax, 6; mov edx, imm32; call edx
        cpu.write(
            BASE + 0x1010,
            &[0xb8, 6, 0, 0, 0, 0xba, 0xc4, 0xe0, 0xc0, 0x7b, 0xff, 0xd2],
        )
        .unwrap();
        let mut ext = Ext::default();
        assert!(!reads_hooked(&cpu, &ext));
        note_read_path(&cpu, &mut ext, &module);
        assert!(!reads_hooked(&cpu, &ext));
        cpu.write(BASE + 0x1010, &[0xe9, 0x23, 0x06, 0x9b, 0x84])
            .unwrap();
        assert!(reads_hooked(&cpu, &ext));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    // spec: NAT-ID-1
    #[test]
    fn distribution_paths_have_tables() {
        let (name, table) = table_of(Path::new("/w/lib/wine/i386-windows/NTDLL.dll")).unwrap();
        assert_eq!(name, "ntdll.dll");
        assert_eq!(table, Path::new("/w/share/hitode/identity/ntdll.dll.tsv"));
        assert!(table_of(Path::new("/game/ntdll.dll")).is_none());
    }
}
