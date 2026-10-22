use super::*;
use iced_x86::code_asm::*;

const BASE: u32 = 0x40_0000;
const LOAD: u32 = BASE + 0x3000;
const LOAD_WIDE: u32 = BASE + 0x3004;
const PROC: u32 = BASE + 0x3008;
const CLASS: u32 = BASE + 0x300c;
const QUERY: u32 = BASE + 0x3010;
const SEEK: u32 = BASE + 0x3014;
const CLSID: [u8; 16] = [
    0x11, 0xf8, 0x90, 0x45, 0x3a, 0x1d, 0xd0, 0x11, 0x89, 0x1f, 0x00, 0xaa, 0x00, 0x4b, 0x2e, 0x24,
];

fn at(offset: u32) -> i32 {
    (BASE + offset) as i32
}

// spec: PERS-EXPORT-9, PERS-EXPORT-10
#[test]
fn constant_names_reach_lookups_through_common_call_shapes() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut thunk = a.create_label();
    a.push(at(0x2000)).unwrap();
    a.call(dword_ptr(LOAD as i32)).unwrap();
    a.push(at(0x2040)).unwrap();
    a.call(dword_ptr(LOAD_WIDE as i32)).unwrap();
    // A module query records no requirement, NULL included.
    a.push(at(0x2030)).unwrap();
    a.call(dword_ptr(QUERY as i32)).unwrap();
    a.push(0).unwrap();
    a.call(dword_ptr(QUERY as i32)).unwrap();
    // A cached import pointer survives calls; EAX does not.
    a.mov(esi, dword_ptr(PROC as i32)).unwrap();
    a.push(at(0x2010)).unwrap();
    a.push(eax).unwrap();
    a.call(esi).unwrap();
    a.push(7).unwrap();
    a.push(eax).unwrap();
    a.call(esi).unwrap();
    a.push(ebx).unwrap();
    a.push(eax).unwrap();
    a.call(esi).unwrap();
    // Arguments stored rather than pushed.
    a.mov(dword_ptr(esp + 4), at(0x2020)).unwrap();
    a.mov(dword_ptr(esp), eax).unwrap();
    a.call(dword_ptr(PROC as i32)).unwrap();
    // SetFilePointer(file, 0, NULL, FILE_END) with its result overwritten.
    a.push(2).unwrap();
    a.push(0).unwrap();
    a.push(0).unwrap();
    a.push(ebx).unwrap();
    a.call(dword_ptr(SEEK as i32)).unwrap();
    a.xor(eax, eax).unwrap();
    // CoCreateInstance(clsid, outer, context, iid, out) through an import thunk.
    a.push(0).unwrap();
    a.push(0).unwrap();
    a.push(1).unwrap();
    a.push(0).unwrap();
    a.mov(ecx, at(0x2100)).unwrap();
    a.push(ecx).unwrap();
    a.call(thunk).unwrap();
    a.ret().unwrap();
    a.set_label(&mut thunk).unwrap();
    a.jmp(dword_ptr(CLASS as i32)).unwrap();
    let code = a.assemble(u64::from(BASE + 0x1000)).unwrap();

    let mut bytes = vec![0; 0x4000];
    bytes[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    for (offset, text) in [
        (0x2000, "Plugin"),
        (0x2010, "Bar"),
        (0x2020, "Baz"),
        (0x2030, "mscoree.dll"),
    ] {
        bytes[offset..offset + text.len()].copy_from_slice(text.as_bytes());
    }
    for (i, unit) in "Wide.DLL".encode_utf16().enumerate() {
        bytes[0x2040 + 2 * i..0x2042 + 2 * i].copy_from_slice(&unit.to_le_bytes());
    }
    bytes[0x2100..0x2110].copy_from_slice(&CLSID);
    let imports = [
        (LOAD, "kernel32.dll", "LoadLibraryA"),
        (LOAD_WIDE, "kernel32.dll", "LoadLibraryW"),
        (PROC, "kernel32.dll", "GetProcAddress"),
        (CLASS, "ole32.dll", "CoCreateInstance"),
        (QUERY, "kernel32.dll", "GetModuleHandleA"),
        (SEEK, "kernel32.dll", "SetFilePointer"),
    ];
    let imports = Imports {
        slots: imports
            .iter()
            .enumerate()
            .map(|(i, &(slot, ..))| (slot, i))
            .collect(),
        targets: imports
            .iter()
            .map(|&(_, dll, name)| Target::new(dll, name))
            .collect(),
    };
    let image = Image {
        base: BASE,
        bytes: &bytes,
    };
    let mut manifest = Manifest::default();
    scan(
        &bytes[0x1000..0x1000 + code.len()],
        BASE + 0x1000,
        &imports,
        &image,
        &mut manifest,
    );
    assert_eq!(
        manifest.loads,
        BTreeSet::from(["plugin.dll".into(), "wide.dll".into()])
    );
    assert_eq!(
        manifest.lookups,
        BTreeSet::from(["#7".into(), "Bar".into(), "Baz".into()])
    );
    assert_eq!(manifest.classes, BTreeSet::from([CLSID]));
    assert_eq!((manifest.sites, manifest.open_sites), (9, 1));
    let used = |dll: &str, name: &str, param, value, ignored| {
        let key = (dll.to_owned(), name.to_owned(), param, Some(value));
        (key, Calls { sites: 1, ignored })
    };
    assert_eq!(
        manifest.uses,
        BTreeMap::from([
            used("kernel32.dll", "SetFilePointer", "dwMoveMethod", 2, 1),
            used("kernel32.dll", "SetFilePointer", "lDistanceToMove", 0, 1),
            // Returned to the caller, so not discarded.
            used("ole32.dll", "CoCreateInstance", "dwClsContext", 1, 0),
        ])
    );
    assert_eq!(guid(&CLSID), "{4590f811-1d3a-11d0-891f-00aa004b2e24}");
}

struct Fake;

impl Provider for Fake {
    fn replaces(&self, module: &str) -> bool {
        module == "d3dx9_43.dll"
    }
    fn provides(&self, module: &str) -> bool {
        matches!(module, "kernel32.dll" | "d3dx9_43.dll")
    }
    fn binds(&self, module: &str, name: &str) -> bool {
        (module, name) != ("kernel32.dll", "Missing")
    }
    fn lookup(&self, name: &str) -> Vec<(String, bool)> {
        match name {
            "Known" => vec![("kernel32.dll".into(), true)],
            "Hidden" => vec![
                ("kernel32.dll".into(), false),
                ("kernelbase.dll".into(), false),
            ],
            _ => Vec::new(),
        }
    }
    fn class(&self, clsid: &[u8; 16]) -> bool {
        clsid[0] == 1
    }
    fn accepts(&self, module: &str, name: &str, param: &str) -> Option<Accepts> {
        ((module, name, param) == ("kernel32.dll", "Sleep", "dwMilliseconds"))
            .then_some(|ms| ms < 100)
    }
}

fn set<const N: usize>(items: [&str; N]) -> BTreeSet<String> {
    items.into_iter().map(str::to_owned).collect()
}

// spec: PERS-EXPORT-9, PERS-EXPORT-10
#[test]
fn gaps_resolve_game_modules_before_host_modules_unless_replaced() {
    let sleep = |ms| {
        (
            "kernel32.dll".to_owned(),
            "Sleep".to_owned(),
            "dwMilliseconds",
            ms,
        )
    };
    let import = |dll: &str, name: &str| (dll.to_owned(), name.to_owned());
    let exe = Manifest {
        imports: BTreeSet::from([
            import("kernel32.dll", "Sleep"),
            import("kernel32.dll", "Missing"),
            import("game.dll", "Exported"),
            import("game.dll", "Absent"),
            import("gone.dll", "Anything"),
            import("d3dx9_43.dll", "D3DXCreateSprite"),
        ]),
        loads: set(["opt.dll", "game.dll", "kernel32.dll"]),
        lookups: set(["Known", "Hidden", "Nowhere", "Exported"]),
        classes: BTreeSet::from([[1; 16], [2; 16]]),
        sites: 5,
        open_sites: 1,
        uses: BTreeMap::from([
            (
                sleep(Some(10)),
                Calls {
                    sites: 2,
                    ignored: 2,
                },
            ),
            (
                sleep(Some(500)),
                Calls {
                    sites: 1,
                    ignored: 1,
                },
            ),
            (
                sleep(None),
                Calls {
                    sites: 4,
                    ignored: 0,
                },
            ),
            (
                (
                    "kernel32.dll".into(),
                    "SetErrorMode".into(),
                    "uMode",
                    Some(1),
                ),
                Calls {
                    sites: 1,
                    ignored: 0,
                },
            ),
            // Bound to a game module, whose arguments are its own business.
            (
                ("game.dll".into(), "Exported".into(), "mode", Some(9)),
                Calls {
                    sites: 1,
                    ignored: 0,
                },
            ),
        ]),
        ..Manifest::default()
    };
    let dll = Manifest {
        imports: BTreeSet::from([import("kernel32.dll", "Missing")]),
        exports: set(["Exported", "#1"]),
        ..Manifest::default()
    };
    let modules = [
        Module {
            file: "game.exe".into(),
            manifest: exe,
        },
        Module {
            file: "plugin/game.dll".into(),
            manifest: dll,
        },
        Module {
            file: "d3dx9_43.dll".into(),
            manifest: Manifest::default(),
        },
    ];
    let gaps = gaps(&modules, &Fake);
    let by = |entries: &[(&str, &[&str])]| -> BTreeMap<String, BTreeSet<String>> {
        entries
            .iter()
            .map(|(item, files)| {
                (
                    (*item).to_owned(),
                    files.iter().map(|f| (*f).to_owned()).collect(),
                )
            })
            .collect()
    };
    assert_eq!(gaps.missing_modules, by(&[("gone.dll", &["game.exe"])]));
    assert_eq!(
        gaps.missing_imports,
        by(&[
            ("game.dll!Absent", &["game.exe"]),
            ("kernel32.dll!Missing", &["game.exe", "plugin/game.dll"]),
        ])
    );
    assert_eq!(gaps.absent_loads, by(&[("opt.dll", &["game.exe"])]));
    assert_eq!(
        gaps.unavailable_lookups,
        by(&[
            ("kernel32.dll!Hidden", &["game.exe"]),
            ("kernelbase.dll!Hidden", &["game.exe"]),
        ])
    );
    assert_eq!(gaps.undeclared_lookups, by(&[("Nowhere", &["game.exe"])]));
    assert_eq!(
        gaps.missing_classes,
        by(&[(guid(&[2; 16]).as_str(), &["game.exe"])])
    );
    assert_eq!((gaps.sites, gaps.open_sites), (5, 1));
    assert_eq!(
        gaps.unsupported_modes,
        by(&[(
            "kernel32.dll!Sleep dwMilliseconds=0x1f4 (result discarded)",
            &["game.exe"]
        )])
    );
    assert_eq!(
        gaps.unreviewed_modes,
        BTreeMap::from([(
            "kernel32.dll!SetErrorMode uMode".to_owned(),
            BTreeSet::from([1])
        )])
    );
    assert_eq!((gaps.mode_sites, gaps.open_mode_sites), (7, 4));
}
