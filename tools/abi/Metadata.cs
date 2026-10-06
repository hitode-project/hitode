// Offline x86 projection of Microsoft's Windows.Win32.winmd. No runtime dependencies.
using System;
using System.Collections.Generic;
using System.Collections.Immutable;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Text;

public static class AbiMetadata
{
    sealed record Ty(string Name, string Kind, int Size, int Align, string Rust);

    sealed class Types : ISignatureTypeProvider<Ty, object>
    {
        public readonly MetadataReader Reader;
        readonly Dictionary<string, TypeDefinitionHandle> definitions = new();
        readonly Dictionary<string, Ty> values = new();
        readonly HashSet<string> active = new();
        public Types(MetadataReader reader)
        {
            Reader = reader;
            foreach (var h in reader.TypeDefinitions)
            {
                var d = reader.GetTypeDefinition(h);
                if (X86(reader, d.GetCustomAttributes()))
                    definitions[FullName(reader, h)] = h;
            }
        }
        public TypeDefinitionHandle Find(string name) => definitions.Values.FirstOrDefault(h => Reader.GetString(Reader.GetTypeDefinition(h).Name) == name) is var h && !h.IsNil ? h : throw new InvalidDataException("type missing: " + name);
        public IEnumerable<TypeDefinitionHandle> Definitions => definitions.Values;
        public Ty GetPrimitiveType(PrimitiveTypeCode code) => code switch
        {
            PrimitiveTypeCode.Void => new("void", "void", 0, 1, "()"),
            PrimitiveTypeCode.Boolean => new("bool", "u32", 1, 1, "u8"),
            PrimitiveTypeCode.Byte => new("u8", "u32", 1, 1, "u8"),
            PrimitiveTypeCode.SByte => new("i8", "i32", 1, 1, "i8"),
            PrimitiveTypeCode.Char or PrimitiveTypeCode.UInt16 => new("u16", "u32", 2, 2, "u16"),
            PrimitiveTypeCode.Int16 => new("i16", "i32", 2, 2, "i16"),
            PrimitiveTypeCode.UInt32 => new("u32", "u32", 4, 4, "u32"),
            PrimitiveTypeCode.Int32 => new("i32", "i32", 4, 4, "i32"),
            PrimitiveTypeCode.UInt64 => new("u64", "u64", 8, 8, "u64"),
            PrimitiveTypeCode.Int64 => new("i64", "i64", 8, 8, "i64"),
            PrimitiveTypeCode.Single => new("f32", "f32", 4, 4, "f32"),
            PrimitiveTypeCode.Double => new("f64", "f64", 8, 8, "f64"),
            PrimitiveTypeCode.IntPtr => new("isize", "i32", 4, 4, "i32"),
            PrimitiveTypeCode.UIntPtr => new("usize", "u32", 4, 4, "u32"),
            _ => new(code.ToString(), "unsupported", 0, 1, ""),
        };
        public Ty GetPointerType(Ty element) => new(element.Name + "*", "ptr", 4, 4, "u32");
        public Ty GetByReferenceType(Ty element) => GetPointerType(element);
        public Ty GetSZArrayType(Ty element) => new(element.Name + "[]", "unsupported", 0, 1, "");
        public Ty GetArrayType(Ty element, ArrayShape shape)
        {
            if (shape.Rank != 1 || shape.Sizes.Length != 1) return GetSZArrayType(element);
            return new(element.Name + "[" + shape.Sizes[0] + "]", "aggregate", element.Size * shape.Sizes[0], element.Align, "[" + element.Rust + "; " + shape.Sizes[0] + "]");
        }
        public Ty GetFunctionPointerType(MethodSignature<Ty> signature) => new("fn*", "ptr", 4, 4, "u32");
        public Ty GetGenericInstantiation(Ty generic, ImmutableArray<Ty> args) => new(generic.Name, "unsupported", 0, 1, "");
        public Ty GetGenericMethodParameter(object context, int index) => new("!!" + index, "unsupported", 0, 1, "");
        public Ty GetGenericTypeParameter(object context, int index) => new("!" + index, "unsupported", 0, 1, "");
        public Ty GetModifiedType(Ty modifier, Ty value, bool required) => value;
        public Ty GetPinnedType(Ty value) => value;
        public Ty GetTypeFromSpecification(MetadataReader reader, object context, TypeSpecificationHandle handle, byte kind) => reader.GetTypeSpecification(handle).DecodeSignature(this, context);
        public Ty GetTypeFromDefinition(MetadataReader reader, TypeDefinitionHandle handle, byte kind) => Named(FullName(reader, handle), kind);
        public Ty GetTypeFromReference(MetadataReader reader, TypeReferenceHandle handle, byte kind) => Named(FullName(reader, handle), kind);
        public Ty Entity(EntityHandle h) => h.Kind switch
        {
            HandleKind.TypeDefinition => GetTypeFromDefinition(Reader, (TypeDefinitionHandle)h, 0x12),
            HandleKind.TypeReference => GetTypeFromReference(Reader, (TypeReferenceHandle)h, 0x12),
            _ => new("unknown", "unsupported", 0, 1, ""),
        };
        Ty Named(string name, byte kind)
        {
            if (values.TryGetValue(name, out var cached)) return cached;
            // WinMD references this CLR primitive instead of defining GUID.
            // Four opaque DWORDs preserve its 16 bytes and native alignment;
            // callers interpret identifier bytes, never CLR object fields.
            if (name == "System.Guid") return new(name, "aggregate", 16, 4, "[u32; 4]");
            if (!definitions.TryGetValue(name, out var h))
                return new(name, "unsupported", 0, 1, "");
            var d = Reader.GetTypeDefinition(h);
            string shortName = Reader.GetString(d.Name);
            if ((d.Attributes & TypeAttributes.Interface) != 0 || kind == 0x12 && BaseName(d) != "System.ValueType" && BaseName(d) != "System.Enum")
                return new(name, "ptr", 4, 4, "u32");
            if (!active.Add(name)) return new(name, "unsupported", 0, 1, "");
            var fields = d.GetFields().Select(f => Reader.GetFieldDefinition(f)).Where(f => (f.Attributes & FieldAttributes.Static) == 0).ToArray();
            Ty result;
            if (BaseName(d) == "System.Enum" || HasAttribute(Reader, d.GetCustomAttributes(), "NativeTypedefAttribute"))
            {
                var inner = fields.Single().DecodeSignature(this, null);
                var abiKind = inner.Kind;
                if (shortName == "PCSTR" || shortName == "PSTR") abiKind = "str8";
                if (shortName == "PCWSTR" || shortName == "PWSTR") abiKind = "str16";
                if (shortName.StartsWith("H") && inner.Kind == "ptr" && shortName != "HRESULT") abiKind = "handle";
                result = inner with { Name = name, Kind = abiKind };
            }
            else
            {
                int size = 0, align = 1;
                var layout = d.GetLayout();
                int pack = layout.PackingSize == 0 ? 8 : layout.PackingSize;
                foreach (var f in fields)
                {
                    var t = f.DecodeSignature(this, null);
                    if (t.Kind == "unsupported") { size = 0; break; }
                    int fa = Math.Min(pack, t.Align);
                    align = Math.Max(align, fa);
                    int offset = f.GetOffset();
                    if (offset < 0) offset = Round(size, fa);
                    size = Math.Max(size, offset + t.Size);
                }
                size = Math.Max(Round(size, align), layout.Size);
                result = new(name, size == 0 ? "unsupported" : "aggregate", size, align, shortName);
            }
            active.Remove(name);
            values[name] = result;
            return result;
        }
        string BaseName(TypeDefinition d) => d.BaseType.IsNil ? "" : FullName(Reader, d.BaseType);
    }

    static int Round(int n, int align) => (n + align - 1) / align * align;
    static string FullName(MetadataReader r, EntityHandle h)
    {
        if (h.IsNil) return "";
        if (h.Kind == HandleKind.TypeDefinition) { var t = r.GetTypeDefinition((TypeDefinitionHandle)h); return r.GetString(t.Namespace) + "." + r.GetString(t.Name); }
        if (h.Kind == HandleKind.TypeReference) { var t = r.GetTypeReference((TypeReferenceHandle)h); return r.GetString(t.Namespace) + "." + r.GetString(t.Name); }
        return "";
    }
    static string AttributeName(MetadataReader r, CustomAttributeHandle h)
    {
        var a = r.GetCustomAttribute(h);
        EntityHandle parent = a.Constructor.Kind == HandleKind.MemberReference
            ? r.GetMemberReference((MemberReferenceHandle)a.Constructor).Parent
            : r.GetMethodDefinition((MethodDefinitionHandle)a.Constructor).GetDeclaringType();
        return FullName(r, parent).Split('.').Last();
    }
    static bool HasAttribute(MetadataReader r, CustomAttributeHandleCollection attrs, string name) => attrs.Any(a => AttributeName(r, a) == name);
    static bool X86(MetadataReader r, CustomAttributeHandleCollection attrs)
    {
        foreach (var a in attrs)
            if (AttributeName(r, a) == "SupportedArchitectureAttribute")
            {
                var blob = r.GetBlobReader(r.GetCustomAttribute(a).Value);
                if (blob.ReadUInt16() != 1) throw new InvalidDataException("attribute prologue");
                return (blob.ReadInt32() & 1) != 0;
            }
        return true;
    }
    static string Parameters(MetadataReader r, MethodDefinition m, MethodSignature<Ty> sig)
    {
        var names = m.GetParameters().Select(p => r.GetParameter(p)).Where(p => p.SequenceNumber > 0).ToDictionary(p => p.SequenceNumber, p => r.GetString(p.Name));
        return string.Join("|", sig.ParameterTypes.Select((t, i) => names.GetValueOrDefault(i + 1, "arg" + i) + ":" + t.Name + ":" + t.Kind + ":" + Round(t.Size, 4) / 4));
    }

    public static void Generate(string winmd, string output, string modulesFile, string supplement, string selectedExports)
        => Generate(winmd, output, modulesFile, supplement, selectedExports, null);

    public static void Generate(string winmd, string output, string modulesFile, string supplement, string selectedExports, string sdkSupplement)
    {
        using var stream = File.OpenRead(winmd);
        using var pe = new PEReader(stream);
        var r = pe.GetMetadataReader();
        var types = new Types(r);
        var modules = File.ReadAllLines(modulesFile).Where(s => s.Length > 0 && !s.StartsWith("#")).ToHashSet(StringComparer.OrdinalIgnoreCase);
        var selected = File.ReadLines(selectedExports).Where(line => line.Length != 0 && !line.StartsWith('#')).Select(line =>
        {
            var f = line.Split('\t');
            if (f.Length != 5 || f.Take(4).Any(s => s.Length == 0) || !int.TryParse(f[4], out int words) || words < 0 || words > 255)
                throw new InvalidDataException("Invalid selected x86 export: " + line);
            return (Destination: f[0] + "\t" + f[1], Source: f[2] + "\t" + f[3], Words: words);
        }).ToArray();
        if (selected.Select(s => s.Destination).Distinct(StringComparer.Ordinal).Count() != selected.Length)
            throw new InvalidDataException("Duplicate selected x86 export destination");
        var selectedSources = selected.Select(s => s.Source).ToHashSet(StringComparer.Ordinal);
        var rows = new SortedDictionary<string, string>(StringComparer.Ordinal);
        var referenced = new HashSet<string>();
        foreach (var h in r.MethodDefinitions)
        {
            var m = r.GetMethodDefinition(h);
            if ((m.Attributes & MethodAttributes.PinvokeImpl) == 0 || !X86(r, m.GetCustomAttributes())) continue;
            var import = m.GetImport();
            string module = r.GetString(r.GetModuleReference(import.Module).Name).ToLowerInvariant();
            if (!module.EndsWith(".dll") && !module.EndsWith(".drv")) module += ".dll";
            string name = r.GetString(import.Name);
            if (!modules.Contains(module) && !selectedSources.Contains(module + "\t" + name)) continue;
            var sig = m.DecodeSignature(types, null);
            referenced.Add(sig.ReturnType.Name);
            foreach (var parameter in sig.ParameterTypes) referenced.Add(parameter.Name);
            string conv = (import.Attributes & MethodImportAttributes.CallingConventionMask) switch
            {
                MethodImportAttributes.CallingConventionCDecl => "cdecl",
                MethodImportAttributes.CallingConventionStdCall or MethodImportAttributes.CallingConventionWinApi => "stdcall",
                _ => "unsupported",
            };
            if (sig.Header.CallingConvention == SignatureCallingConvention.VarArgs) conv = "variadic";
            string row = module + "\t" + name + "\t" + conv + "\t" + sig.ReturnType.Kind + "\t" + sig.ReturnType.Name + "\t" + Parameters(r, m, sig);
            string key = module + "\t" + name;
            if (rows.TryGetValue(key, out var prior) && prior != row) throw new InvalidDataException("Conflicting x86 signature: " + key);
            rows[key] = row;
        }
        foreach (var selection in selected)
        {
            if (!rows.TryGetValue(selection.Source, out var source))
                throw new InvalidDataException("Selected x86 export missing from pinned WinMD: " + selection.Source);
            var f = source.Split('\t');
            int words = f[5].Split('|', StringSplitOptions.RemoveEmptyEntries).Sum(parameter => int.Parse(parameter.Split(':').Last()));
            if (f[2] != "stdcall" || words != selection.Words)
                throw new InvalidDataException("Selected x86 export stack contract changed: " + selection.Source);
            // Aliases inherit the complete source declaration; no parameter or
            // return-type declaration is copied into the selection input.
            string row = selection.Destination + "\t" + string.Join("\t", f.Skip(2));
            if (rows.TryGetValue(selection.Destination, out var prior) && prior != row)
                throw new InvalidDataException("Conflicting selected x86 export: " + selection.Destination);
            rows[selection.Destination] = row;
        }
        Directory.CreateDirectory(output);
        foreach (var row in File.ReadLines(supplement).Where(line => line.Length != 0 && !line.StartsWith('#')))
        {
            var f = row.Split('\t');
            if (f.Length != 6 || !new[] { "cdecl", "data" }.Contains(f[2]) || !new[] { "u32", "i32", "u64", "void", "ptr" }.Contains(f[3])
                || (f[2] == "data" && (f[3] == "void" || f[5].Length != 0)))
                throw new InvalidDataException("Invalid supplemental x86 signature: " + row);
            foreach (var parameter in f[5].Split('|', StringSplitOptions.RemoveEmptyEntries))
            {
                var p = parameter.Split(':');
                if (p.Length != 4 || !new[] { "u32", "i32", "ptr", "str8", "str16" }.Contains(p[2]) || p[3] != "1")
                    throw new InvalidDataException("Invalid supplemental x86 parameter: " + parameter);
            }
            var key = f[0] + "\t" + f[1];
            if (rows.TryGetValue(key, out var prior) && prior != row)
                throw new InvalidDataException("Conflicting supplemental x86 signature: " + key);
            rows[key] = row;
        }
        // Explicit SDK declarations cover exports omitted by the pinned WinMD.
        // Keep their scope narrow and merge into the same runtime signature table.
        if (sdkSupplement != null)
        foreach (var row in File.ReadLines(sdkSupplement).Where(line => line.Length != 0 && !line.StartsWith('#')))
        {
            var f = row.Split('\t');
            bool vcm = f.Length == 6 && f[0] == "msvfw32.dll" && new[] { "ICInstall", "ICRemove" }.Contains(f[1]) && f[3] == "i32";
            bool d3dx = f.Length == 6 && f[0] == "d3dx9_43.dll"
                && new[] { "D3DXMatrixMultiply", "D3DXMatrixTranspose", "D3DXMatrixLookAtLH", "D3DXVec3Project" }.Contains(f[1])
                && f[3] == "ptr" && f[4] == (f[1] == "D3DXVec3Project" ? "D3DXVECTOR3*" : "D3DXMATRIX*");
            bool d3dxEffect = f.Length == 6 && f[0] == "d3dx9_43.dll"
                && new[] { "D3DXCreateEffect", "D3DXGetShaderOutputSemantics" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "HRESULT";
            // The old RTL export is absent from WinMD. Its BOOLEAN (AL) return
            // and RET 16 are verified by the Microsoft x86 wrapper and SDK
            // import library; the WithStatus documentation supplies its four
            // pointer declarations, without aliasing that NTSTATUS return.
            bool ntPath = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "RtlDosPathNameToNtPathName_U"
                && f[3] == "u32" && f[4] == "Windows.Win32.Foundation.BOOLEAN"
                && f[5] == "DosFileName:Windows.Win32.Foundation.PCWSTR:str16:1|NtFileName:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1|FilePart:Windows.Win32.Foundation.PWSTR*:ptr:1|RelativeName:void*:ptr:1";
            // User-mode Nt/Zw share one native service. This sole alias uses
            // the SDK winternl.h NtCreateFile declaration and both SDK x86
            // import symbols @44, without projecting other native services.
            bool ntCreate = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "ZwCreateFile"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "FileHandle:Windows.Win32.Foundation.HANDLE*:ptr:1|DesiredAccess:u32:u32:1|ObjectAttributes:OBJECT_ATTRIBUTES*:ptr:1|IoStatusBlock:IO_STATUS_BLOCK*:ptr:1|AllocationSize:LARGE_INTEGER*:ptr:1|FileAttributes:u32:u32:1|ShareAccess:u32:u32:1|CreateDisposition:u32:u32:1|CreateOptions:u32:u32:1|EaBuffer:void*:ptr:1|EaLength:u32:u32:1";
            // Microsoft Learn's WDM declaration and both SDK ntdll.lib @20
            // symbols supply this one observed native query, absent in WinMD.
            bool ntQuery = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "ZwQueryInformationFile"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "FileHandle:Windows.Win32.Foundation.HANDLE:handle:1|IoStatusBlock:IO_STATUS_BLOCK*:ptr:1|FileInformation:void*:ptr:1|Length:u32:u32:1|FileInformationClass:FILE_INFORMATION_CLASS:i32:1";
            bool ntSet = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "ZwSetInformationFile"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "FileHandle:Windows.Win32.Foundation.HANDLE:handle:1|IoStatusBlock:IO_STATUS_BLOCK*:ptr:1|FileInformation:void*:ptr:1|Length:u32:u32:1|FileInformationClass:FILE_INFORMATION_CLASS:i32:1";
            bool ntRead = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "ZwReadFile"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "FileHandle:Windows.Win32.Foundation.HANDLE:handle:1|Event:Windows.Win32.Foundation.HANDLE:handle:1|ApcRoutine:PIO_APC_ROUTINE:ptr:1|ApcContext:void*:ptr:1|IoStatusBlock:IO_STATUS_BLOCK*:ptr:1|Buffer:void*:ptr:1|Length:u32:u32:1|ByteOffset:LARGE_INTEGER*:ptr:1|Key:u32*:ptr:1";
            bool ntClose = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "ZwClose"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "Handle:Windows.Win32.Foundation.HANDLE:handle:1";
            // Legacy desktop declaration plus the SDK's _EnableEUDC@4 import.
            bool eudc = f.Length == 6 && f[0] == "gdi32.dll" && f[1] == "EnableEUDC"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.BOOL"
                && f[5] == "fEnableEUDC:Windows.Win32.Foundation.BOOL:i32:1";
            if ((!vcm && !d3dx && !d3dxEffect && !ntPath && !ntCreate && !ntQuery && !ntSet && !ntRead && !ntClose && !eudc) || f[2] != "stdcall")
                throw new InvalidDataException("Invalid scoped SDK x86 signature: " + row);
            if (d3dx) {
                int expected = f[1] switch { "D3DXMatrixMultiply" => 3, "D3DXMatrixTranspose" => 2, "D3DXMatrixLookAtLH" => 4, "D3DXVec3Project" => 6, _ => 0 };
                if (f[5].Split('|', StringSplitOptions.RemoveEmptyEntries).Length != expected)
                    throw new InvalidDataException("D3DX x86 stack contract changed: " + row);
            }
            if (d3dxEffect && f[5].Split('|').Length != (f[1] == "D3DXCreateEffect" ? 9 : 3))
                throw new InvalidDataException("D3DX Effect x86 stack contract changed: " + row);
            foreach (var parameter in f[5].Split('|', StringSplitOptions.RemoveEmptyEntries))
            {
                var p = parameter.Split(':');
                if (p.Length != 4 || !(ntPath ? new[] { "ptr", "str16" }.Contains(p[2]) : ntCreate ? new[] { "ptr", "u32" }.Contains(p[2]) : ntQuery || ntSet ? new[] { "handle", "ptr", "u32", "i32" }.Contains(p[2]) : ntRead ? new[] { "handle", "ptr", "u32" }.Contains(p[2]) : ntClose ? p[2] == "handle" : d3dx ? p[2] == "ptr" : d3dxEffect ? new[] { "ptr", "u32" }.Contains(p[2]) : new[] { "u32", "i32", "str8" }.Contains(p[2])) || p[3] != "1")
                    throw new InvalidDataException("Invalid SDK x86 parameter: " + parameter);
            }
            var key = f[0] + "\t" + f[1];
            if (rows.TryGetValue(key, out var prior) && prior != row)
                throw new InvalidDataException("Conflicting SDK x86 signature: " + key);
            rows[key] = row;
        }
        File.WriteAllLines(Path.Combine(output, "signatures.tsv"), new[] { "# Generated by tools/abi/Metadata.cs from pinned Windows.Win32.winmd, selected legacy exports/aliases and SDK supplemental declarations; do not edit.", "# dll\tname\tconvention\treturn kind\treturn type\tparameters (name:type:kind:stack words)" }.Concat(rows.Values), new UTF8Encoding(false));

        var structs = new StringBuilder("// Generated from pinned Windows.Win32.winmd, x86 only.\n#![allow(non_snake_case)]\n");
        foreach (string name in new[] { "FILETIME", "SYSTEMTIME", "POINT", "RECT", "SECURITY_ATTRIBUTES", "D3DPRESENT_PARAMETERS", "D3DLOCKED_RECT", "D3DSURFACE_DESC", "D3DVIEWPORT9", "D3DVERTEXBUFFER_DESC", "D3DINDEXBUFFER_DESC", "D3DDISPLAYMODE", "D3DDEVICE_CREATION_PARAMETERS", "D3DVSHADERCAPS2_0", "D3DPSHADERCAPS2_0", "D3DCAPS9", "D3DADAPTER_IDENTIFIER9" })
        {
            var h = types.Find(name);
            var d = r.GetTypeDefinition(h);
            var t = types.GetTypeFromDefinition(r, h, 0x11);
            if (t.Kind != "aggregate") throw new InvalidDataException("not a struct: " + name);
            var fields = d.GetFields().Select(f => r.GetFieldDefinition(f)).Where(f => (f.Attributes & FieldAttributes.Static) == 0).ToArray();
            bool floatFields = fields.Any(f => f.DecodeSignature(types, null).Kind is "f32" or "f64");
            bool manualDefault = fields.Any(f => { var ft = f.DecodeSignature(types, null); return ft.Rust.StartsWith("[") && ft.Size > 32; });
            int structPack = d.GetLayout().PackingSize;
            string repr = structPack != 0 && fields.Any(f => f.DecodeSignature(types, null).Align > structPack) ? "C, packed(" + structPack + ")" : "C";
            structs.Append("#[repr(" + repr + ")]\n#[derive(Clone, Copy, Debug, " + (manualDefault ? "" : "Default, ") + "PartialEq, " + (floatFields ? "" : "Eq, ") + "bytemuck::Pod, bytemuck::Zeroable)]\npub struct " + name + " {\n");
            int cursor = 0, padding = 0;
            foreach (var f in fields)
            {
                var ft = f.DecodeSignature(types, null);
                int at = f.GetOffset();
                if (at < 0) at = Round(cursor, Math.Min(d.GetLayout().PackingSize == 0 ? 8 : d.GetLayout().PackingSize, ft.Align));
                if (at < cursor) throw new InvalidDataException("overlapping struct fields: " + name);
                if (at > cursor) structs.Append("    pub _padding" + padding++ + ": [u8; " + (at - cursor) + "],\n");
                structs.Append("    pub " + r.GetString(f.Name) + ": " + ft.Rust + ",\n");
                cursor = at + ft.Size;
            }
            if (t.Size > cursor) structs.Append("    pub _padding" + padding + ": [u8; " + (t.Size - cursor) + "],\n");
            structs.Append("}\nconst _: () = assert!(core::mem::size_of::<" + name + ">() == " + t.Size + ");\n");
            structs.Append("const _: () = assert!(core::mem::align_of::<" + name + ">() == " + t.Align + ");\n");
            if (manualDefault) structs.Append("impl Default for " + name + " { fn default() -> Self { <Self as bytemuck::Zeroable>::zeroed() } }\n");
            int offset = 0;
            foreach (var f in d.GetFields().Select(f => r.GetFieldDefinition(f)).Where(f => (f.Attributes & FieldAttributes.Static) == 0))
            {
                var fieldType = f.DecodeSignature(types, null);
                int explicitOffset = f.GetOffset();
                offset = explicitOffset < 0 ? Round(offset, Math.Min(d.GetLayout().PackingSize == 0 ? 8 : d.GetLayout().PackingSize, fieldType.Align)) : explicitOffset;
                structs.Append("const _: () = assert!(core::mem::offset_of!(" + name + ", " + r.GetString(f.Name) + ") == " + offset + ");\n");
                offset += fieldType.Size;
            }
            // Large output records are pointer-only, beyond the by-value ABI's
            // bounded stack word count. GuestPtr<T> still supports their Pod.
            if (Round(t.Size, 4) / 4 <= byte.MaxValue)
                structs.Append("impl crate::GuestArg for " + name + " {\n    const KIND: crate::AbiKind = crate::AbiKind::Aggregate;\n    const WORDS: u8 = " + Round(t.Size, 4) / 4 + ";\n    fn decode(args: &mut crate::Arguments<'_>) -> Result<Self, adv_cpu::backend::MemFault> { args.pod() }\n}\n");
        }
        File.WriteAllText(Path.Combine(output, "structs.rs"), structs.ToString(), new UTF8Encoding(false));

        var com = new List<string> { "# Generated x86 COM vtable signatures: interface\tslot\tmethod\treturn kind\treturn type\tparameters (including this)" };
        foreach (string name in new[] { "IDirectDraw", "IDirectDraw7", "IDirectDrawClipper", "IDirectDrawSurface", "IDirectDrawSurface7", "IDirectSound", "IDirectSound8", "IDirectSoundBuffer", "IDirectSoundBuffer8", "IDirectSoundNotify", "IDirectInput8A", "IDirectInput8W", "IDirect3D9", "IDirect3DDevice9", "IDirect3DResource9", "IDirect3DBaseTexture9", "IDirect3DTexture9", "IDirect3DSurface9", "IDirect3DVertexBuffer9", "IDirect3DIndexBuffer9", "IDirect3DSwapChain9", "IDirect3DStateBlock9", "IDirect3DVertexDeclaration9", "IDirect3DVertexShader9", "IDirect3DPixelShader9", "IDirect3DQuery9", "IGraphBuilder", "IFilterGraph", "IFilterGraph2", "IMediaControl", "IMediaEvent", "IMediaEventEx", "IMediaEventSink", "IMediaSeeking", "IMediaPosition", "IMediaFilter", "IBasicAudio", "IBasicVideo", "IVideoWindow", "IBaseFilter", "IEnumFilters", "IEnumPins", "IPin", "IEnumMediaTypes", "IMemInputPin", "IMemAllocator", "IMediaSample", "IAsyncReader", "IReferenceClock", "IFileSourceFilter" })
        {
            int slot = 0;
            foreach (var mh in ComMethods(r, types, types.Find(name)))
            {
                var m = r.GetMethodDefinition(mh);
                var sig = m.DecodeSignature(types, null);
                com.Add(name + "\t" + slot++ + "\t" + r.GetString(m.Name) + "\t" + sig.ReturnType.Kind + "\t" + sig.ReturnType.Name + "\tthis:" + name + "*:ptr:1" + (sig.ParameterTypes.Length == 0 ? "" : "|" + Parameters(r, m, sig)));
            }
        }
        if (sdkSupplement != null)
        {
            var seen = new HashSet<string>(com.Skip(1).Select(row => string.Join('\t', row.Split('\t').Take(2))));
            var counts = new Dictionary<string, int> { ["IDxDiagProvider"] = 0, ["IDxDiagContainer"] = 0, ["ID3DXEffect"] = 0 };
            foreach (var row in File.ReadLines(Path.Combine(Path.GetDirectoryName(sdkSupplement), "sdk-com-input.tsv"))
                .Where(line => line.Length != 0 && !line.StartsWith('#')))
            {
                var f = row.Split('\t');
                if (f.Length != 6 || !counts.ContainsKey(f[0]) || !int.TryParse(f[1], out int slot)
                    || slot != counts[f[0]] || !new[] { "i32", "u32", "ptr" }.Contains(f[3]))
                    throw new InvalidDataException("Invalid scoped SDK COM signature: " + row);
                foreach (var parameter in f[5].Split('|'))
                {
                    var p = parameter.Split(':');
                    if (p.Length != 4 || !(f[0] == "ID3DXEffect" ? new[] { "ptr", "u32", "i32", "f32", "str8" }.Contains(p[2]) : new[] { "ptr", "u32", "str16" }.Contains(p[2])) || p[3] != "1")
                        throw new InvalidDataException("Invalid SDK COM parameter: " + parameter);
                }
                if (!seen.Add(f[0] + "\t" + f[1]))
                    throw new InvalidDataException("Conflicting SDK COM slot: " + row);
                counts[f[0]]++;
                com.Add(row);
            }
            if (counts["IDxDiagProvider"] != 5 || counts["IDxDiagContainer"] != 9)
                throw new InvalidDataException("Incomplete SDK DxDiag vtable declarations");
            if (counts["ID3DXEffect"] != 79)
                throw new InvalidDataException("Incomplete SDK ID3DXEffect vtable declarations");
        }
        File.WriteAllLines(Path.Combine(output, "com.tsv"), com, new UTF8Encoding(false));
        var constants = new SortedSet<string>(StringComparer.Ordinal);
        foreach (var h in types.Definitions)
        {
            var d = r.GetTypeDefinition(h);
            string typeName = FullName(r, h);
            if (!referenced.Contains(typeName) || FullName(r, d.BaseType) != "System.Enum") continue;
            foreach (var fh in d.GetFields())
            {
                var f = r.GetFieldDefinition(fh);
                var ch = f.GetDefaultValue();
                if (ch.IsNil) continue;
                var c = r.GetConstant(ch);
                var value = r.GetBlobReader(c.Value);
                // Preserve raw bits; the enum's underlying type describes signedness.
                if (value.Length is not (1 or 2 or 4 or 8)) throw new InvalidDataException("enum blob width: " + typeName + "." + r.GetString(f.Name) + ": " + value.Length);
                ulong raw = value.Length switch
                {
                    1 => value.ReadByte(), 2 => value.ReadUInt16(), 4 => value.ReadUInt32(), 8 => value.ReadUInt64(),
                    _ => throw new InvalidDataException("unsupported enum constant " + typeName),
                };
                constants.Add(typeName + "\t" + r.GetString(f.Name) + "\t" + raw.ToString("x") + "\t" + (HasAttribute(r, d.GetCustomAttributes(), "FlagsAttribute") ? "flags" : "enum"));
            }
        }
        File.WriteAllLines(Path.Combine(output, "constants.tsv"), new[] { "# Generated from pinned Win32Metadata: enum type\tconstant\thex bits\tkind" }.Concat(constants), new UTF8Encoding(false));
        Console.WriteLine("Generated " + rows.Count + " x86 exports and " + (com.Count - 1) + " COM slots.");
    }

    static IEnumerable<MethodDefinitionHandle> ComMethods(MetadataReader r, Types types, TypeDefinitionHandle h)
    {
        var d = r.GetTypeDefinition(h);
        foreach (var ih in d.GetInterfaceImplementations())
        {
            var b = r.GetInterfaceImplementation(ih).Interface;
            var baseHandle = b.Kind == HandleKind.TypeDefinition ? (TypeDefinitionHandle)b : types.Find(FullName(r, b).Split('.').Last());
            foreach (var m in ComMethods(r, types, baseHandle)) yield return m;
        }
        foreach (var m in d.GetMethods()) yield return m;
    }
}
