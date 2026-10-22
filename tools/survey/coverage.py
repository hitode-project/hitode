"""Report how much of each fixed export catalog the registered host APIs cover.

Usage: python tools/survey/coverage.py [--missing] [dll ...]
Catalogs: data/abi/signatures.tsv (documented Win32 functions from the pinned WinMD
and SDK inputs) and data/abi/crt-exports.tsv (CRT export names). Registered APIs
come from docs/host-call-boundary.tsv. A UCRT API-set name counts as covered when
ucrtbase.dll registers it. Data exports are not host calls and are absent from the
table; they are counted apart and never reported as missing code. Functions
data/abi/post-win7.tsv lists were introduced after Windows 7, stay unimplemented
so discovery hides them, and are counted as excluded. --closure replaces the
catalogs with data/abi/closure.tsv, the functions sample games import or look up.
Without DLL arguments every module is listed, most missing first; --missing also
prints the missing names.
"""
import argparse
import collections
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import where  # noqa: E402

ABI = where.ROOT / "data" / "abi"


def rows(path):
    for line in path.read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            yield line.split("\t")


def catalog():
    """DLL to {name: kind} over both catalogs."""
    result = collections.defaultdict(dict)
    for dll, name, *_ in rows(ABI / "signatures.tsv"):
        result[dll.lower()][name] = "code"
    for dll, name, kind in rows(ABI / "crt-exports.tsv"):
        result[dll][name] = kind
    return result


def post_win7():
    path = ABI / "post-win7.tsv"
    return {(dll.lower(), name) for dll, name, *_ in rows(path)} if path.exists() else set()


def covered(have, dll, name):
    dlls = have.get(name, ())
    return dll in dlls or (dll.startswith("api-ms-win-crt-") and "ucrtbase.dll" in dlls)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dlls", nargs="*")
    parser.add_argument("--missing", action="store_true")
    parser.add_argument("--closure", action="store_true")
    args = parser.parse_args()
    have, exports, later = where.registered(), catalog(), post_win7()
    if args.closure:
        sampled = collections.defaultdict(dict)
        for dll, name, _ in rows(ABI / "closure.tsv"):
            sampled[dll][name] = exports.get(dll, {}).get(name, "code")
        exports = sampled
    selected = [dll.lower() for dll in args.dlls] or list(exports)
    unknown = [dll for dll in selected if dll not in exports]
    if unknown:
        parser.error(f"not in a catalog: {' '.join(unknown)}")
    report = []
    for dll in selected:
        code = [name for name, kind in exports[dll].items() if kind == "code"]
        excluded = sum((dll, name) in later for name in code)
        missing = sorted(name for name in code
                         if (dll, name) not in later and not covered(have, dll, name))
        report.append((dll, len(code), excluded, len(missing),
                       len(exports[dll]) - len(code), missing))
    report.sort(key=lambda row: (-row[3], row[0]))
    print(f"{'dll':36} {'code':>5} {'excl':>5} {'have':>5} {'miss':>5} {'data':>5}")
    for dll, code, excluded, missing, data, names in report:
        print(f"{dll:36} {code:5} {excluded:5} {code - excluded - missing:5} {missing:5} {data:5}")
        if args.missing and names:
            print("  " + " ".join(names))
    total = [sum(r[i] for r in report) for i in range(1, 5)]
    print(f"{'total':36} {total[0]:5} {total[1]:5} {total[0] - total[1] - total[2]:5} "
          f"{total[2]:5} {total[3]:5}")


if __name__ == "__main__":
    main()
