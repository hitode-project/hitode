//! Host FreeType 2.13.2 behind Wine's i386 ABI. Only guest addresses enter
//! the mirrors; faces and their memory-face input stay owned by this module.
//! Layouts follow the 2.13.2 public headers compiled for i386 (32-bit long
//! and pointers, four-byte maximum alignment). The standalone raster uses
//! i386 conic subdivision on every host; 64-bit FreeType's DDA differs in
//! coverage rounding. FreeType is copyright 2000-2023 The FreeType Project;
//! its source and license are in this crate's `freetype` directory.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::mem::size_of;
use std::ptr;

use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, Env, Outcome};
use freetype_sys as ft;

use crate::{Function, libc::heap_call};

pub const SONAME: &str = "libfreetype.so.6";
pub const HANDLE: u32 = 0xffff_ff01;
const FACE: usize = 132;
const SIZE: usize = FACE;
const SLOT: usize = SIZE + 44;
const RECORDS: usize = SLOT + 160;

macro_rules! functions {
    ($($name:ident: $argc:literal),* $(,)?) => {
        pub fn function(name: &str) -> Option<Function> {
            Some(match name {
                $(stringify!($name) => (Conv::Cdecl { argc: $argc },
                    |env| call(env, stringify!($name))),)*
                _ => return None,
            })
        }
    };
}
functions! {
    FT_Init_FreeType: 1, FT_Library_Version: 4,
    FT_Done_Face: 1, FT_New_Face: 4, FT_New_Memory_Face: 5,
    FT_Get_Char_Index: 2, FT_Get_First_Char: 2, FT_Get_Next_Char: 3,
    FT_Get_Sfnt_Name: 3, FT_Get_Sfnt_Name_Count: 1, FT_Get_Sfnt_Table: 2,
    FT_Get_TrueType_Engine_Type: 1, FT_Get_WinFNT_Header: 2,
    FT_Library_SetLcdFilter: 2, FT_Load_Glyph: 3, FT_Load_Sfnt_Table: 5,
    FT_Matrix_Multiply: 2, FT_MulDiv: 3, FT_MulFix: 2,
    FT_Outline_Embolden: 2, FT_Outline_Get_Bitmap: 3, FT_Outline_Get_CBox: 2,
    FT_Outline_Transform: 2, FT_Outline_Translate: 3, FT_Property_Set: 4,
    FT_Render_Glyph: 2, FT_Set_Charmap: 2, FT_Set_Pixel_Sizes: 3,
    FT_Vector_Length: 1, FT_Vector_Transform: 2, FT_Vector_Unit: 2,
}

// These exports are absent from freetype-sys's declarations.
unsafe extern "C" {
    fn FT_New_Memory() -> ft::FT_Memory;
    fn FT_Done_Memory(memory: ft::FT_Memory);
    fn FT_Library_Version(
        library: ft::FT_Library,
        major: *mut i32,
        minor: *mut i32,
        patch: *mut i32,
    );
    fn FT_Get_TrueType_Engine_Type(library: ft::FT_Library) -> u32;
    fn FT_Get_WinFNT_Header(face: ft::FT_Face, header: *mut u8) -> i32;
    fn FT_Vector_Transform(vector: *mut ft::FT_Vector, matrix: *const ft::FT_Matrix);
    fn FT_Matrix_Multiply(left: *const ft::FT_Matrix, right: *mut ft::FT_Matrix);
    fn FT_Vector_Unit(vector: *mut ft::FT_Vector, angle: ::libc::c_long);
}

// FT_Module_Class and FT_Renderer_Class, from FreeType's public module API.
// The first word of FT_ModuleRec is its class (ftobjs.h, pinned at 2.13.2).
#[repr(C)]
#[derive(Clone, Copy)]
struct ModuleClass {
    flags: ::libc::c_ulong,
    size: ::libc::c_long,
    name: *const ::libc::c_char,
    version: ::libc::c_long,
    requires: ::libc::c_long,
    interface: *const (),
    init: *const (),
    done: *const (),
    request: *const (),
}
#[repr(C)]
#[derive(Clone, Copy)]
struct RendererClass {
    root: ModuleClass,
    format: u32,
    render: *const (),
    transform: *const (),
    cbox: *const (),
    mode: *const (),
    raster: *const (),
}
unsafe extern "C" {
    static hitode_grays_raster: u8;
    fn FT_Get_Module(library: ft::FT_Library, name: *const ::libc::c_char) -> ft::FT_Module;
    fn FT_Remove_Module(library: ft::FT_Library, module: ft::FT_Module) -> i32;
    fn FT_Add_Module(library: ft::FT_Library, class: *const ModuleClass) -> i32;
    fn FT_Set_Renderer(
        library: ft::FT_Library,
        renderer: ft::FT_Module,
        count: u32,
        parameters: *const (),
    ) -> i32;
}
struct Library {
    host: ft::FT_Library,
    _renderer: Box<RendererClass>,
}
fn scalar_renderer(host: ft::FT_Library) -> Result<Box<RendererClass>, i32> {
    // SAFETY: the bundled library provides the smooth renderer and these
    // version-pinned public class records. FreeType retains the boxed class.
    unsafe {
        let module = FT_Get_Module(host, c"smooth".as_ptr());
        let original = *module.cast::<*const RendererClass>();
        let mut class = Box::new(*original);
        class.raster = (&raw const hitode_grays_raster).cast();
        let error = FT_Remove_Module(host, module);
        if error != 0 {
            return Err(error);
        }
        let error = FT_Add_Module(host, &class.root);
        if error != 0 {
            return Err(error);
        }
        let renderer = FT_Get_Module(host, c"smooth".as_ptr());
        FT_Set_Renderer(host, renderer, 0, ptr::null());
        Ok(class)
    }
}

#[derive(Default)]
struct State {
    libraries: HashMap<u32, Library>,
    faces: HashMap<u32, Face>,
}
impl Drop for State {
    fn drop(&mut self) {
        self.faces.clear();
        for library in self.libraries.values() {
            // SAFETY: each library was initialized here, after all its faces.
            unsafe { ft::FT_Done_FreeType(library.host) };
        }
    }
}
struct Face {
    host: ft::FT_Face,
    library: u32,
    // FreeType borrows this allocation until FT_Done_Face.
    _input: Option<Vec<u8>>,
    charmaps: Vec<(u32, ft::FT_CharMap)>,
    tables: HashMap<u32, u32>,
    names: Vec<u32>,
    data: u32,
    capacity: usize,
}
impl Drop for Face {
    fn drop(&mut self) {
        // SAFETY: this is the unique owner, including the input buffer.
        unsafe { ft::FT_Done_Face(self.host) };
    }
}

fn library(env: &mut Env<'_>, key: u32) -> ft::FT_Library {
    env.ext
        .get_or_default::<State>()
        .libraries
        .get(&key)
        .map(|library| library.host)
        .unwrap_or(ptr::null_mut())
}
fn face(env: &mut Env<'_>, key: u32) -> ft::FT_Face {
    env.ext
        .get_or_default::<State>()
        .faces
        .get(&key)
        .map_or(ptr::null_mut(), |f| f.host)
}
fn read(env: &Env<'_>, at: u32, len: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0; len];
    env.cpu.read(at, &mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}
fn word(env: &Env<'_>, at: u32) -> Result<u32, String> {
    env.cpu.read_u32(at).map_err(|e| e.to_string())
}
fn write(env: &mut Env<'_>, at: u32, bytes: &[u8]) -> Result<(), String> {
    env.cpu.write(at, bytes).map_err(|e| e.to_string())
}
fn put(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn short(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn longs(values: &[::libc::c_long]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|&v| (v as i32).to_le_bytes())
        .collect()
}
fn append(bytes: &mut Vec<u8>, data: &[u8]) -> u32 {
    let at = bytes.len().next_multiple_of(4);
    bytes.resize(at, 0);
    bytes.extend_from_slice(data);
    at as u32
}

/// Packs a public C record from its header's field sequence. `l` is an
/// FT_Long/FT_ULong, `s` a short, `b` a byte and `p` a private null pointer.
/// SAFETY: host points to the FreeType record described by fields.
unsafe fn pack(host: *const u8, fields: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut offset = 0usize;
    for &field in fields {
        let width = match field {
            b'l' => size_of::<::libc::c_long>(),
            b'p' => size_of::<usize>(),
            b's' => 2,
            _ => 1,
        };
        offset = offset.next_multiple_of(width);
        bytes.resize(bytes.len().next_multiple_of(width.min(4)), 0);
        // SAFETY: the field is within the caller's record, aligned above.
        unsafe {
            match field {
                b'l' => bytes.extend_from_slice(
                    &(*(host.add(offset).cast::<::libc::c_long>()) as u32).to_le_bytes(),
                ),
                b'p' => bytes.extend_from_slice(&0u32.to_le_bytes()),
                b's' => bytes.extend_from_slice(&(*host.add(offset).cast::<u16>()).to_le_bytes()),
                _ => bytes.push(*host.add(offset)),
            }
        }
        offset += width;
    }
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes
}

fn call(env: &mut Env<'_>, name: &str) -> Outcome {
    match name {
        "FT_Init_FreeType" => init(env),
        "FT_New_Face" | "FT_New_Memory_Face" => new_face(env, name == "FT_New_Memory_Face"),
        "FT_Done_Face" => done_face(env),
        "FT_Load_Glyph" | "FT_Set_Pixel_Sizes" | "FT_Render_Glyph" => mutate(env, name),
        _ => match scalar(env, name) {
            Ok(result) => Outcome::Return(result),
            Err(error) => Outcome::Fail(format!("{name}: {error}")),
        },
    }
}

fn init(env: &mut Env<'_>) -> Outcome {
    let out = env.arg(0);
    let mut host = ptr::null_mut();
    // Wine's libc getenv exposes no FreeType properties. Follow FT_Init_FreeType
    // without FT_Set_Default_Properties, which reads the host's environment.
    // SAFETY: FreeType's own allocator owns this library until Done_FreeType.
    unsafe {
        let memory = FT_New_Memory();
        if memory.is_null() {
            return Outcome::Return(ft::FT_Err_Unimplemented_Feature as u32);
        }
        let error = ft::FT_New_Library(memory, &mut host);
        if error != 0 {
            FT_Done_Memory(memory);
            return Outcome::Return(error as u32);
        }
        ft::FT_Add_Default_Modules(host);
    }
    let renderer = match scalar_renderer(host) {
        Ok(renderer) => renderer,
        Err(error) => {
            // SAFETY: initialization failed before any faces were created.
            unsafe { ft::FT_Done_FreeType(host) };
            return Outcome::Return(error as u32);
        }
    };
    heap_call(env, "RtlAllocateHeap", &[8, 4], move |env, key| {
        if key == 0 {
            // SAFETY: the newly created library has no faces.
            unsafe { ft::FT_Done_FreeType(host) };
            return Outcome::Return(ft::FT_Err_Out_Of_Memory as u32);
        }
        env.ext.get_or_default::<State>().libraries.insert(
            key,
            Library {
                host,
                _renderer: renderer,
            },
        );
        match env.cpu.write_u32(out, key) {
            Ok(()) => Outcome::Return(0),
            Err(e) => Outcome::Fail(format!("FT_Init_FreeType: {e}")),
        }
    })
}

fn new_face(env: &mut Env<'_>, memory: bool) -> Outcome {
    let args = [
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        if memory { env.arg(4) } else { 0 },
    ];
    if args[1] == 0 {
        return Outcome::Return(ft::FT_Err_Invalid_Argument as u32);
    }
    let out = if memory { args[4] } else { args[3] };
    let host_library = library(env, args[0]);
    let mut host = ptr::null_mut();
    let mut input = None;
    let error;
    // SAFETY: inputs are copied out of the guest, outputs are host handles.
    unsafe {
        if memory {
            if (args[2] as i32) < 0 {
                return Outcome::Return(ft::FT_Err_Invalid_Argument as u32);
            }
            let bytes = match read(env, args[1], args[2] as usize) {
                Ok(bytes) => bytes,
                Err(e) => return Outcome::Fail(e),
            };
            error = ft::FT_New_Memory_Face(
                host_library,
                bytes.as_ptr(),
                args[2] as ::libc::c_long,
                args[3] as i32 as ::libc::c_long,
                if out == 0 { ptr::null_mut() } else { &mut host },
            );
            input = Some(bytes);
        } else {
            let path = match env.cpu.read_cstr(args[1], ::libc::PATH_MAX as usize) {
                Ok(bytes) => CString::new(bytes).unwrap(),
                Err(e) => return Outcome::Fail(e.to_string()),
            };
            error = ft::FT_New_Face(
                host_library,
                path.as_ptr(),
                args[2] as i32 as ::libc::c_long,
                if out == 0 { ptr::null_mut() } else { &mut host },
            );
        }
    }
    if error != 0 {
        return Outcome::Return(error as u32);
    }
    if out == 0 {
        return Outcome::Return(0);
    }
    let mut mirror = Face {
        host,
        library: args[0],
        _input: input,
        charmaps: Vec::new(),
        tables: HashMap::new(),
        names: Vec::new(),
        data: 0,
        capacity: 0,
    };
    let (mut bytes, relocations) = metadata(&mut mirror);
    heap_call(
        env,
        "RtlAllocateHeap",
        &[8, bytes.len() as u32],
        move |env, key| {
            if key == 0 {
                return Outcome::Return(ft::FT_Err_Out_Of_Memory as u32);
            }
            for at in relocations {
                let relative = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                put(&mut bytes, at, key + relative);
            }
            for (address, _) in &mut mirror.charmaps {
                *address += key;
            }
            for address in mirror.tables.values_mut().chain(mirror.names.iter_mut()) {
                if *address != 0 {
                    *address += key;
                }
            }
            if let Err(e) = write(env, key, &bytes)
                .and_then(|()| env.cpu.write_u32(out, key).map_err(|e| e.to_string()))
            {
                return Outcome::Fail(e);
            }
            if let Err(e) = sync_size(env, key, mirror.host) {
                return Outcome::Fail(e);
            }
            sync(env, key, mirror, 0)
        },
    )
}

/// Face metadata is immutable for the entry points Wine loads. Relocations
/// are guest pointers into this allocation, fixed once after heap allocation.
fn metadata(mirror: &mut Face) -> (Vec<u8>, Vec<usize>) {
    let mut bytes = vec![0; RECORDS];
    let mut relocations = Vec::new();
    // SAFETY: mirror owns the live face and its public arrays.
    unsafe {
        let f = &*mirror.host;
        bytes[..20].copy_from_slice(&longs(&[
            f.num_faces,
            f.face_index,
            f.face_flags,
            f.style_flags,
            f.num_glyphs,
        ]));
        for (at, name) in [(20, f.family_name), (24, f.style_name)] {
            if !name.is_null() {
                let address = append(&mut bytes, CStr::from_ptr(name).to_bytes_with_nul());
                put(&mut bytes, at, address);
                relocations.push(at);
            }
        }
        put(&mut bytes, 28, f.num_fixed_sizes as u32);
        if f.num_fixed_sizes > 0 {
            let start = bytes.len().next_multiple_of(4) as u32;
            for i in 0..f.num_fixed_sizes as usize {
                let size = &*f.available_sizes.add(i);
                let mut data = vec![0; 16];
                short(&mut data, 0, size.height as u16);
                short(&mut data, 2, size.width as u16);
                data[4..].copy_from_slice(&longs(&[size.size, size.x_ppem, size.y_ppem]));
                append(&mut bytes, &data);
            }
            put(&mut bytes, 32, start);
            relocations.push(32);
        }
        put(&mut bytes, 36, f.num_charmaps as u32);
        if f.num_charmaps > 0 {
            let array = append(&mut bytes, &vec![0; f.num_charmaps as usize * 4]);
            put(&mut bytes, 40, array);
            relocations.push(40);
            for i in 0..f.num_charmaps as usize {
                let host = *f.charmaps.add(i);
                let c = &*host;
                let mut data = vec![0; 12];
                put(&mut data, 4, c.encoding);
                short(&mut data, 8, c.platform_id);
                short(&mut data, 10, c.encoding_id);
                let address = append(&mut bytes, &data);
                relocations.push(address as usize);
                put(&mut bytes, array as usize + i * 4, address);
                relocations.push(array as usize + i * 4);
                mirror.charmaps.push((address, host));
                if host == f.charmap {
                    put(&mut bytes, 92, address);
                    relocations.push(92);
                }
            }
        }
        bytes[52..68].copy_from_slice(&longs(&[
            f.bbox.xMin,
            f.bbox.yMin,
            f.bbox.xMax,
            f.bbox.yMax,
        ]));
        for (i, value) in [
            f.units_per_EM,
            f.ascender as u16,
            f.descender as u16,
            f.height as u16,
            f.max_advance_width as u16,
            f.max_advance_height as u16,
            f.underline_position as u16,
            f.underline_thickness as u16,
        ]
        .into_iter()
        .enumerate()
        {
            short(&mut bytes, 68 + i * 2, value);
        }
        if !f.glyph.is_null() {
            put(&mut bytes, 84, SLOT as u32);
            relocations.push(84);
        }
        if !f.size.is_null() {
            put(&mut bytes, 88, SIZE as u32);
            relocations.push(88);
        }
        for (tag, fields) in [
            (ft::ft_sfnt_head, b"llllssllllsssssssss".as_slice()),
            (
                ft::ft_sfnt_os2,
                b"ssssssssssssssssbbbbbbbbbbllllbbbbssssssssllsssssss".as_slice(),
            ),
            (ft::ft_sfnt_hhea, b"lsssssssssssssssspp".as_slice()),
            (ft::ft_sfnt_post, b"llsslllll".as_slice()),
        ] {
            let table = ft::FT_Get_Sfnt_Table(mirror.host, tag);
            if !table.is_null() {
                let address = append(&mut bytes, &pack(table.cast(), fields));
                mirror.tables.insert(tag, address);
            }
        }
        for i in 0..ft::FT_Get_Sfnt_Name_Count(mirror.host) {
            let mut name = std::mem::zeroed();
            if ft::FT_Get_Sfnt_Name(mirror.host, i, &mut name) == 0 {
                let data = if name.string_len == 0 {
                    &[]
                } else {
                    std::slice::from_raw_parts(name.string, name.string_len as usize)
                };
                mirror.names.push(if data.is_empty() {
                    0
                } else {
                    append(&mut bytes, data)
                });
            } else {
                mirror.names.push(0);
            }
        }
    }
    (bytes, relocations)
}

fn done_face(env: &mut Env<'_>) -> Outcome {
    let key = env.arg(0);
    let Some(face) = env.ext.get_or_default::<State>().faces.remove(&key) else {
        return Outcome::Return(ft::FT_Err_Invalid_Face_Handle as u32);
    };
    let data = face.data;
    drop(face);
    heap_call(env, "RtlFreeHeap", &[0, key], move |env, _| {
        if data == 0 {
            Outcome::Return(0)
        } else {
            heap_call(env, "RtlFreeHeap", &[0, data], |_, _| Outcome::Return(0))
        }
    })
}

fn mutate(env: &mut Env<'_>, name: &str) -> Outcome {
    let (arg, a, b) = (
        env.arg(0),
        env.arg(1),
        if name == "FT_Render_Glyph" {
            0
        } else {
            env.arg(2)
        },
    );
    let key = if name == "FT_Render_Glyph" {
        arg.wrapping_sub(SLOT as u32)
    } else {
        arg
    };
    let Some(mirror) = env.ext.get_or_default::<State>().faces.remove(&key) else {
        return Outcome::Return(if name == "FT_Render_Glyph" {
            ft::FT_Err_Invalid_Argument
        } else {
            ft::FT_Err_Invalid_Face_Handle
        } as u32);
    };
    // SAFETY: the registry owns this face and its slot.
    let error = unsafe {
        match name {
            "FT_Load_Glyph" => ft::FT_Load_Glyph(mirror.host, a, b as i32),
            "FT_Set_Pixel_Sizes" => ft::FT_Set_Pixel_Sizes(mirror.host, a, b),
            _ => {
                if let Err(e) = import_slot(env, key, mirror.host) {
                    return Outcome::Fail(e);
                }
                ft::FT_Render_Glyph((*mirror.host).glyph, a)
            }
        }
    };
    if name == "FT_Set_Pixel_Sizes" {
        let written = sync_size(env, key, mirror.host);
        env.ext.get_or_default::<State>().faces.insert(key, mirror);
        return match written {
            Ok(()) => Outcome::Return(error as u32),
            Err(e) => Outcome::Fail(e),
        };
    }
    sync(env, key, mirror, error as u32)
}

fn sync_size(env: &mut Env<'_>, key: u32, host: ft::FT_Face) -> Result<(), String> {
    // SAFETY: the face owns its current size.
    let size = unsafe { (*host).size };
    if !size.is_null() {
        let mut bytes = vec![0; 44];
        put(&mut bytes, 0, key);
        // SAFETY: size belongs to the live face.
        let m = unsafe { (*size).metrics };
        short(&mut bytes, 12, m.x_ppem);
        short(&mut bytes, 14, m.y_ppem);
        bytes[16..40].copy_from_slice(&longs(&[
            m.x_scale,
            m.y_scale,
            m.ascender,
            m.descender,
            m.height,
            m.max_advance,
        ]));
        write(env, key + SIZE as u32, &bytes)?;
    }
    Ok(())
}

fn sync(env: &mut Env<'_>, key: u32, mirror: Face, result: u32) -> Outcome {
    let (mut slot, data, relocations) = snapshot(key, &mirror);
    let needed = data.len();
    let finish = move |env: &mut Env<'_>, mirror: Face| {
        for at in relocations {
            let relative = u32::from_le_bytes(slot[at..at + 4].try_into().unwrap());
            put(&mut slot, at, mirror.data + relative);
        }
        let written = (|| {
            if !data.is_empty() {
                write(env, mirror.data, &data)?;
            }
            write(env, key + SLOT as u32, &slot)
        })();
        // Keep the allocation and host object alive even if a guest write fails.
        env.ext.get_or_default::<State>().faces.insert(key, mirror);
        match written {
            Ok(()) => Outcome::Return(result),
            Err(e) => Outcome::Fail(e),
        }
    };
    if needed > mirror.capacity {
        let size = needed as u32;
        let (name, args) = if mirror.data == 0 {
            ("RtlAllocateHeap", vec![0, size])
        } else {
            ("RtlReAllocateHeap", vec![0, mirror.data, size])
        };
        heap_call(env, name, &args, move |env, block| {
            let mut mirror = mirror;
            if block == 0 {
                env.ext.get_or_default::<State>().faces.insert(key, mirror);
                return Outcome::Return(ft::FT_Err_Out_Of_Memory as u32);
            }
            mirror.data = block;
            mirror.capacity = size as usize;
            finish(env, mirror)
        })
    } else {
        finish(env, mirror)
    }
}

fn snapshot(key: u32, mirror: &Face) -> (Vec<u8>, Vec<u8>, Vec<usize>) {
    let mut slot = vec![0; 160];
    let mut data = Vec::new();
    let mut relocations = Vec::new();
    // SAFETY: the face owns all the arrays copied below.
    unsafe {
        let g = (*mirror.host).glyph;
        if g.is_null() {
            return (slot, data, relocations);
        }
        let g = &*g;
        put(&mut slot, 0, mirror.library);
        put(&mut slot, 4, key);
        put(&mut slot, 12, g.reserved);
        let m = g.metrics;
        slot[24..56].copy_from_slice(&longs(&[
            m.width,
            m.height,
            m.horiBearingX,
            m.horiBearingY,
            m.horiAdvance,
            m.vertBearingX,
            m.vertBearingY,
            m.vertAdvance,
        ]));
        slot[56..72].copy_from_slice(&longs(&[
            g.linearHoriAdvance,
            g.linearVertAdvance,
            g.advance.x,
            g.advance.y,
        ]));
        put(&mut slot, 72, g.format);
        let b = &g.bitmap;
        put(&mut slot, 76, b.rows as u32);
        put(&mut slot, 80, b.width as u32);
        put(&mut slot, 84, b.pitch as u32);
        short(&mut slot, 92, b.num_grays as u16);
        slot[94] = b.pixel_mode as u8;
        slot[95] = b.palette_mode as u8;
        if !b.buffer.is_null() {
            let length = b.rows as usize * b.pitch.unsigned_abs() as usize;
            let offset = append(&mut data, std::slice::from_raw_parts(b.buffer, length));
            put(&mut slot, 88, offset);
            relocations.push(88);
        }
        put(&mut slot, 100, g.bitmap_left as u32);
        put(&mut slot, 104, g.bitmap_top as u32);
        let o = &g.outline;
        short(&mut slot, 108, o.n_contours as u16);
        short(&mut slot, 110, o.n_points as u16);
        put(&mut slot, 124, o.flags as u32);
        if o.n_points > 0 {
            let mut points = Vec::new();
            for p in std::slice::from_raw_parts(o.points, o.n_points as usize) {
                points.extend_from_slice(&longs(&[p.x, p.y]));
            }
            put(&mut slot, 112, append(&mut data, &points));
            relocations.push(112);
            put(
                &mut slot,
                116,
                append(
                    &mut data,
                    std::slice::from_raw_parts(o.tags.cast(), o.n_points as usize),
                ),
            );
            relocations.push(116);
        }
        if o.n_contours > 0 {
            let contours: Vec<u8> = std::slice::from_raw_parts(o.contours, o.n_contours as usize)
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            put(&mut slot, 120, append(&mut data, &contours));
            relocations.push(120);
        }
        put(&mut slot, 144, g.lsb_delta as u32);
        put(&mut slot, 148, g.rsb_delta as u32);
    }
    (slot, data, relocations)
}

struct Outline {
    raw: ft::FT_Outline,
    points: Vec<ft::FT_Vector>,
    tags: Vec<::libc::c_char>,
    contours: Vec<i16>,
    guest_points: u32,
}
impl Outline {
    fn read(env: &Env<'_>, address: u32) -> Result<Self, String> {
        let bytes = read(env, address, 20)?;
        let n_contours = i16::from_le_bytes(bytes[..2].try_into().unwrap());
        let n_points = i16::from_le_bytes(bytes[2..4].try_into().unwrap());
        let guest_points = word(env, address + 4)?;
        let mut points = Vec::new();
        for i in 0..n_points.max(0) as u32 {
            points.push(vector(env, guest_points + i * 8)?);
        }
        let mut tags: Vec<::libc::c_char> = if n_points > 0 {
            read(env, word(env, address + 8)?, n_points as usize)?
                .into_iter()
                .map(|v| v as ::libc::c_char)
                .collect()
        } else {
            Vec::new()
        };
        let mut contours = Vec::new();
        for i in 0..n_contours.max(0) as u32 {
            let bytes = read(env, word(env, address + 12)? + i * 2, 2)?;
            contours.push(i16::from_le_bytes(bytes.try_into().unwrap()));
        }
        let raw = ft::FT_Outline {
            n_contours,
            n_points,
            points: if points.is_empty() {
                ptr::null_mut()
            } else {
                points.as_mut_ptr()
            },
            tags: if tags.is_empty() {
                ptr::null_mut()
            } else {
                tags.as_mut_ptr()
            },
            contours: if contours.is_empty() {
                ptr::null_mut()
            } else {
                contours.as_mut_ptr()
            },
            flags: word(env, address + 16)? as i32,
        };
        Ok(Self {
            raw,
            points,
            tags,
            contours,
            guest_points,
        })
    }
    fn write_points(&self, env: &mut Env<'_>) -> Result<(), String> {
        let mut bytes = Vec::new();
        for p in &self.points {
            bytes.extend_from_slice(&longs(&[p.x, p.y]));
        }
        if !bytes.is_empty() {
            write(env, self.guest_points, &bytes)?;
        }
        Ok(())
    }
}
fn vector(env: &Env<'_>, address: u32) -> Result<ft::FT_Vector, String> {
    Ok(ft::FT_Vector {
        x: word(env, address)? as i32 as ::libc::c_long,
        y: word(env, address + 4)? as i32 as ::libc::c_long,
    })
}
/// FreeType's CORDIC length with i386 FT_Pos arithmetic. The distribution's
/// optimized prenorm shifts left for MSB 31; x86 masks that shift to 30.
fn vector_length(vector: &ft::FT_Vector) -> u32 {
    let (mut x, mut y) = (vector.x as i32, vector.y as i32);
    if x == 0 {
        return y.wrapping_abs() as u32;
    }
    if y == 0 {
        return x.wrapping_abs() as u32;
    }
    let msb = 31 - ((x.wrapping_abs() | y.wrapping_abs()) as u32).leading_zeros() as i32;
    let shift = 29 - msb;
    if msb == 30 {
        x >>= 1;
        y >>= 1;
    } else {
        x = x.wrapping_shl(shift as u32);
        y = y.wrapping_shl(shift as u32);
    }
    if y > x {
        if y > x.wrapping_neg() {
            (x, y) = (y, x.wrapping_neg());
        } else {
            x = x.wrapping_neg();
            y = y.wrapping_neg();
        }
    } else if y < x.wrapping_neg() {
        (x, y) = (y.wrapping_neg(), x);
    }
    let mut rounding = 1i32;
    for i in 1..23 {
        let next;
        if y > 0 {
            next = x.wrapping_add(y.wrapping_add(rounding) >> i);
            y = y.wrapping_sub(x.wrapping_add(rounding) >> i);
        } else {
            next = x.wrapping_sub(y.wrapping_add(rounding) >> i);
            y = y.wrapping_add(x.wrapping_add(rounding) >> i);
        }
        x = next;
        rounding <<= 1;
    }
    let scaled = ((x.wrapping_abs() as i64 as u64)
        .wrapping_mul(0xdbd95b16)
        .wrapping_add(0x40000000)
        >> 32) as i32;
    x = if x < 0 { scaled.wrapping_neg() } else { scaled };
    if shift > 0 {
        (x.wrapping_add(1 << (shift - 1)) >> shift) as u32
    } else {
        x.wrapping_shl((-shift) as u32) as u32
    }
}

fn matrix(env: &Env<'_>, address: u32) -> Result<ft::FT_Matrix, String> {
    Ok(ft::FT_Matrix {
        xx: word(env, address)? as i32 as ::libc::c_long,
        xy: word(env, address + 4)? as i32 as ::libc::c_long,
        yx: word(env, address + 8)? as i32 as ::libc::c_long,
        yy: word(env, address + 12)? as i32 as ::libc::c_long,
    })
}

fn import_slot(env: &Env<'_>, key: u32, host: ft::FT_Face) -> Result<(), String> {
    let outline = Outline::read(env, key + SLOT as u32 + 108)?;
    // SAFETY: host owns the slot's allocations. Wine edits the mirrored points
    // in place, and the counts identify the allocation we are updating.
    unsafe {
        let g = &mut *(*host).glyph;
        if outline.raw.n_points != g.outline.n_points
            || outline.raw.n_contours != g.outline.n_contours
        {
            return Err("FT_Render_Glyph: changed slot outline allocation".into());
        }
        for (i, point) in outline.points.iter().enumerate() {
            *g.outline.points.add(i) = *point;
        }
        for (i, tag) in outline.tags.iter().enumerate() {
            *g.outline.tags.add(i) = *tag;
        }
        for (i, contour) in outline.contours.iter().enumerate() {
            *g.outline.contours.add(i) = *contour;
        }
        g.outline.flags = outline.raw.flags;
    }
    Ok(())
}

fn outline_call(env: &mut Env<'_>, name: &str) -> Result<u32, String> {
    let (a, b) = (env.arg(0), env.arg(1));
    if name == "FT_Outline_Get_Bitmap" && env.arg(2) == 0 {
        return Ok(ft::FT_Err_Invalid_Argument as u32);
    }
    let address = if name == "FT_Outline_Get_Bitmap" {
        b
    } else {
        a
    };
    if address == 0 {
        return Ok(match name {
            "FT_Outline_Embolden" => ft::FT_Err_Invalid_Outline as u32,
            "FT_Outline_Get_Bitmap" if library(env, a).is_null() => {
                ft::FT_Err_Invalid_Library_Handle as u32
            }
            "FT_Outline_Get_Bitmap" => ft::FT_Err_Invalid_Outline as u32,
            _ => 0,
        });
    }
    let mut outline = Outline::read(env, address)?;
    // SAFETY: the copy's arrays are owned by outline throughout each call.
    let result = unsafe {
        match name {
            "FT_Outline_Transform" => {
                if b != 0 {
                    ft::FT_Outline_Transform(&outline.raw, &matrix(env, b)?);
                }
                0
            }
            "FT_Outline_Translate" => {
                ft::FT_Outline_Translate(
                    &outline.raw,
                    b as i32 as ::libc::c_long,
                    env.arg(2) as i32 as ::libc::c_long,
                );
                0
            }
            "FT_Outline_Embolden" => {
                ft::FT_Outline_Embolden(&mut outline.raw, b as i32 as ::libc::c_long)
            }
            "FT_Outline_Get_CBox" => {
                if b != 0 {
                    let mut bbox = ft::FT_BBox::default();
                    ft::FT_Outline_Get_CBox(&outline.raw, &mut bbox);
                    write(
                        env,
                        b,
                        &longs(&[bbox.xMin, bbox.yMin, bbox.xMax, bbox.yMax]),
                    )?;
                }
                return Ok(0);
            }
            _ => {
                let address = env.arg(2);
                let (rows, width, pitch, buffer) = (
                    word(env, address)?,
                    word(env, address + 4)?,
                    word(env, address + 8)? as i32,
                    word(env, address + 12)?,
                );
                let mut data = read(env, buffer, rows as usize * pitch.unsigned_abs() as usize)?;
                let format = word(env, address + 16)?;
                let bitmap = ft::FT_Bitmap {
                    rows: rows as i32,
                    width: width as i32,
                    pitch,
                    buffer: if buffer == 0 {
                        ptr::null_mut()
                    } else {
                        data.as_mut_ptr()
                    },
                    num_grays: format as i16,
                    pixel_mode: (format >> 16) as ::libc::c_char,
                    palette_mode: (format >> 24) as ::libc::c_char,
                    palette: ptr::null_mut(),
                };
                let result = ft::FT_Outline_Get_Bitmap(library(env, a), &mut outline.raw, &bitmap);
                if !data.is_empty() {
                    write(env, buffer, &data)?;
                }
                return Ok(result as u32);
            }
        }
    };
    outline.write_points(env)?;
    Ok(result as u32)
}

fn scalar(env: &mut Env<'_>, name: &str) -> Result<u32, String> {
    if name.starts_with("FT_Outline_") {
        return outline_call(env, name);
    }
    let a = env.arg(0);
    let b = if matches!(
        name,
        "FT_Get_TrueType_Engine_Type" | "FT_Get_Sfnt_Name_Count" | "FT_Vector_Length"
    ) {
        0
    } else {
        env.arg(1)
    };
    // SAFETY: handles come exclusively from the owning registry; structures
    // are copied from guest memory before passing their host counterparts.
    unsafe {
        Ok(match name {
            "FT_Library_Version" => {
                let (mut major, mut minor, mut patch) = (0, 0, 0);
                FT_Library_Version(library(env, a), &mut major, &mut minor, &mut patch);
                for (at, value) in [(b, major), (env.arg(2), minor), (env.arg(3), patch)] {
                    if at != 0 {
                        env.cpu
                            .write_u32(at, value as u32)
                            .map_err(|e| e.to_string())?;
                    }
                }
                0
            }
            "FT_Get_TrueType_Engine_Type" => FT_Get_TrueType_Engine_Type(library(env, a)),
            "FT_Library_SetLcdFilter" => ft::FT_Library_SetLcdFilter(library(env, a), b) as u32,
            "FT_Property_Set" => {
                let module =
                    CString::new(env.cpu.read_cstr(b, 256).map_err(|e| e.to_string())?).unwrap();
                let property = CString::new(
                    env.cpu
                        .read_cstr(env.arg(2), 256)
                        .map_err(|e| e.to_string())?,
                )
                .unwrap();
                // Wine sets the TrueType interpreter version (FT_UInt).
                let value = word(env, env.arg(3))?;
                ft::FT_Property_Set(
                    library(env, a),
                    module.as_ptr(),
                    property.as_ptr(),
                    (&value as *const u32).cast(),
                ) as u32
            }
            "FT_Get_Char_Index" => ft::FT_Get_Char_Index(face(env, a), b as ft::FT_ULong),
            "FT_Get_First_Char" | "FT_Get_Next_Char" => {
                let mut index = 0;
                let code = if name == "FT_Get_First_Char" {
                    ft::FT_Get_First_Char(face(env, a), &mut index)
                } else {
                    ft::FT_Get_Next_Char(face(env, a), b as ft::FT_ULong, &mut index)
                };
                let out = if name == "FT_Get_First_Char" {
                    b
                } else {
                    env.arg(2)
                };
                if out != 0 {
                    env.cpu.write_u32(out, index).map_err(|e| e.to_string())?;
                }
                code as u32
            }
            "FT_Set_Charmap" => {
                let host = face(env, a);
                let map = env
                    .ext
                    .get_or_default::<State>()
                    .faces
                    .values()
                    .flat_map(|f| &f.charmaps)
                    .find(|(k, _)| *k == b)
                    .map_or(ptr::null_mut(), |(_, c)| *c);
                let error = ft::FT_Set_Charmap(host, map);
                if error == 0 {
                    env.cpu.write_u32(a + 92, b).map_err(|e| e.to_string())?;
                }
                error as u32
            }
            "FT_Get_Sfnt_Table" => env
                .ext
                .get_or_default::<State>()
                .faces
                .get(&a)
                .and_then(|f| f.tables.get(&b))
                .copied()
                .unwrap_or(0),
            "FT_Get_Sfnt_Name_Count" => ft::FT_Get_Sfnt_Name_Count(face(env, a)),
            "FT_Get_Sfnt_Name" => {
                let mut record = std::mem::zeroed();
                let out = env.arg(2);
                let error = ft::FT_Get_Sfnt_Name(
                    face(env, a),
                    b,
                    if out == 0 {
                        ptr::null_mut()
                    } else {
                        &mut record
                    },
                );
                if error == 0 {
                    let string = env.ext.get_or_default::<State>().faces[&a].names[b as usize];
                    let mut bytes = vec![0; 16];
                    for (i, v) in [
                        record.platform_id,
                        record.encoding_id,
                        record.language_id,
                        record.name_id,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        short(&mut bytes, i * 2, v);
                    }
                    put(&mut bytes, 8, string);
                    put(&mut bytes, 12, record.string_len);
                    write(env, env.arg(2), &bytes)?;
                }
                error as u32
            }
            "FT_Load_Sfnt_Table" => {
                let (offset, buffer, length) = (env.arg(2), env.arg(3), env.arg(4));
                let mut len = if length == 0 {
                    0
                } else {
                    word(env, length)? as ft::FT_ULong
                };
                let mut bytes = if buffer == 0 {
                    Vec::new()
                } else {
                    read(env, buffer, len as usize)?
                };
                let result = ft::FT_Load_Sfnt_Table(
                    face(env, a),
                    b as ft::FT_ULong,
                    offset as i32 as ft::FT_Long,
                    if buffer == 0 {
                        ptr::null_mut()
                    } else {
                        bytes.as_mut_ptr()
                    },
                    if length == 0 {
                        ptr::null_mut()
                    } else {
                        &mut len
                    },
                );
                if length != 0 {
                    env.cpu
                        .write_u32(length, len as u32)
                        .map_err(|e| e.to_string())?;
                }
                if !bytes.is_empty() {
                    write(env, buffer, &bytes)?;
                }
                result as u32
            }
            "FT_Get_WinFNT_Header" => {
                // Largest host record: 32-bit fields widen and align on LP64.
                let mut header = [0u64; 32];
                let result = FT_Get_WinFNT_Header(
                    face(env, a),
                    if b == 0 {
                        ptr::null_mut()
                    } else {
                        header.as_mut_ptr().cast()
                    },
                );
                if result == 0 {
                    let bytes = pack(header.as_ptr().cast(), b"slbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbsssssssbbbsbssbssbbbbsllllblssssllll");
                    write(env, b, &bytes)?;
                }
                result as u32
            }
            "FT_MulDiv" => ft::FT_MulDiv(
                a as i32 as ft::FT_Long,
                b as i32 as ft::FT_Long,
                env.arg(2) as i32 as ft::FT_Long,
            ) as u32,
            "FT_MulFix" => ft::FT_MulFix(a as i32 as ft::FT_Long, b as i32 as ft::FT_Long) as u32,
            "FT_Matrix_Multiply" => {
                if a != 0 && b != 0 {
                    let left = matrix(env, a)?;
                    let mut right = matrix(env, b)?;
                    FT_Matrix_Multiply(&left, &mut right);
                    write(env, b, &longs(&[right.xx, right.xy, right.yx, right.yy]))?;
                }
                0
            }
            "FT_Vector_Length" => {
                if a == 0 {
                    0
                } else {
                    vector_length(&vector(env, a)?)
                }
            }
            "FT_Vector_Transform" => {
                if a != 0 && b != 0 {
                    let mut v = vector(env, a)?;
                    FT_Vector_Transform(&mut v, &matrix(env, b)?);
                    write(env, a, &longs(&[v.x, v.y]))?;
                }
                0
            }
            "FT_Vector_Unit" => {
                if a != 0 {
                    let mut v = ft::FT_Vector::default();
                    FT_Vector_Unit(&mut v, b as i32 as ft::FT_Long);
                    write(env, a, &longs(&[v.x, v.y]))?;
                }
                0
            }
            _ => unreachable!(),
        })
    }
}
