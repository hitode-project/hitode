"""Print imports, exports and sections of every PE file in one game directory.

Usage: python pe_detail.py <game-dir>
Prints statistics and API names only; nothing is written.
"""
import pathlib, sys

import pefile


def main():
    base = pathlib.Path(sys.argv[1])
    for p in sorted(base.rglob('*')):
        if not p.is_file() or p.stat().st_size < 64:
            continue
        with p.open('rb') as f:
            if f.read(2) != b'MZ':
                continue
        try:
            pe = pefile.PE(str(p), fast_load=True)
            pe.parse_data_directories(directories=[0, 1, 13])
        except pefile.PEFormatError:
            continue
        arch = {0x14c: 'x86', 0x8664: 'x64'}.get(pe.FILE_HEADER.Machine, hex(pe.FILE_HEADER.Machine))
        kind = 'DLL' if pe.FILE_HEADER.Characteristics & 0x2000 else 'EXE'
        linker = f'{pe.OPTIONAL_HEADER.MajorLinkerVersion}.{pe.OPTIONAL_HEADER.MinorLinkerVersion}'
        secs = [s.Name.rstrip(b'\0').decode(errors='replace') for s in pe.sections]
        print(f'== {p.relative_to(base)}  {arch} {kind} {p.stat().st_size >> 10}K linker={linker} sections={secs}')
        for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', []) + getattr(pe, 'DIRECTORY_ENTRY_DELAY_IMPORT', []):
            names = sorted(i.name.decode(errors='replace') if i.name else f'#{i.ordinal}' for i in d.imports)
            print(f'   {d.dll.decode(errors="replace").lower():14} {len(names):3}: {" ".join(names)}')
        exp = getattr(pe, 'DIRECTORY_ENTRY_EXPORT', None)
        if exp:
            names = [s.name.decode(errors='replace') if s.name else f'#{s.ordinal}' for s in exp.symbols]
            print(f'   exports {len(names)}: {" ".join(names[:60])}')


if __name__ == '__main__':
    main()
