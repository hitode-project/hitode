/* Scripted x86 DLL export driver, built by build-dll-driver.ps1.
 * Usage: run fixture_dll_driver.exe in a directory containing dll-driver.json.
 * Input: version-1 JSON, a 32-bit DLL, raw DWORD arguments and buffer inputs.
 * Output: dll-driver.jsonl with loop markers, counts, scalar results, QPC ticks
 * and FNV-1a memory hashes. Pointer addresses and memory bytes are never output.
 * The same EXE and JSON run on native Windows and both adv-run backends.
 * No CRT, Windows headers or engine knowledge are required. */
#include "win32_min.h"

typedef unsigned long long U64;
API HANDLE WINAPI CreateFileW(const WORD *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI ReadFile(HANDLE, void *, DWORD, DWORD *, void *);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API DWORD WINAPI GetFileSize(HANDLE, DWORD *);
API HMODULE WINAPI LoadLibraryW(const WORD *);
API int WINAPI MultiByteToWideChar(UINT, DWORD, const char *, int, WORD *, int);
API void *WINAPI VirtualAlloc(void *, DWORD, DWORD, DWORD);
API BOOL WINAPI QueryPerformanceCounter(U64 *);
API BOOL WINAPI QueryPerformanceFrequency(U64 *);

#define MAX_JSON (1024 * 1024)
#define MAX_TOKENS 16384
#define MAX_BUFFERS 256
#define MAX_CALLS 1024
#define MAX_ARGS 64
#define INVALID_HANDLE 0xffffffffu

typedef struct { int kind, start, end, next, count; } Token;
typedef struct { int id; BYTE *data; DWORD size; } Buffer;
typedef struct { int id; DWORD eax, edx; } Result;
static char source[MAX_JSON + 1];
static Token tokens[MAX_TOKENS];
static int position, token_count, buffer_count, result_count;
static Buffer buffers[MAX_BUFFERS];
static Result results[MAX_CALLS];
static HANDLE output;
static char line[65536];
static DWORD line_size;

static DWORD length(const char *s) { DWORD n = 0; while (s[n]) ++n; return n; }
static void write_bytes(const char *s, DWORD n) {
    DWORD written, i;
    for (i = 0; i < n; ++i) {
        if (line_size == sizeof(line)) ExitProcess(3);
        line[line_size++] = s[i];
        if (s[i] == '\n') {
            if (!WriteFile(output, line, line_size, &written, NULL) || written != line_size) ExitProcess(3);
            line_size = 0;
        }
    }
}
static void emit(const char *s) { write_bytes(s, length(s)); }
static void number(DWORD n) {
    char text[10]; int count = 0;
    do { text[count++] = (char)('0' + n % 10); n /= 10; } while (n);
    while (count) write_bytes(&text[--count], 1);
}
static void hex64(U64 n) {
    char text[16]; int i;
    for (i = 15; i >= 0; --i) { text[i] = "0123456789abcdef"[(DWORD)n & 15]; n >>= 4; }
    write_bytes(text, 16);
}
static void fail(const char *reason) {
    line_size = 0;
    emit("{\"event\":\"error\",\"message\":\""); emit(reason);
    emit("\",\"position\":"); number((DWORD)position); emit("}\n");
    ExitProcess(2);
}
static void require(BOOL ok, const char *reason) { if (!ok) fail(reason); }
static int hex_digit(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}
static void whitespace(void) {
    while (source[position] == ' ' || source[position] == '\t' ||
           source[position] == '\r' || source[position] == '\n') ++position;
}
static int parse(int depth) {
    int t, child, i; char c;
    whitespace(); require(depth < 64 && token_count < MAX_TOKENS, "JSON limit exceeded");
    t = ++token_count; c = source[position];
    tokens[t - 1].start = position; tokens[t - 1].kind = c;
    if (c == '{' || c == '[') {
        ++position; whitespace();
        if (source[position] != (c == '{' ? '}' : ']')) {
            for (;;) {
                if (c == '{') {
                    require(source[position] == '"', "object key must be a string");
                    parse(depth + 1); whitespace();
                    require(source[position++] == ':', "missing colon");
                }
                child = parse(depth + 1); (void)child;
                ++tokens[t - 1].count; whitespace();
                if (source[position] != ',') break;
                ++position; whitespace();
            }
        }
        require(source[position++] == (c == '{' ? '}' : ']'), "missing closing delimiter");
    } else if (c == '"') {
        ++position;
        while ((c = source[position]) != '"') {
            require((BYTE)c >= 32, "invalid string"); ++position;
            if (c == '\\') {
                c = source[position++];
                if (c == 'u') {
                    for (i = 0; i < 4; ++i) require(hex_digit(source[position++]) >= 0, "invalid Unicode escape");
                } else require(c == '"' || c == '\\' || c == '/' || c == 'b' || c == 'f' ||
                               c == 'n' || c == 'r' || c == 't', "invalid escape");
            }
        }
        ++position;
    } else {
        tokens[t - 1].kind = 'n';
        if (c == '-') ++position;
        require(source[position] >= '0' && source[position] <= '9', "expected integer");
        if (source[position] == '0') ++position;
        else while (source[position] >= '0' && source[position] <= '9') ++position;
    }
    tokens[t - 1].end = position; tokens[t - 1].next = token_count + 1;
    return t;
}
static Token *tok(int t) { require(t > 0 && t <= token_count, "missing field"); return &tokens[t - 1]; }
static DWORD unicode_unit(int *p) {
    DWORD n = 0; int i;
    for (i = 0; i < 4; ++i) n = (n << 4) | (DWORD)hex_digit(source[(*p)++]);
    return n;
}
static void string(int t, char *out, DWORD cap) {
    Token *v = tok(t); int p = v->start + 1; DWORD n = 0, u, low; char c;
    require(v->kind == '"', "expected string");
    while (p < v->end - 1) {
        c = source[p++];
        if (c == '\\') {
            c = source[p++];
            if (c == 'u') {
                u = unicode_unit(&p);
                if (u >= 0xd800 && u <= 0xdbff) {
                    require(p + 6 <= v->end - 1 && source[p] == '\\' && source[p + 1] == 'u', "unpaired surrogate");
                    p += 2; low = unicode_unit(&p);
                    require(low >= 0xdc00 && low <= 0xdfff, "unpaired surrogate");
                    u = 0x10000 + ((u - 0xd800) << 10) + low - 0xdc00;
                } else require(u < 0xdc00 || u > 0xdfff, "unpaired surrogate");
                require(u != 0 && n + 4 < cap, "string too long or contains NUL");
                if (u >= 0x10000) out[n++] = (char)(0xf0 | (u >> 18));
                if (u >= 0x800) out[n++] = (char)((u >= 0x10000 ? 0x80 : 0xe0) | ((u >> 12) & 63));
                if (u >= 0x80) out[n++] = (char)((u >= 0x800 ? 0x80 : 0xc0) | ((u >> 6) & 63));
                out[n++] = (char)(u < 0x80 ? u : 0x80 | (u & 63)); continue;
            }
            if (c == 'b') c = '\b'; else if (c == 'f') c = '\f'; else if (c == 'n') c = '\n';
            else if (c == 'r') c = '\r'; else if (c == 't') c = '\t';
        }
        require(n + 1 < cap, "string too long"); out[n++] = c;
    }
    out[n] = 0;
}
static BOOL same(int a, int b) {
    char left[256], right[256]; DWORD i;
    string(a, left, sizeof(left)); string(b, right, sizeof(right));
    for (i = 0; left[i] && left[i] == right[i]; ++i) {}
    return left[i] == right[i];
}
static BOOL is(int t, const char *text) {
    char decoded[256]; DWORD i;
    string(t, decoded, sizeof(decoded));
    for (i = 0; decoded[i] && decoded[i] == text[i]; ++i) {}
    return decoded[i] == text[i];
}
static int field(int object, const char *name) {
    int p, found = 0; Token *o = tok(object);
    require(o->kind == '{', "expected object");
    for (p = object + 1; p < o->next;) {
        int value = p + 1;
        if (is(p, name)) { require(!found, "duplicate field"); found = value; }
        p = tok(value)->next;
    }
    return found;
}
/* Reject misspellings instead of silently running different work. */
static void fields(int object, const char *const *allowed) {
    int p, q, i; Token *o = tok(object);
    require(o->kind == '{', "expected object");
    for (p = object + 1; p < o->next; p = tok(p + 1)->next) {
        for (i = 0; allowed[i] && !is(p, allowed[i]); ++i) {}
        require(allowed[i] != NULL, "unknown field");
        for (q = object + 1; q < p; q = tok(q + 1)->next) require(!same(p, q), "duplicate field");
    }
}
static DWORD integer(int t) {
    Token *v = tok(t); char text[64]; const char *p; DWORD n = 0, base = 10, digit; BOOL negative = 0;
    if (v->kind == '"') string(t, text, sizeof(text));
    else {
        int i, count = v->end - v->start;
        require((DWORD)count < sizeof(text), "integer too long");
        for (i = 0; i < count; ++i) text[i] = source[v->start + i]; text[count] = 0;
    }
    p = text; if (*p == '-') { negative = 1; ++p; }
    if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) { base = 16; p += 2; }
    require(*p != 0, "empty integer");
    while (*p) {
        digit = (DWORD)hex_digit(*p++);
        require(digit < base && n <= (0xffffffffu - digit) / base, "integer outside DWORD");
        n = n * base + digit;
    }
    require(!negative || n <= 0x80000000u, "integer outside signed DWORD");
    return negative ? 0u - n : n;
}
static DWORD option(int object, const char *key, DWORD fallback) {
    int t = field(object, key); return t ? integer(t) : fallback;
}
static void path(int t, WORD *out, int cap) {
    char text[4096]; string(t, text, sizeof(text));
    require(MultiByteToWideChar(65001, 8, text, -1, out, cap) > 0, "invalid UTF-8 path");
}
static Buffer *buffer(int id) {
    int i; for (i = 0; i < buffer_count; ++i) if (same(id, buffers[i].id)) return &buffers[i];
    fail("unknown buffer"); return NULL;
}
static DWORD word(int t) {
    static const char *const keys[] = {"buffer", "result", "read", "offset", "word", NULL};
    Token *v = tok(t); int b, r, rd, i; DWORD offset;
    if (v->kind != '{') return integer(t);
    fields(t, keys); b = field(t, "buffer"); r = field(t, "result"); rd = field(t, "read");
    require(!!b + !!r + !!rd == 1, "word needs one reference"); offset = option(t, "offset", 0);
    require(!field(t, "word") || r, "word selector needs a result");
    if (b || rd) {
        Buffer *buf = buffer(b ? b : rd);
        require(offset <= buf->size && (!rd || buf->size - offset >= 4), "buffer reference outside allocation");
        if (b) return (DWORD)(buf->data + offset);
        return buf->data[offset] | (buf->data[offset + 1] << 8) |
               (buf->data[offset + 2] << 16) | ((DWORD)buf->data[offset + 3] << 24);
    }
    for (i = 0; i < result_count; ++i) if (same(r, results[i].id)) {
        int selector = field(t, "word");
        require(!selector || is(selector, "eax") || is(selector, "edx"), "unknown return word");
        return (selector && is(selector, "edx") ? results[i].edx : results[i].eax) + offset;
    }
    fail("result must reference an earlier call"); return 0;
}
static void initialize(int array) {
    static const char *const keys[] = {"id", "size", "fill", "hex", "words", "file", NULL};
    int p, i; DWORD total = 0;
    if (!array) return;
    require(tok(array)->kind == '[', "buffers must be an array");
    for (p = array + 1; p < tok(array)->next; p = tok(p)->next) {
        Buffer *b; int hex, words, file, w; DWORD j, count, high, read, size, fill; WORD file_path[4096]; HANDLE h;
        fields(p, keys); require(buffer_count < MAX_BUFFERS, "too many buffers");
        b = &buffers[buffer_count]; b->id = field(p, "id");
        for (i = 0; i < buffer_count; ++i) require(!same(b->id, buffers[i].id), "duplicate buffer id");
        /* Decode even the first id, which has no duplicate to compare. */
        { char id[256]; string(b->id, id, sizeof(id)); require(id[0] != 0, "empty buffer id"); }
        b->size = integer(field(p, "size"));
        require(b->size > 0 && b->size <= 64 * 1024 * 1024 && total <= 256 * 1024 * 1024 - b->size, "buffer allocation limit");
        total += b->size;
        b->data = (BYTE *)VirtualAlloc(NULL, b->size, 0x3000, 4); require(b->data != NULL, "allocation failed");
        fill = option(p, "fill", 0); require(fill <= 255, "fill outside byte");
        for (j = 0; j < b->size; ++j) b->data[j] = (BYTE)fill;
        hex = field(p, "hex"); words = field(p, "words"); file = field(p, "file");
        require(!!hex + !!words + !!file <= 1, "choose one buffer initializer");
        if (hex) {
            static char bytes[MAX_JSON + 1]; string(hex, bytes, sizeof(bytes)); count = length(bytes);
            require(!(count & 1) && count / 2 <= b->size, "hex size mismatch");
            for (j = 0; j < count; j += 2) {
                require(hex_digit(bytes[j]) >= 0 && hex_digit(bytes[j + 1]) >= 0, "invalid hex data");
                b->data[j / 2] = (BYTE)((hex_digit(bytes[j]) << 4) | hex_digit(bytes[j + 1]));
            }
        }
        if (words) {
            require(tok(words)->kind == '[' && (DWORD)tok(words)->count <= b->size / 4, "words size mismatch"); j = 0;
            for (w = words + 1; w < tok(words)->next; w = tok(w)->next) {
                DWORD n = integer(w); for (i = 0; i < 4; ++i) { b->data[j++] = (BYTE)n; n >>= 8; }
            }
        }
        if (file) {
            path(file, file_path, 4096); h = CreateFileW(file_path, 0x80000000u, 1, NULL, 3, 0, 0);
            require(h != INVALID_HANDLE, "buffer file open failed"); size = GetFileSize(h, &high);
            require(high == 0 && size <= b->size, "buffer file too large");
            require(ReadFile(h, b->data, size, &read, NULL) && read == size, "buffer file read failed"); CloseHandle(h);
        }
        ++buffer_count;
    }
}
static void patches(int array) {
    static const char *const keys[] = {"buffer", "offset", "words", NULL};
    int p, w, i;
    if (!array) return;
    require(tok(array)->kind == '[', "writes must be an array");
    for (p = array + 1; p < tok(array)->next; p = tok(p)->next) {
        Buffer *b; DWORD offset; int words;
        fields(p, keys); b = buffer(field(p, "buffer")); offset = option(p, "offset", 0); words = field(p, "words");
        require(tok(words)->kind == '[' && offset <= b->size && (DWORD)tok(words)->count <= (b->size - offset) / 4, "write outside buffer");
        for (w = words + 1; w < tok(words)->next; w = tok(w)->next) {
            DWORD n = word(w); for (i = 0; i < 4; ++i) { b->data[offset++] = (BYTE)n; n >>= 8; }
        }
    }
}
/* EBP anchors restoration and return capture independently of stack cleanup. */
__declspec(naked) static DWORD __cdecl invoke(void *entry, const DWORD *args, DWORD count,
                                            DWORD self, DWORD callee_pop, DWORD *result) {
    __asm {
        push ebp
        mov ebp, esp
        push ebx
        push esi
        push edi
        mov esi, [ebp + 12]
        mov edi, [ebp + 16]
    push_loop:
        test edi, edi
        jz call_export
        dec edi
        push dword ptr [esi + edi * 4]
        jmp push_loop
    call_export:
        mov ecx, [ebp + 20]
        xor edx, edx
        call dword ptr [ebp + 8]
        mov esi, [ebp + 28]
        mov [esi], eax
        mov [esi + 4], edx
        lea ebx, [ebp - 12]
        cmp dword ptr [ebp + 24], 0
        jne check_stack
        mov eax, [ebp + 16]
        shl eax, 2
        sub ebx, eax
    check_stack:
        cmp esp, ebx
        setne al
        movzx eax, al
        lea esp, [ebp - 12]
        pop edi
        pop esi
        pop ebx
        pop ebp
        ret
    }
}
static U64 hash(const BYTE *bytes, DWORD size) {
    U64 value = 0xcbf29ce484222325ull; DWORD i;
    for (i = 0; i < size; ++i) value = (value ^ bytes[i]) * 0x100000001b3ull;
    return value;
}
static void quoted(int t) { Token *v = tok(t); write_bytes(source + v->start, (DWORD)(v->end - v->start)); }
static void hash_range(Buffer *b, DWORD offset, DWORD size) {
    require(offset <= b->size && size <= b->size - offset, "hash outside buffer");
    emit("{\"buffer\":"); quoted(b->id); emit(",\"offset\":"); number(offset);
    emit(",\"size\":"); number(size); emit(",\"fnv1a64\":\""); hex64(hash(b->data + offset, size)); emit("\"}");
}
static void hashes(int array) {
    static const char *const keys[] = {"buffer", "offset", "size", NULL};
    int p, i = 0;
    emit("[");
    if (!array) {
        for (i = 0; i < buffer_count; ++i) { if (i) emit(","); hash_range(&buffers[i], 0, buffers[i].size); }
    } else {
        require(tok(array)->kind == '[', "hashes must be an array");
        for (p = array + 1; p < tok(array)->next; p = tok(p)->next) {
            Buffer *b; DWORD offset;
            fields(p, keys); b = buffer(field(p, "buffer")); offset = option(p, "offset", 0);
            require(offset <= b->size, "hash offset outside buffer");
            if (i++) emit(","); hash_range(b, offset, option(p, "size", b->size - offset));
        }
    }
    emit("]");
}
static void calls(HMODULE module, int array) {
    static const char *const keys[] = {"id", "export", "abi", "args", "this", "iterations", "warmup", "return", "writes", "hashes", "expect", NULL};
    int p;
    require(tok(array)->kind == '[', "calls must be an array");
    require(tok(array)->count > 0, "empty calls");
    for (p = array + 1; p < tok(array)->next; p = tok(p)->next) {
        char name[256], id[256]; DWORD args[MAX_ARGS], count = 0, self = 0, callee_pop, n, warm, i, ret[2];
        int abi, a, r, kind, hash_fields; void *entry; U64 begin, end, frequency; Result *saved;
        fields(p, keys); require(result_count < MAX_CALLS, "too many calls");
        saved = &results[result_count]; saved->id = field(p, "id");
        string(saved->id, id, sizeof(id)); require(id[0] != 0, "empty call id");
        for (i = 0; i < (DWORD)result_count; ++i) require(!same(saved->id, results[i].id), "duplicate call id");
        r = field(p, "export");
        if (tok(r)->kind == '"') { string(r, name, sizeof(name)); entry = GetProcAddress(module, name); }
        else { n = integer(r); require(n > 0 && n <= 65535, "ordinal outside WORD"); entry = GetProcAddress(module, (const char *)n); }
        require(entry != NULL, "export lookup failed");
        abi = field(p, "abi"); require(is(abi, "stdcall") || is(abi, "cdecl") || is(abi, "thiscall"), "unsupported ABI");
        callee_pop = !is(abi, "cdecl");
        patches(field(p, "writes"));
        if (is(abi, "thiscall")) self = word(field(p, "this"));
        else require(!field(p, "this"), "this requires thiscall");
        a = field(p, "args");
        if (a) {
            require(tok(a)->kind == '[' && tok(a)->count <= MAX_ARGS, "argument limit exceeded");
            for (r = a + 1; r < tok(a)->next; r = tok(r)->next) args[count++] = word(r);
        }
        kind = field(p, "return");
        require(!kind || is(kind, "u32") || is(kind, "u64") || is(kind, "pointer") || is(kind, "void"), "unsupported return kind");
        n = option(p, "iterations", 1); warm = option(p, "warmup", 0); require(n > 0, "zero iterations");
        hash_fields = field(p, "hashes");
        for (i = 0; i < warm; ++i) require(!invoke(entry, args, count, self, callee_pop, ret), "ABI stack cleanup mismatch");
        require(QueryPerformanceFrequency(&frequency), "QPC frequency failed");
        emit("{\"event\":\"begin\",\"id\":"); quoted(saved->id); emit("}\n");
        require(QueryPerformanceCounter(&begin), "QPC start failed");
        for (i = 0; i < n; ++i) require(!invoke(entry, args, count, self, callee_pop, ret), "ABI stack cleanup mismatch");
        require(QueryPerformanceCounter(&end), "QPC end failed");
        /* Host observer timestamps this marker before serialization or hashing. */
        emit("{\"event\":\"end\",\"id\":"); quoted(saved->id); emit("}\n");
        saved->eax = ret[0]; saved->edx = ret[1]; ++result_count;
        if (field(p, "expect")) require(ret[0] == integer(field(p, "expect")), "unexpected EAX");
        emit("{\"event\":\"result\",\"id\":"); quoted(saved->id);
        emit(",\"iterations\":"); number(n); emit(",\"warmup\":"); number(warm);
        emit(",\"return_kind\":"); if (kind) quoted(kind); else emit("\"u32\"");
        if (kind && is(kind, "pointer")) { emit(",\"is_null\":"); number(ret[0] == 0); }
        else if (!kind || !is(kind, "void")) {
            emit(",\"eax\":"); number(ret[0]);
            if (kind && is(kind, "u64")) { emit(",\"edx\":"); number(ret[1]); }
        }
        emit(",\"qpc_ticks_hex\":\""); hex64(end - begin); emit("\",\"qpc_frequency_hex\":\""); hex64(frequency);
        emit("\",\"hashes\":"); hashes(hash_fields); emit("}\n");
    }
}
void mainCRTStartup(void) {
    static const char *const keys[] = {"version", "dll", "buffers", "calls", NULL};
    HANDLE input; DWORD size, high, read; WORD dll_path[4096]; HMODULE module; int root;
    static const WORD input_path[] = {'d','l','l','-','d','r','i','v','e','r','.','j','s','o','n',0};
    static const WORD output_path[] = {'d','l','l','-','d','r','i','v','e','r','.','j','s','o','n','l',0};
    output = CreateFileW(output_path, 0x40000000u, 3, NULL, 2, 0, 0);
    if (output == INVALID_HANDLE) ExitProcess(3);
    input = CreateFileW(input_path, 0x80000000u, 1, NULL, 3, 0, 0); require(input != INVALID_HANDLE, "script open failed");
    size = GetFileSize(input, &high); require(high == 0 && size <= MAX_JSON, "script size limit exceeded");
    require(ReadFile(input, source, size, &read, NULL) && read == size, "script read failed"); CloseHandle(input);
    require(size > 0, "empty script"); source[size] = 0;
    root = parse(0); whitespace(); require((DWORD)position == size, "trailing JSON data");
    fields(root, keys); require(integer(field(root, "version")) == 1, "unsupported script version");
    initialize(field(root, "buffers")); path(field(root, "dll"), dll_path, 4096);
    module = LoadLibraryW(dll_path); require(module != 0, "DLL load failed");
    calls(module, field(root, "calls")); require(FreeLibrary(module), "DLL unload failed");
    emit("{\"event\":\"complete\",\"calls\":"); number((DWORD)result_count); emit("}\n");
    CloseHandle(output); ExitProcess(0);
}
