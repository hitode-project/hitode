"""Report how much of each fixed export catalog the registered host APIs cover.

Usage: python tools/survey/coverage.py [--missing] [dll ...]
Catalogs: data/abi/signatures.tsv (documented Win32 functions from the pinned WinMD
and SDK inputs) and data/abi/crt-exports.tsv (CRT export names). Registered APIs
come from docs/host-call-boundary.tsv. A UCRT API-set name counts as covered when
ucrtbase.dll registers it. Data exports are not host calls and are absent from the
table; they are counted apart and never reported as missing code. Without DLL
arguments every catalog module is listed, most missing first; --missing also
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


def covered(have, dll, name):
    dlls = have.get(name, ())
    return dll in dlls or (dll.startswith("api-ms-win-crt-") and "ucrtbase.dll" in dlls)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dlls", nargs="*")
    parser.add_argument("--missing", action="store_true")
    args = parser.parse_args()
    have, exports = where.registered(), catalog()
    selected = [dll.lower() for dll in args.dlls] or list(exports)
    unknown = [dll for dll in selected if dll not in exports]
    if unknown:
        parser.error(f"not in a catalog: {' '.join(unknown)}")
    report = []
    for dll in selected:
        code = [name for name, kind in exports[dll].items() if kind == "code"]
        missing = sorted(name for name in code if not covered(have, dll, name))
        report.append((dll, len(code), len(missing), len(exports[dll]) - len(code), missing))
    report.sort(key=lambda row: (-row[2], row[0]))
    print(f"{'dll':36} {'code':>5} {'have':>5} {'miss':>5} {'data':>5}")
    for dll, code, missing, data, names in report:
        print(f"{dll:36} {code:5} {code - missing:5} {missing:5} {data:5}")
        if args.missing and names:
            print("  " + " ".join(names))
    print(f"{'total':36} {sum(r[1] for r in report):5} {sum(r[1] - r[2] for r in report):5} "
          f"{sum(r[2] for r in report):5} {sum(r[3] for r in report):5}")


if __name__ == "__main__":
    main()
