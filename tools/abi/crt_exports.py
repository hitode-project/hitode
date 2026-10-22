"""Project x86 CRT export names from mingw-w64 module definitions into a fixed catalog.

Usage: python tools/abi/crt_exports.py --mingw <mingw-w64-checkout> [--check]
Input: the public-domain mingw-w64-crt .def files, preprocessed for i386 by `cc -E`.
Output: sorted data/abi/crt-exports.tsv and provenance JSON; stdout contains counts
only. WinMD omits the C runtimes, so this catalog lists which names each CRT DLL
exports and whether an export is data. It records no calling convention, ordinal
or implementation promise. mingw hides some real exports it replaces: in
`alias == export` only the right side is a DLL export, and `; name ; reason`
comments keep the name unless the reason says the i386 DLL lacks it. Explicit
SDK/native kind evidence corrects DATA markers used for locally replaced code.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

MODULES = {
    "msvcrt.dll": "lib-common/msvcrt.def.in",
    "msvcrt20.dll": "lib32/msvcrt20.def.in",
    "msvcrt40.dll": "lib32/msvcrt40.def.in",
    "msvcr70.dll": "lib32/msvcr70.def.in",
    "msvcr71.dll": "lib32/msvcr71.def.in",
    "msvcr80.dll": "lib32/msvcr80.def.in",
    "msvcr90.dll": "lib32/msvcr90.def.in",
    "msvcr100.dll": "lib32/msvcr100.def.in",
    "msvcr110.dll": "lib32/msvcr110.def.in",
    "msvcr120.dll": "lib32/msvcr120.def.in",
    "ucrtbase.dll": "lib-common/ucrtbase.def.in",
    "vcruntime140.dll": "lib-common/vcruntime140.def.in",
    "msvcp60.dll": "lib32/msvcp60.def",
}
API_SETS = "lib-common/api-ms-win-crt-*-l1-1-0.def*"
DECORATION = re.compile(r"@\d+$")
# `; name ; reason` or `; name replaced ...`: an export mingw replaces with its own.
HIDDEN = re.compile(r";\s*([A-Za-z_?$][\w@?$]*)\s*(?:;\s*(.*)|\s+((?:replaced|disabled?)\b.*))$")
# Reasons saying the plain i386 DLL lacks the name: removed exports, ARM64EC-hosted
# CHPE builds and functions mingw supplies because i386 never exported them.
ABSENT = re.compile(r"^removed|chpe|^i386 \S+ provided by emu", re.IGNORECASE)
# An architecture macro that expanded to nothing leaves `; i386 name replaced by emu`
# for a name newer i386 DLLs export and mingw replaces.
ELIDED = re.compile(r";\s*i386 (?:and x64 )?([A-Za-z_][\w]*) replaced by emu\b.*")


def undecorated(name):
    return name if name.startswith("?") else DECORATION.sub("", name)


def exports(text):
    """Export name to kind for preprocessed module-definition text."""
    result, hidden = {}, set()
    for line in text.splitlines():
        line = line.strip()
        elided = ELIDED.fullmatch(line)
        if elided:
            hidden.add(elided.group(1))
            continue
        comment = HIDDEN.fullmatch(line)
        if comment:
            if not ABSENT.search(comment.group(2) or comment.group(3) or ""):
                hidden.add(undecorated(comment.group(1)))
            continue
        line = line.split(";", 1)[0].strip()
        if not line or line.startswith(("LIBRARY", "EXPORTS")):
            continue
        if "==" in line:
            hidden.add(undecorated(line.split("==", 1)[1].split()[0]))
            continue
        words = line.replace("=", " = ").split()
        name = undecorated(words[0])
        kind = "data" if "DATA" in words[1:] else "code"
        if result.get(name, kind) != kind:
            raise ValueError(f"conflicting kinds for {name}")
        result[name] = kind
    for name in hidden:
        result.setdefault(name, "code")
    return result


def kind_overrides(path):
    """Explicit SDK/native kind evidence; import-library DATA can name code."""
    result = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) != 4:
            raise ValueError("invalid CRT kind override")
        module, name, kind, source = fields
        key = module, name
        if not module.endswith(".dll") or not name or kind not in {"code", "data"} or not source or key in result:
            raise ValueError("invalid or duplicate CRT kind override")
        result[key] = kind
    return result


def correct_kinds(module, names, overrides):
    """Correct existing exports only; kind evidence cannot create an export."""
    result = dict(names)
    for (owner, name), kind in overrides.items():
        if owner != module:
            continue
        if name not in result:
            raise ValueError("CRT kind override has no source export")
        result[name] = kind
    return result


def preprocess(crt, path):
    command = ["cc", "-E", "-P", "-undef", "-D__i386__", "-x", "c",
               "-I", str(crt / "def-include"), "-I", str(crt / "lib-common"),
               "-I", str(crt / "lib32"), str(path)]
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


def project(mingw):
    crt = mingw / "mingw-w64-crt"
    modules = dict(MODULES)
    for path in sorted(crt.glob(API_SETS)):
        modules[path.name.split(".def")[0] + ".dll"] = str(path.relative_to(crt))
    evidence = Path(__file__).resolve().parents[2] / "data" / "abi" / "crt-kind-overrides.tsv"
    overrides = kind_overrides(evidence)
    if any(module not in modules for module, _ in overrides):
        raise ValueError("CRT kind override has no source module")
    rows, source = [], {}
    for module, definition in modules.items():
        path = crt / definition
        names = correct_kinds(module, exports(preprocess(crt, path)), overrides)
        source[module] = {"definition": definition, "exports": len(names),
                          "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        rows += [(module, name, kind) for name, kind in names.items()]
    commit = subprocess.run(["git", "-C", str(mingw), "rev-parse", "HEAD"], check=True,
                            capture_output=True, text=True).stdout.strip()
    rows.sort()
    return rows, {"mingw-w64": commit, "license": "public domain (mingw-w64-crt/DISCLAIMER.PD)",
                  "modules": source,
                  "kind_overrides": {"input": "data/abi/crt-kind-overrides.tsv",
                                     "sha256": hashlib.sha256(evidence.read_bytes()).hexdigest(),
                                     "provenance": "data/abi/crt-source.json#export_kind_corrections"}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mingw", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path,
                        default=Path(__file__).resolve().parents[2] / "data" / "abi")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        rows, source = project(args.mingw)
    except (OSError, ValueError, subprocess.CalledProcessError):
        parser.error("CRT module definitions could not be projected")
    header = ("# Generated by tools/abi/crt_exports.py from mingw-w64 module definitions; names only.\n"
              "# module\tname\tkind\n")
    outputs = {
        "crt-exports.tsv": header + "".join("\t".join(row) + "\n" for row in rows),
        "crt-exports-source.json": json.dumps({"format": 1, "source": source}, indent=2, sort_keys=True) + "\n",
    }
    for name, output in outputs.items():
        if args.check:
            if (args.output_dir / name).read_text(encoding="utf-8") != output:
                parser.error("CRT export catalog differs from the selected source")
        else:
            (args.output_dir / name).write_text(output, encoding="utf-8", newline="\n")
    print(f"modules={len(source['modules'])} exports={len(rows)} "
          f"data={sum(row[2] == 'data' for row in rows)}")


if __name__ == "__main__":
    main()
