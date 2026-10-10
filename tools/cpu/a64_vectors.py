"""Regenerate or check the AArch64 encoder golden vectors with NDK clang.

The vector file holds one `<hex word>\t<assembly>` line per host instruction.
Encoder tests in adv-cpu-a64 look each expected word up by its assembly text,
so a word is never typed by hand. These are host instruction encodings, not
guest code.

  python tools/cpu/a64_vectors.py                  # rewrite the words from clang
  python tools/cpu/a64_vectors.py --check          # fail if any word is stale
  python tools/cpu/a64_vectors.py --add "nop" ...  # append new lines, then rewrite

clang and llvm-objcopy come from $ANDROID_NDK_HOME (or $ANDROID_NDK_ROOT).
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
VECTORS = ROOT / "crates/adv-cpu-a64/testdata/encode.tsv"
TARGET = "aarch64-linux-android"


def parse(text: str) -> list[tuple[str, str]]:
    """(word, assembly) pairs; blank lines and `#` comments are dropped."""
    rows = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        word, sep, asm = line.partition("\t")
        if not sep or not asm.strip():
            raise ValueError(f"line {number}: expected '<word>\\t<assembly>'")
        rows.append((word.strip(), asm.strip()))
    return rows


def render(rows: list[tuple[str, str]]) -> str:
    return "".join(f"{word}\t{asm}\n" for word, asm in rows)


def toolchain() -> tuple[Path, Path]:
    ndk = os.environ.get("ANDROID_NDK_HOME") or os.environ.get("ANDROID_NDK_ROOT")
    if not ndk:
        sys.exit("ANDROID_NDK_HOME is not set")
    bins = sorted((Path(ndk) / "toolchains/llvm/prebuilt").glob("*/bin"))
    if not bins:
        sys.exit(f"no LLVM toolchain under {ndk}")
    suffix = ".exe" if os.name == "nt" else ""
    return bins[0] / f"clang{suffix}", bins[0] / f"llvm-objcopy{suffix}"


def assemble(lines: list[str]) -> list[str]:
    """Encode each line separately; one line must yield exactly one word."""
    clang, objcopy = toolchain()
    with tempfile.TemporaryDirectory() as tmp:
        source = Path(tmp) / "v.s"
        obj, raw = source.with_suffix(".o"), source.with_suffix(".bin")
        source.write_text("".join(f"{line}\n" for line in lines), encoding="utf-8")
        subprocess.run([str(clang), f"--target={TARGET}", "-c", str(source), "-o", str(obj)],
                       check=True)
        subprocess.run([str(objcopy), "-O", "binary", "--only-section=.text", str(obj), str(raw)],
                       check=True)
        data = raw.read_bytes()
    if len(data) != 4 * len(lines):
        sys.exit(f"{len(lines)} lines produced {len(data)} bytes; each must be one instruction")
    return [f"{int.from_bytes(data[i:i + 4], 'little'):08x}" for i in range(0, len(data), 4)]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail on stale words, write nothing")
    parser.add_argument("--add", nargs="+", default=[], metavar="ASM", help="append lines")
    parser.add_argument("--file", type=Path, default=VECTORS)
    args = parser.parse_args(argv)

    rows = parse(args.file.read_text(encoding="utf-8")) if args.file.exists() else []
    known = {asm for _, asm in rows}
    rows += [("", asm) for asm in args.add if asm not in known]
    asms = [asm for _, asm in rows]
    if len(set(asms)) != len(asms):
        sys.exit("duplicate assembly lines")
    words = assemble(asms)
    fresh = list(zip(words, asms))
    if args.check:
        stale = [f"{asm}: {old} != {new}" for (old, asm), (new, _) in zip(rows, fresh) if old != new]
        for line in stale:
            print(line)
        return 1 if stale else 0
    args.file.parent.mkdir(parents=True, exist_ok=True)
    args.file.write_text(render(fresh), encoding="utf-8", newline="\n")
    print(f"{len(fresh)} vectors -> {args.file}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
