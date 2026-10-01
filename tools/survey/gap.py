"""Compare the system imports of PE files against the APIs pal-win32 registers.

Usage: python gap.py <pal-win32-src> <pe-or-dir> [<pe-or-dir> ...]
Registered names are collected from `"Name", <argc>` patterns, which is how pal-win32
registers handlers; treat the result as approximate.
"""
import collections, pathlib, re, sys

import pefile

SKIP_DLL = re.compile(r'(?i)^(pal|ogg|vorbis)')


def registered(src):
    names = set()
    for p in pathlib.Path(src).rglob('*.rs'):
        names |= set(re.findall(r'"([A-Za-z_][A-Za-z0-9_]*)",\s*\d+\s*,', p.read_text(encoding='utf-8')))
    return names


def pe_files(arg):
    p = pathlib.Path(arg)
    files = [p] if p.is_file() else [f for f in p.rglob('*') if f.is_file()]
    for f in files:
        with f.open('rb') as h:
            if h.read(2) == b'MZ':
                yield f


def imports(path, own):
    pe = pefile.PE(str(path), fast_load=True)
    pe.parse_data_directories(directories=[1, 13])
    out = set()
    for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', []) + getattr(pe, 'DIRECTORY_ENTRY_DELAY_IMPORT', []):
        dll = d.dll.decode(errors='replace').lower()
        if SKIP_DLL.match(dll) or dll in own:
            continue
        for i in d.imports:
            out.add((dll, i.name.decode(errors='replace') if i.name else f'#{i.ordinal}'))
    return out


def main():
    have = registered(sys.argv[1])
    print(f'pal-win32 registered (approx): {len(have)}')
    for arg in sys.argv[2:]:
        files = list(pe_files(arg))
        own = {f.name.lower() for f in files}
        need = set().union(*(imports(f, own) for f in files))
        missing = collections.defaultdict(list)
        for dll, name in sorted(need):
            if name not in have:
                missing[dll].append(name)
        total = sum(len(v) for v in missing.values())
        print(f'\n== {pathlib.Path(arg).name}: {len(files)} PE, {len(need)} imports, {total} missing')
        for dll, names in sorted(missing.items(), key=lambda kv: -len(kv[1])):
            print(f'  {dll:14} {len(names):3}: {" ".join(names)}')


if __name__ == '__main__':
    main()
