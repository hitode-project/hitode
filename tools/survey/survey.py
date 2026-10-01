"""Survey PE files under $ADV_SAMPLES: system imports, odd sections, bundled libraries.

Prints statistics only; nothing is written. Usage:
    ADV_SAMPLES=<dir> python survey.py [--libs-only]
Each first-level directory of ADV_SAMPLES is treated as one game.
"""
import collections, os, pathlib, re, sys

import pefile

SKIP = re.compile(r'(?i)(setup|uninst|unins\d+|config|update|checker|support)')
STD_SECTIONS = {'.text', '.rdata', '.data', '.rsrc', '.reloc', '.idata', '.tls', '.bss', '.pdata',
                '.CRT', '.gfids', '.00cfg', 'CODE', 'DATA', 'BSS', '.edata'}
LIBS = {
    'zlib': rb'(?:de|in)flate \d\.\d[\d.]* Copyright',
    'libpng': rb'libpng version \d',
    'libjpeg': rb'JFIF|Independent JPEG Group',
    'vorbis': rb'Xiph\.Org libVorbis',
    'ogg': rb'OggS',
    'lua': rb'Lua \d\.\d',
    'squirrel': rb'Squirrel \d',
    'freetype': rb'FreeType',
    'msvc-crt': rb'Microsoft Visual C\+\+ Runtime Library',
    'vcl': rb'Borland|Embarcadero',
    'dshow': rb'(?i)IGraphBuilder|quartz\.dll',
}
INJECT = {'CreateRemoteThread', 'WriteProcessMemory', 'VirtualAllocEx', 'NtCreateThreadEx',
          'SetWindowsHookExA', 'SetWindowsHookExW', 'QueueUserAPC'}


def imports(pe):
    out = collections.defaultdict(set)
    for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', []) + getattr(pe, 'DIRECTORY_ENTRY_DELAY_IMPORT', []):
        for i in d.imports:
            out[d.dll.decode(errors='replace').lower()].add(
                i.name.decode(errors='replace') if i.name else f'#{i.ordinal}')
    return out


def show(path, base, libs_only):
    raw = path.read_bytes()
    try:
        pe = pefile.PE(data=raw, fast_load=True)
        pe.parse_data_directories(directories=[1, 13])
    except pefile.PEFormatError:
        return
    libs = [k for k, r in LIBS.items() if re.search(r, raw)]
    rel = path.relative_to(base)
    if libs_only:
        if libs:
            print(f'  {str(rel):48} {", ".join(libs)}')
        return
    imps = imports(pe)
    funcs = {f for v in imps.values() for f in v}
    arch = {0x14c: 'x86', 0x8664: 'x64'}.get(pe.FILE_HEADER.Machine, hex(pe.FILE_HEADER.Machine))
    print(f'  {str(rel):48} {arch} {len(raw) >> 10:7}K funcs={len(funcs)}')
    print('      imports: ' + ', '.join(f'{k}:{len(v)}' for k, v in sorted(imps.items(), key=lambda kv: -len(kv[1]))))
    odd = [s.Name.rstrip(b'\0').decode(errors='replace') for s in pe.sections]
    odd = [s for s in odd if s not in STD_SECTIONS]
    if odd:
        print(f'      sections: {odd}')
    if libs:
        print(f'      libraries: {libs}')
    if funcs & INJECT:
        print(f'      injection APIs: {sorted(funcs & INJECT)}')


def main():
    root = os.environ.get('ADV_SAMPLES')
    if not root:
        sys.exit('set ADV_SAMPLES to the sample directory')
    libs_only = '--libs-only' in sys.argv
    for game in sorted(p for p in pathlib.Path(root).iterdir() if p.is_dir()):
        print(f'== {game.name}')
        files = [p for p in game.rglob('*') if p.is_file() and p.suffix.lower() in ('.exe', '.dll')
                 and not SKIP.search(p.name) and p.stat().st_size < 256 << 20]
        for p in sorted(files):
            show(p, game, libs_only)


if __name__ == '__main__':
    main()
