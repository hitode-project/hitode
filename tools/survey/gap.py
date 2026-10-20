"""Compare the static imports of PE files against the registered host APIs.

Usage: python tools/survey/gap.py <pe-or-dir> [<pe-or-dir> ...]   (needs pefile)
Registered APIs come from docs/host-call-boundary.tsv, per DLL. Output lists,
per folder, the imports no registered DLL provides under that name. Data
exports are not host calls and are absent from the table, so they appear too.
A static import is an estimate of work, not evidence that a game calls the API
(CONTRIBUTING.md, Verifying a change): implement from a real call in a run
report's `missing_apis` (triage_report.py), and use this list to plan.
"""
import collections, pathlib, re, sys

import pefile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import where  # noqa: E402

SKIP_DLL = re.compile(r'(?i)^(pal|ogg|vorbis)')


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
    have = where.registered()
    print(f'registered API names: {len(have)}')
    for arg in sys.argv[1:]:
        files = list(pe_files(arg))
        own = {f.name.lower() for f in files}
        need = set().union(*(imports(f, own) for f in files))
        missing = collections.defaultdict(list)
        for dll, name in sorted(need):
            if dll not in have.get(name, ()):
                missing[dll].append(name)
        total = sum(len(v) for v in missing.values())
        print(f'\n== {pathlib.Path(arg).name}: {len(files)} PE, {len(need)} imports, {total} missing')
        for dll, names in sorted(missing.items(), key=lambda kv: -len(kv[1])):
            print(f'  {dll:14} {len(names):3}: {" ".join(names)}')


if __name__ == '__main__':
    main()
