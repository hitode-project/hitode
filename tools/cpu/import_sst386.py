"""Fetch pinned hardware vectors and convert MOO to JSONL; data stays in target.

Format reference: https://github.com/dbalsom/moo/blob/c438962d2b30856817d8e59bd5f0f4c628596ca8/doc/moo_format_v1.md
The upstream data are public domain. This tool never executes downloaded code.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import urllib.request

COMMIT = "459d49fbe6280e9ed46fee887b58dacd9cb880ab"
REPO = "SingleStepTests/80386"
REGS = "cr0 cr3 eax ebx ecx edx esi edi ebp esp cs ds es fs gs ss eip eflags dr6 dr7".split()
# Register/immediate-only accumulator ALU and all register INC/DEC, 8/16/32-bit.
# Other opcode files can be named explicitly; the runner reports exclusions.
DEFAULT = [f"{base + size:02X}" for base in range(0, 0x40, 8) for size in (4, 5)]
DEFAULT += [f"66{base + 5:02X}" for base in range(0, 0x40, 8)]
DEFAULT += [f"{prefix}{op:02X}" for prefix in ("", "66") for op in range(0x40, 0x50)]
DEFAULT += [f"{prefix}{op}" for prefix in ("", "66") for op in ("98", "99")]
DEFAULT += ["A8", "A9", "9E", "9F", "F5", "F8", "F9", "FC", "FD"]
EXTENDED = [f"{op}.{group}" for op in ("C0", "C1", "66C1", "D2", "D3", "66D3")
            for group in (0, 1, 2, 3, 4, 5, 7)]
EXTENDED += [f"{op}.{group}" for op in ("F6", "F7", "66F7") for group in (2, 3, 4, 5, 6, 7)]
EXTENDED += [prefix + op for op in ("69", "6B", "0FAF", "0FA3", "0FAB", "0FB3", "0FBB",
                                   "0FBC", "0FBD", "0FB6", "0FB7", "0FBE", "0FBF")
             for prefix in ("", "66")]


def chunks(data: bytes):
    """Length-checked chunks; malformed/truncated input is never a partial pass."""
    offset = 0
    while offset < len(data):
        if len(data) - offset < 8:
            raise ValueError("truncated chunk header")
        tag, size = struct.unpack_from("<4sI", data, offset)
        offset += 8
        if size > len(data) - offset:
            raise ValueError(f"truncated {tag!r} chunk")
        yield tag, data[offset:offset + size]
        offset += size


def registers(data: bytes) -> dict[str, int]:
    if len(data) < 4:
        raise ValueError("truncated register mask")
    present, = struct.unpack_from("<I", data)
    if present >> len(REGS) or len(data) != 4 + 4 * present.bit_count():
        raise ValueError("invalid register payload")
    values = iter(struct.unpack(f"<{present.bit_count()}I", data[4:]))
    return {name: next(values) for bit, name in enumerate(REGS) if present & (1 << bit)}


def counted(data: bytes, width: int) -> bytes:
    if len(data) < 4:
        raise ValueError("truncated count")
    size, = struct.unpack_from("<I", data)
    if len(data) != 4 + width * size:
        raise ValueError("invalid counted payload")
    return data[4:]


def state(data: bytes) -> dict:
    result = {"regs": {}, "ram": [], "masks": {}}
    seen = set()
    for tag, payload in chunks(data):
        if tag in seen:
            raise ValueError(f"duplicate state chunk {tag!r}")
        seen.add(tag)
        if tag == b"RG32":
            result["regs"] = registers(payload)
        elif tag == b"RM32":
            result["masks"] = registers(payload)
        elif tag == b"RAM ":
            result["ram"] = list(struct.iter_unpack("<IB", counted(payload, 5)))
        elif tag not in (b"EA32", b"QUEU"):
            raise ValueError(f"unsupported state chunk {tag!r}")
    return result


def parse_moo(data: bytes) -> list[dict]:
    source = list(chunks(data))
    if not source or source[0][0] != b"MOO ":
        raise ValueError("missing MOO header")
    header = source[0][1]
    if len(header) != 12 or header[:2] not in (b"\x01\x00", b"\x01\x01") or header[8:] != b"386E":
        raise ValueError("unsupported format/CPU header")
    count, = struct.unpack_from("<I", header, 4)
    common_masks = {}
    tests = []
    indices = set()
    for tag, payload in source[1:]:
        if tag == b"RM32":
            if tests or common_masks:
                raise ValueError("late/duplicate common masks")
            common_masks = registers(payload)
            continue
        if tag == b"META":
            if len(payload) != 31 or payload[27] != 0:
                raise ValueError("only real-mode source metadata is supported")
            continue
        if tag != b"TEST" or len(payload) < 4:
            raise ValueError(f"unsupported top-level chunk {tag!r}")
        index, = struct.unpack_from("<I", payload)
        if index in indices:
            raise ValueError("duplicate test index")
        indices.add(index)
        test = {"idx": index, "masks": common_masks.copy()}
        seen = set()
        for sub, value in chunks(payload[4:]):
            if sub in seen:
                raise ValueError(f"duplicate test chunk {sub!r}")
            seen.add(sub)
            if sub == b"NAME":
                test["name"] = counted(value, 1).decode("utf-8")
            elif sub == b"BYTS":
                test["bytes"] = list(counted(value, 1))
            elif sub in (b"INIT", b"FINA"):
                test["initial" if sub == b"INIT" else "final"] = state(value)
            elif sub == b"HASH":
                if len(value) != 20:
                    raise ValueError("invalid test identity")
                test["hash"] = value.hex()
            elif sub == b"EXCP":
                if len(value) != 5:
                    raise ValueError("invalid exception payload")
                number, address = struct.unpack("<BI", value)
                test["exception"] = {"number": number, "flag_address": address}
            elif sub == b"GMET":
                # Upstream generator seed/u16 attempt count, not CPU state.
                if len(value) != 10:
                    raise ValueError("unsupported generator metadata")
            elif sub != b"CYCL":
                raise ValueError(f"unsupported test chunk {sub!r}")
        if not {"initial", "final", "bytes", "hash", "name"} <= test.keys():
            raise ValueError("incomplete hardware test")
        tests.append(test)
    if len(tests) != count:
        raise ValueError(f"test count mismatch: {len(tests)} != {count}")
    return tests


def fetch(url: str, path: Path) -> bytes:
    if path.exists():
        return path.read_bytes()
    request = urllib.request.Request(url, headers={"User-Agent": "adv-host-cpu-validation"})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024:
        raise ValueError(f"oversized download: {url}")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".partial")
    temporary.write_bytes(data)
    temporary.replace(path)
    return data


def git_blob(data: bytes) -> str:
    return hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path("target/cpu-reference/sst386"))
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--files", nargs="+", help="opcode names, e.g. 04 6605 D3.4")
    selection.add_argument("--suite", choices=("core", "extended"), default="core")
    args = parser.parse_args()
    files = sorted(set(args.files or (DEFAULT if args.suite == "core" else EXTENDED)))
    if any(not re.fullmatch(r"(?:[0-9A-F]{2})+(?:\.[0-7])?", name) for name in files):
        parser.error("opcode names must be uppercase hexadecimal with optional group suffix")
    args.out.mkdir(parents=True, exist_ok=True)
    cache = args.out / COMMIT
    tree_bytes = fetch(f"https://api.github.com/repos/{REPO}/git/trees/{COMMIT}?recursive=1", cache / "tree.json")
    tree = json.loads(tree_bytes)
    if tree.get("truncated"):
        raise ValueError("truncated source tree")
    entries = {item["path"]: item for item in tree["tree"] if item["type"] == "blob"}
    missing = [name for name in files if f"v1_ex_real_mode/{name}.MOO.gz" not in entries]
    if missing:
        parser.error(f"opcode files absent from pinned source: {', '.join(missing)}")

    def source(path: str) -> bytes:
        entry = entries[path]
        data = fetch(f"https://raw.githubusercontent.com/{REPO}/{COMMIT}/{path}", cache / path)
        if len(data) != entry["size"] or git_blob(data) != entry["sha"]:
            raise ValueError(f"cached source identity mismatch: {path}")
        return data

    revoked = {line.strip() for line in source("revocation_list.txt").decode().splitlines()
               if line.strip() and not line.startswith("#")}
    source("LICENSE")
    manifest = {"source": REPO, "commit": COMMIT, "revoked": sorted(revoked), "files": []}
    output = args.out / "vectors.jsonl"
    temporary = args.out / "vectors.jsonl.partial"
    with temporary.open("w", encoding="utf-8", newline="\n") as stream:
        for name in files:
            path = f"v1_ex_real_mode/{name}.MOO.gz"
            compressed = source(path)
            # Bounded decompression, even if a corrupted local cache is supplied.
            with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as zipped:
                decoded = zipped.read(64 * 1024 * 1024 + 1)
            if len(decoded) > 64 * 1024 * 1024:
                raise ValueError(f"oversized MOO file: {path}")
            tests = parse_moo(decoded)
            for test in tests:
                test.update(source=name, revoked=test["hash"] in revoked)
                stream.write(json.dumps(test, separators=(",", ":")) + "\n")
            manifest["files"].append({"name": name, "sha256": hashlib.sha256(compressed).hexdigest(),
                                      "bytes": len(compressed), "tests": len(tests)})
            print(f"{name}: {len(tests)} hardware vectors", flush=True)
    temporary.replace(output)
    manifest["vectors_sha256"] = hashlib.sha256(output.read_bytes()).hexdigest()
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {output}; source data and converted vectors are not committed.")


if __name__ == "__main__":
    main()
