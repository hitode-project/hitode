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
            bool crtVariadic = f.Length == 6 && new[] { "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "sprintf_s", "_snprintf_s", "swprintf_s", "_snwprintf_s", "_scprintf", "_scwprintf" }.Contains(f[1])
                && f[2] == "variadic" && f[3] == "i32" && f[4] == "int";
            bool crtSprintf = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "sprintf" && f[2] == "variadic" && f[3] == "i32" && f[4] == "int"
                && f[5] == "buffer:char*:str8:1|format:const char*:str8:1";
            bool crtStrtod = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "strtod" && f[2] == "cdecl" && f[3] == "f64" && f[4] == "double"
                && f[5] == "text:const char*:str8:1|end:char**:ptr:1";
            bool crtDifftime = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "_difftime64" && f[2] == "cdecl" && f[3] == "f64" && f[4] == "double"
                && f[5] == "end:__time64_t:u64:2|begin:__time64_t:u64:2";
            bool crtRound = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "floor", "ceil", "fabs" }.Contains(f[1]) && f[2] == "cdecl"
                && f[3] == "f64" && f[4] == "double" && f[5] == "value:double:f64:2";
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "_CIpow", "_CIlog10", "_CIlog" }.Contains(f[1])
                && (f[2] != "cdecl" || f[3] != "void" || f[4] != "void" || f[5].Length != 0))
                throw new InvalidDataException("CRT register-stack x87 intrinsic contract changed: " + row);
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "qsort"
                && (f[2] != "cdecl" || f[3] != "void" || f[4] != "void"
                    || f[5] != "base:void*:ptr:1|number:size_t:u32:1|width:size_t:u32:1|compare:fn*:ptr:1"))
                throw new InvalidDataException("CRT guest comparator sort stack contract changed: " + row);
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "fputc", "putc" }.Contains(f[1])
                && (f[2] != "cdecl" || f[3] != "i32" || f[4] != "int"
                    || f[5] != "character:int:i32:1|stream:FILE*:ptr:1"))
                throw new InvalidDataException("CRT byte stream output stack contract changed: " + row);
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "strpbrk", "strcspn", "strspn" }.Contains(f[1]))
            {
                bool pointer = f[1] == "strpbrk";
                if (f[2] != "cdecl" || f[3] != (pointer ? "ptr" : "u32")
                    || f[4] != (pointer ? "char*" : "size_t")
                    || f[5] != "text:const char*:str8:1|set:const char*:str8:1")
                    throw new InvalidDataException("CRT byte-set scan stack contract changed: " + row);
            }
            bool crtModf = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "modf" && f[2] == "cdecl" && f[3] == "f64" && f[4] == "double"
                && f[5] == "value:double:f64:2|integer:double*:ptr:1";
            bool crtLdexp = f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "ldexp" && f[2] == "cdecl" && f[3] == "f64" && f[4] == "double"
                && f[5] == "value:double:f64:2|exponent:int:i32:1";
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "ldexp" && !crtLdexp)
                throw new InvalidDataException("CRT binary64 scaling stack contract changed: " + row);
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && f[1] == "modf" && !crtModf)
                throw new InvalidDataException("CRT fractional split stack contract changed: " + row);
            if (f.Length == 6 && new[] { "msvcrt.dll", "msvcr80.dll", "msvcr90.dll" }.Contains(f[0])
                && new[] { "_beginthreadex", "_endthreadex" }.Contains(f[1]))
            {
                bool begin = f[1] == "_beginthreadex";
                string parameters = begin
                    ? "security:void*:ptr:1|stackSize:unsigned int:u32:1|startAddress:_beginthreadex_proc_type:ptr:1|argument:void*:ptr:1|initFlag:unsigned int:u32:1|threadId:unsigned int*:ptr:1"
                    : "returnCode:unsigned int:u32:1";
                if (f[2] != "cdecl" || f[3] != (begin ? "u32" : "void")
                    || f[4] != (begin ? "uintptr_t" : "void") || f[5] != parameters)
                    throw new InvalidDataException("CRT extended thread stack contract changed: " + row);
            }
            if (f.Length != 6 || (!new[] { "cdecl", "data" }.Contains(f[2]) && !crtVariadic && !crtSprintf) || (!new[] { "u32", "i32", "u64", "void", "ptr" }.Contains(f[3]) && !crtStrtod && !crtDifftime && !crtRound && !crtModf && !crtLdexp)
                || (f[2] == "data" && (f[3] == "void" || f[5].Length != 0)))
                throw new InvalidDataException("Invalid supplemental x86 signature: " + row);
            foreach (var parameter in f[5].Split('|', StringSplitOptions.RemoveEmptyEntries))
            {
                var p = parameter.Split(':');
                bool crtTime64Parameter = crtDifftime && p.Length == 4
                    && p[1] == "__time64_t" && p[2] == "u64" && p[3] == "2";
                bool crtRoundParameter = (crtRound || crtModf || crtLdexp) && p.Length == 4
                    && p[1] == "double" && p[2] == "f64" && p[3] == "2";
                if (p.Length != 4 || ((!new[] { "u32", "i32", "ptr", "str8", "str16" }.Contains(p[2]) || p[3] != "1") && !crtTime64Parameter && !crtRoundParameter))
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
            bool drawDibOpen = f.Length == 6 && f[0] == "msvfw32.dll" && f[1] == "DrawDibOpen"
                && f[3] == "handle" && f[4] == "Windows.Win32.Foundation.HANDLE" && f[5].Length == 0;
            bool drawDib = f.Length == 6 && f[0] == "msvfw32.dll" && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.BOOL"
                && ((f[1] == "DrawDibClose" && f[5] == "hdd:Windows.Win32.Foundation.HANDLE:handle:1")
                    || (f[1] == "DrawDibDraw" && f[5] == "hdd:Windows.Win32.Foundation.HANDLE:handle:1|hdc:Windows.Win32.Graphics.Gdi.HDC:handle:1|xDst:i32:i32:1|yDst:i32:i32:1|dxDst:i32:i32:1|dyDst:i32:i32:1|lpbi:Windows.Win32.Graphics.Gdi.BITMAPINFOHEADER*:ptr:1|lpBits:void*:ptr:1|xSrc:i32:i32:1|ySrc:i32:i32:1|dxSrc:i32:i32:1|dySrc:i32:i32:1|wFlags:u32:u32:1"));
            bool amErrorText = f.Length == 6 && f[0] == "quartz.dll" && f[1] == "AMGetErrorTextA"
                && f[3] == "u32" && f[4] == "u32"
                && f[5] == "hr:Windows.Win32.Foundation.HRESULT:i32:1|pbuffer:Windows.Win32.Foundation.PSTR:str8:1|MaxLen:u32:u32:1";
            bool d3dx = f.Length == 6 && f[0] == "d3dx9_43.dll"
                && new[] { "D3DXMatrixMultiply", "D3DXMatrixTranspose", "D3DXMatrixScaling", "D3DXMatrixTranslation", "D3DXMatrixLookAtLH", "D3DXVec3Project", "D3DXMatrixRotationYawPitchRoll", "D3DXMatrixRotationY", "D3DXMatrixPerspectiveFovLH", "D3DXVec4Transform", "D3DXVec3TransformCoord", "D3DXVec3Transform", "D3DXVec3CatmullRom" }.Contains(f[1])
                && f[3] == "ptr" && f[4] == (f[1] switch { "D3DXVec3Project" or "D3DXVec3TransformCoord" or "D3DXVec3CatmullRom" => "D3DXVECTOR3*", "D3DXVec4Transform" or "D3DXVec3Transform" => "D3DXVECTOR4*", _ => "D3DXMATRIX*" });
            bool d3dxScalars = d3dx && new[] { "D3DXMatrixScaling", "D3DXMatrixTranslation", "D3DXMatrixRotationYawPitchRoll", "D3DXMatrixRotationY", "D3DXMatrixPerspectiveFovLH", "D3DXVec3CatmullRom" }.Contains(f[1]);
            bool d3dxEffect = f.Length == 6 && f[0] == "d3dx9_43.dll"
                && new[] { "D3DXCreateEffect", "D3DXGetShaderOutputSemantics" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "HRESULT";
            bool d3dxStack = f.Length == 6 && f[0] == "d3dx9_43.dll" && f[1] == "D3DXCreateMatrixStack"
                && f[3] == "i32" && f[4] == "HRESULT"
                && f[5] == "Flags:DWORD:u32:1|ppStack:LPD3DXMATRIXSTACK*:ptr:1";
            bool d3dxTexture = f.Length == 6 && f[0] == "d3dx9_43.dll" && f[3] == "i32" && f[4] == "HRESULT"
                && (f[1] == "D3DXCreateTextureFromFileInMemoryEx"
                    && f[5] == "pDevice:LPDIRECT3DDEVICE9:ptr:1|pSrcData:LPCVOID:ptr:1|SrcDataSize:UINT:u32:1|Width:UINT:u32:1|Height:UINT:u32:1|MipLevels:UINT:u32:1|Usage:DWORD:u32:1|Format:D3DFORMAT:i32:1|Pool:D3DPOOL:i32:1|Filter:DWORD:u32:1|MipFilter:DWORD:u32:1|ColorKey:D3DCOLOR:u32:1|pSrcInfo:D3DXIMAGE_INFO*:ptr:1|pPalette:PALETTEENTRY*:ptr:1|ppTexture:LPDIRECT3DTEXTURE9*:ptr:1"
                    || f[1] == "D3DXGetImageInfoFromFileInMemory"
                    && f[5] == "pSrcData:LPCVOID:ptr:1|SrcDataSize:UINT:u32:1|pSrcInfo:D3DXIMAGE_INFO*:ptr:1");
            // The old RTL export is absent from WinMD. Its BOOLEAN (AL) return
            // and RET 16 are verified by the Microsoft x86 wrapper and SDK
            // import library; the WithStatus documentation supplies its four
            // pointer declarations, without aliasing that NTSTATUS return.
            bool ntPath = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "RtlDosPathNameToNtPathName_U"
                && f[3] == "u32" && f[4] == "Windows.Win32.Foundation.BOOLEAN"
                && f[5] == "DosFileName:Windows.Win32.Foundation.PCWSTR:str16:1|NtFileName:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1|FilePart:Windows.Win32.Foundation.PWSTR*:ptr:1|RelativeName:void*:ptr:1";
            bool ntDirectory = f.Length == 6 && f[0] == "ntdll.dll"
                && (f[1] == "RtlSetCurrentDirectory_U" && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                    && f[5] == "PathName:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1"
                    || f[1] == "RtlGetCurrentDirectory_U" && f[3] == "u32" && f[4] == "u32"
                    && f[5] == "BufferLength:u32:u32:1|Buffer:Windows.Win32.Foundation.PWSTR:str16:1");
            bool ntLoaderLock = f.Length == 6 && f[0] == "ntdll.dll" && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && (f[1] == "LdrLockLoaderLock" && f[5] == "Flags:u32:u32:1|Disposition:u32*:ptr:1|Cookie:void**:ptr:1"
                    || f[1] == "LdrUnlockLoaderLock" && f[5] == "Flags:u32:u32:1|Cookie:void*:ptr:1");
            bool ntStrings = f.Length == 6 && f[0] == "ntdll.dll"
                && (f[1] == "RtlEqualUnicodeString" && f[3] == "u32" && f[4] == "Windows.Win32.Foundation.BOOLEAN"
                    && f[5] == "String1:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1|String2:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1|CaseInSensitive:Windows.Win32.Foundation.BOOLEAN:u32:1"
                    || f[1] == "RtlInitAnsiString" && f[3] == "void" && f[4] == "void"
                    && f[5] == "DestinationString:Windows.Win32.Foundation.ANSI_STRING*:ptr:1|SourceString:Windows.Win32.Foundation.PCSTR:str8:1");
            bool ntProcedure = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "LdrGetProcedureAddress"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "DllHandle:void*:ptr:1|ProcedureName:Windows.Win32.Foundation.ANSI_STRING*:ptr:1|ProcedureNumber:u32:u32:1|ProcedureAddress:void**:ptr:1";
            bool ntLibrary = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "LdrLoadDll"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "DllPath:Windows.Win32.Foundation.PCWSTR:str16:1|DllCharacteristics:u32*:ptr:1|DllName:Windows.Win32.Foundation.UNICODE_STRING*:ptr:1|DllHandle:void**:ptr:1";
            bool ntDelay = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "NtDelayExecution"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "Alertable:Windows.Win32.Foundation.BOOLEAN:u32:1|DelayInterval:LARGE_INTEGER*:ptr:1";
            bool ntThreadContext = f.Length == 6 && f[0] == "ntdll.dll" && new[] { "NtGetContextThread", "NtSetContextThread" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ThreadHandle:Windows.Win32.Foundation.HANDLE:handle:1|ThreadContext:CONTEXT*:ptr:1";
            bool ntAllocation = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "NtAllocateVirtualMemory"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ProcessHandle:Windows.Win32.Foundation.HANDLE:handle:1|BaseAddress:void**:ptr:1|ZeroBits:usize:u32:1|RegionSize:usize*:ptr:1|AllocationType:u32:u32:1|Protect:u32:u32:1";
            bool ntMemoryWrite = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "NtWriteVirtualMemory"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ProcessHandle:Windows.Win32.Foundation.HANDLE:handle:1|BaseAddress:void*:ptr:1|Buffer:void*:ptr:1|NumberOfBytesToWrite:usize:u32:1|NumberOfBytesWritten:usize*:ptr:1";
            bool ntCache = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "NtFlushInstructionCache"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ProcessHandle:Windows.Win32.Foundation.HANDLE:handle:1|BaseAddress:void*:ptr:1|RegionSize:usize:u32:1";
            bool ntRegistryOpen = f.Length == 6 && f[0] == "ntdll.dll"
                && new[] { "NtOpenKey", "ZwOpenKey" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "KeyHandle:Windows.Win32.Foundation.HANDLE*:ptr:1|DesiredAccess:u32:u32:1|ObjectAttributes:OBJECT_ATTRIBUTES*:ptr:1";
            bool internalProcess = f.Length == 6 && f[0] == "kernel32.dll" && f[1] == "CreateProcessInternalW"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.BOOL"
                && f[5] == "hToken:Windows.Win32.Foundation.HANDLE:handle:1|lpApplicationName:Windows.Win32.Foundation.PCWSTR:str16:1|lpCommandLine:Windows.Win32.Foundation.PWSTR:str16:1|lpProcessAttributes:Windows.Win32.Security.SECURITY_ATTRIBUTES*:ptr:1|lpThreadAttributes:Windows.Win32.Security.SECURITY_ATTRIBUTES*:ptr:1|bInheritHandles:Windows.Win32.Foundation.BOOL:i32:1|dwCreationFlags:u32:u32:1|lpEnvironment:void*:ptr:1|lpCurrentDirectory:Windows.Win32.Foundation.PCWSTR:str16:1|lpStartupInfo:STARTUPINFOW*:ptr:1|lpProcessInformation:PROCESS_INFORMATION*:ptr:1|hNewToken:Windows.Win32.Foundation.HANDLE*:ptr:1";
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
            // ntifs.h and the SDK x86 ntdll.lib @24/@16 imports establish only
            // these four VM declarations; no general native-service projection.
            bool ntAllocate = f.Length == 6 && f[0] == "ntdll.dll"
                && f[1] == "ZwAllocateVirtualMemory"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ProcessHandle:Windows.Win32.Foundation.HANDLE:handle:1|BaseAddress:void**:ptr:1|ZeroBits:usize:u32:1|RegionSize:usize*:ptr:1|AllocationType:u32:u32:1|Protect:u32:u32:1";
            bool ntFree = f.Length == 6 && f[0] == "ntdll.dll"
                && new[] { "NtFreeVirtualMemory", "ZwFreeVirtualMemory" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ProcessHandle:Windows.Win32.Foundation.HANDLE:handle:1|BaseAddress:void**:ptr:1|RegionSize:usize*:ptr:1|FreeType:u32:u32:1";
            bool ntContinue = f.Length == 6 && f[0] == "ntdll.dll" && f[1] == "NtContinue"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.NTSTATUS"
                && f[5] == "ContextRecord:CONTEXT*:ptr:1|TestAlert:Windows.Win32.Foundation.BOOLEAN:u32:1";
            // The observed eager initializer requires these exact declarations.
            // SDK import decorations independently establish their x86 widths;
            // unimplemented entries retain strict generated thunks.
            int ntInitializerWords = f.Length == 6 && f[0] == "ntdll.dll" ? f[1] switch {
                "NtQuerySection" or "NtQueryKey" or "NtQueryObject" or "NtReadVirtualMemory" or "NtQueryInformationFile" => 5,
                "RtlQueryEnvironmentVariable_U" => 3,
                "NtCreateKey" or "NtDuplicateObject" => 7,
                "NtEnumerateKey" or "NtEnumerateValueKey" or "NtQueryVirtualMemory" => 6,
                "NtSetInformationProcess" or "CsrClientCallServer" => 4,
                "NtResumeThread" or "NtQueryFullAttributesFile" or "NtQueryAttributesFile" => 2,
                "RtlAcquirePebLock" or "RtlReleasePebLock" => 0,
                "NtCreateFile" or "NtQueryDirectoryFile" => 11,
                "NtReadFile" => 9,
                _ => -1
            } : -1;
            bool ntInitializer = ntInitializerWords >= 0
                && f[3] == (ntInitializerWords == 0 ? "void" : "i32")
                && f[4] == (ntInitializerWords == 0 ? "void" : "Windows.Win32.Foundation.NTSTATUS")
                && f[5].Split('|', StringSplitOptions.RemoveEmptyEntries).Length == ntInitializerWords;
            bool directInput = f.Length == 6 && f[0] == "dinput.dll"
                && new[] { "DirectInputCreateA", "DirectInputCreateW" }.Contains(f[1])
                && f[3] == "i32" && f[4] == "HRESULT" && f[5].Split('|').Length == 4;
            // Legacy desktop declaration plus the SDK's _EnableEUDC@4 import.
            bool eudc = f.Length == 6 && f[0] == "gdi32.dll" && f[1] == "EnableEUDC"
                && f[3] == "i32" && f[4] == "Windows.Win32.Foundation.BOOL"
                && f[5] == "fEnableEUDC:Windows.Win32.Foundation.BOOL:i32:1";
            if ((!vcm && !drawDibOpen && !drawDib && !amErrorText && !d3dx && !d3dxEffect && !d3dxStack && !d3dxTexture && !ntPath && !ntDirectory && !ntLoaderLock && !ntStrings && !ntProcedure && !ntLibrary && !ntDelay && !internalProcess && !ntRegistryOpen && !ntCache && !ntMemoryWrite && !ntAllocation && !ntThreadContext && !ntCreate && !ntQuery && !ntSet && !ntRead && !ntClose && !ntAllocate && !ntFree && !ntContinue && !ntInitializer && !directInput && !eudc) || f[2] != "stdcall")
                throw new InvalidDataException("Invalid scoped SDK x86 signature: " + row);
            if (d3dx) {
                int expected = f[1] switch { "D3DXMatrixMultiply" or "D3DXVec4Transform" or "D3DXVec3TransformCoord" or "D3DXVec3Transform" => 3, "D3DXMatrixTranspose" or "D3DXMatrixRotationY" => 2, "D3DXMatrixScaling" or "D3DXMatrixTranslation" or "D3DXMatrixRotationYawPitchRoll" => 4, "D3DXMatrixPerspectiveFovLH" => 5, "D3DXMatrixLookAtLH" => 4, "D3DXVec3Project" or "D3DXVec3CatmullRom" => 6, _ => 0 };
                if (f[5].Split('|', StringSplitOptions.RemoveEmptyEntries).Length != expected)
                    throw new InvalidDataException("D3DX x86 stack contract changed: " + row);
                string scalarDeclaration = f[1] switch {
                    "D3DXMatrixScaling" => "pOut:D3DXMATRIX*:ptr:1|sx:FLOAT:f32:1|sy:FLOAT:f32:1|sz:FLOAT:f32:1",
                    "D3DXMatrixTranslation" => "pOut:D3DXMATRIX*:ptr:1|x:FLOAT:f32:1|y:FLOAT:f32:1|z:FLOAT:f32:1",
                    "D3DXMatrixRotationYawPitchRoll" => "pOut:D3DXMATRIX*:ptr:1|Yaw:FLOAT:f32:1|Pitch:FLOAT:f32:1|Roll:FLOAT:f32:1",
                    "D3DXMatrixRotationY" => "pOut:D3DXMATRIX*:ptr:1|Angle:FLOAT:f32:1",
                    "D3DXMatrixPerspectiveFovLH" => "pOut:D3DXMATRIX*:ptr:1|fovy:FLOAT:f32:1|Aspect:FLOAT:f32:1|zn:FLOAT:f32:1|zf:FLOAT:f32:1",
                    "D3DXVec3CatmullRom" => "pOut:D3DXVECTOR3*:ptr:1|pV0:const D3DXVECTOR3*:ptr:1|pV1:const D3DXVECTOR3*:ptr:1|pV2:const D3DXVECTOR3*:ptr:1|pV3:const D3DXVECTOR3*:ptr:1|s:FLOAT:f32:1",
                    _ => null
                };
                if (d3dxScalars && f[5] != scalarDeclaration)
                    throw new InvalidDataException("D3DX scalar matrix declaration changed: " + row);
                if (f[1] == "D3DXVec4Transform" && f[5] != "pOut:D3DXVECTOR4*:ptr:1|pV:const D3DXVECTOR4*:ptr:1|pM:const D3DXMATRIX*:ptr:1")
                    throw new InvalidDataException("D3DX vector4 declaration changed: " + row);
                if (f[1] == "D3DXVec3TransformCoord" && f[5] != "pOut:D3DXVECTOR3*:ptr:1|pV:const D3DXVECTOR3*:ptr:1|pM:const D3DXMATRIX*:ptr:1"
                    || f[1] == "D3DXVec3Transform" && f[5] != "pOut:D3DXVECTOR4*:ptr:1|pV:const D3DXVECTOR3*:ptr:1|pM:const D3DXMATRIX*:ptr:1")
                    throw new InvalidDataException("D3DX vector3 declaration changed: " + row);
            }
            if (d3dxEffect && f[5].Split('|').Length != (f[1] == "D3DXCreateEffect" ? 9 : 3))
                throw new InvalidDataException("D3DX Effect x86 stack contract changed: " + row);
            foreach (var parameter in f[5].Split('|', StringSplitOptions.RemoveEmptyEntries))
            {
                var p = parameter.Split(':');
                if (p.Length != 4 || !(drawDib ? new[] { "handle", "ptr", "u32", "i32" }.Contains(p[2]) : ntCache ? new[] { "handle", "ptr", "u32" }.Contains(p[2]) : ntMemoryWrite ? new[] { "handle", "ptr", "u32" }.Contains(p[2]) : ntAllocation ? new[] { "handle", "ptr", "u32" }.Contains(p[2]) : ntThreadContext ? new[] { "handle", "ptr" }.Contains(p[2]) : internalProcess ? new[] { "ptr", "handle", "u32", "i32", "str16" }.Contains(p[2]) : ntDelay ? new[] { "ptr", "u32" }.Contains(p[2]) : ntLibrary ? new[] { "ptr", "str16" }.Contains(p[2]) : ntProcedure ? new[] { "ptr", "u32" }.Contains(p[2]) : ntStrings ? new[] { "ptr", "u32", "str8" }.Contains(p[2]) : ntLoaderLock || ntRegistryOpen ? new[] { "ptr", "u32" }.Contains(p[2]) : ntDirectory ? new[] { "ptr", "str16", "u32" }.Contains(p[2]) : ntPath ? new[] { "ptr", "str16" }.Contains(p[2]) : ntCreate ? new[] { "ptr", "u32" }.Contains(p[2]) : ntQuery || ntSet || ntInitializer ? new[] { "handle", "ptr", "u32", "i32" }.Contains(p[2]) : ntRead || ntAllocate || ntFree || directInput ? new[] { "handle", "ptr", "u32" }.Contains(p[2]) : ntContinue ? new[] { "ptr", "u32" }.Contains(p[2]) : ntClose ? p[2] == "handle" : d3dx ? (p[2] == "ptr" || d3dxScalars && p[2] == "f32") : d3dxEffect || d3dxStack ? new[] { "ptr", "u32" }.Contains(p[2]) : d3dxTexture ? new[] { "ptr", "u32", "i32" }.Contains(p[2]) : new[] { "u32", "i32", "str8" }.Contains(p[2])) || p[3] != "1")
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
        foreach (string name in new[] { "IDirectDraw", "IDirectDraw7", "IDirectDrawClipper", "IDirectDrawSurface", "IDirectDrawSurface7", "IDirectSound", "IDirectSound8", "IDirectSoundBuffer", "IDirectSoundBuffer8", "IDirectSoundNotify", "IDirectInput8A", "IDirectInput8W", "IDirect3D9", "IDirect3DDevice9", "IDirect3DResource9", "IDirect3DBaseTexture9", "IDirect3DTexture9", "IDirect3DSurface9", "IDirect3DVertexBuffer9", "IDirect3DIndexBuffer9", "IDirect3DSwapChain9", "IDirect3DStateBlock9", "IDirect3DVertexDeclaration9", "IDirect3DVertexShader9", "IDirect3DPixelShader9", "IDirect3DQuery9", "IGraphBuilder", "IFilterGraph", "IFilterGraph2", "IMediaControl", "IMediaEvent", "IMediaEventEx", "IMediaEventSink", "IMediaSeeking", "IMediaPosition", "IMediaFilter", "IBasicAudio", "IBasicVideo", "IVideoWindow", "IBaseFilter", "IEnumFilters", "IEnumPins", "IPin", "IEnumMediaTypes", "IMemInputPin", "IMemAllocator", "IMediaSample", "IAsyncReader", "IReferenceClock", "IFileSourceFilter", "ITfThreadMgr", "ITfDocumentMgr", "ITfContext" })
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
            var counts = new Dictionary<string, int> { ["IDxDiagProvider"] = 0, ["IDxDiagContainer"] = 0, ["ID3DXEffect"] = 0, ["ID3DXMatrixStack"] = 0, ["IDirectInputA"] = 0, ["IDirectInputW"] = 0, ["IDirectInputDeviceA"] = 0, ["IDirectInputDeviceW"] = 0 };
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
                    if (p.Length != 4 || !(f[0] == "ID3DXEffect" ? new[] { "ptr", "u32", "i32", "f32", "str8" }.Contains(p[2]) : f[0] == "ID3DXMatrixStack" ? new[] { "ptr", "f32" }.Contains(p[2]) : f[0].StartsWith("IDirectInput", StringComparison.Ordinal) ? new[] { "ptr", "u32", "handle" }.Contains(p[2]) : new[] { "ptr", "u32", "str16" }.Contains(p[2])) || p[3] != "1")
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
            if (counts["ID3DXMatrixStack"] != 18)
                throw new InvalidDataException("Incomplete SDK ID3DXMatrixStack vtable declarations");
            if (counts["IDirectInputA"] != 8 || counts["IDirectInputW"] != 8
                || counts["IDirectInputDeviceA"] != 18 || counts["IDirectInputDeviceW"] != 18)
                throw new InvalidDataException("Incomplete SDK legacy DirectInput vtable declarations");
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
