#!/usr/bin/env python3
"""Compare observed native PC samples with a PE static discovery closure.

Usage: static_coverage.py RUN --report CHECKPOINT --game-relative GAME [--output JSON]
Inputs: pc-samples.tsv and ADV_JIT_MAP_DIR maps from pc_sample.py in RUN, a local
runner checkpoint with loaded_modules, PE files under ADV_SAMPLES, llvm-objdump.
Entry, TLS, all exports and relocated executable pointers seed direct call/branch
closure; contiguous relocated pointer arrays include jump-table candidates.
Output: aggregate counts only, split by primary EXE/game DLL/host/non-image.
No PCs, disassembly, code bytes or paths are exported. This is sampled native-PC
coverage, not all-tier executed-instruction coverage. Original file bytes are
not verified against runtime-modified code; misses do not prove indirect calls.
"""
from __future__ import annotations

import argparse
from collections import Counter, deque
import json
import os
from pathlib import Path
import re
import sys

from attribute_pc import JitMap, read_samples

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "survey"))
from module_profile import Module, instructions


def executable(module: Module, rva: int) -> bool:
    return any(s["flags"] & 0x20000000 and s["rva"] <= rva < s["rva"] + s["raw_size"]
               for s in module.sections)


def seeds(module: Module) -> tuple[set[int], dict]:
    roots, counts = set(), Counter()

    def add(rva: int, source: str) -> None:
        if executable(module, rva):
            roots.add(module.image_base + rva)
            counts[source] += 1

    add(module.entry, "entry")
    tls, _ = module.directories[9]
    if tls:
        pointer = module.word(tls + 12)
        callbacks = pointer - module.image_base
        for index in range(4096 if pointer else 0):
            value = module.word(callbacks + index * 4)
            if not value:
                break
            add(value - module.image_base, "tls")
    exports, export_size = module.directories[0]
    if exports:
        table = module.word(exports + 28)
        for index in range(min(module.word(exports + 20), 65536)):
            value = module.word(table + index * 4)
            if value and not exports <= value < exports + export_size:
                add(value, "export")
    reloc, size = module.directories[5]
    end, locations = reloc + size, []
    while reloc and reloc + 8 <= end:
        page, length = module.word(reloc), module.word(reloc + 4)
        if length < 8 or length % 2 or reloc + length > end:
            raise ValueError("invalid relocation block")
        for offset in range(8, length, 2):
            item = module.word(reloc + offset, 2)
            if item >> 12 != 3:
                continue
            location = page + (item & 4095)
            target = module.word(location) - module.image_base
            if executable(module, target):
                add(target, "relocated_pointer")
                locations.append(location)
        reloc += length
    locations = sorted(set(locations))
    counts["adjacent_relocated_pointer_pairs"] = sum(b == a + 4 for a, b in zip(locations, locations[1:]))
    return roots, dict(counts)


def closure(rows: list[tuple[int, str, str]], roots: set[int]) -> set[int]:
    """Linear disassembly supplies boundaries; unreachable padding is not swept."""
    rows = sorted(rows)
    positions = {row[0]: i for i, row in enumerate(rows)}
    pending, seen = deque(roots), set()
    while pending:
        pc = pending.popleft()
        index = positions.get(pc)
        if pc in seen or index is None:
            continue
        address, mnemonic, operands = rows[index]
        seen.add(address)
        target = re.match(r"^(?:0x)?([0-9a-fA-F]+)(?:\s|$)", operands)
        target = int(target[1], 16) if target else None
        mnemonic = mnemonic.split()[-1]
        transfer = mnemonic.startswith("j") or mnemonic.startswith("loop") or mnemonic == "call"
        if transfer and target in positions:
            pending.append(target)
        terminal = mnemonic in ("jmp", "ljmp", "ud2", "hlt", "int3") or mnemonic.startswith(("ret", "iret"))
        if not terminal and index + 1 < len(rows) and 0 < rows[index + 1][0] - address <= 15:
            pending.append(rows[index + 1][0])
    return seen


def aggregate(observed: list[dict], images: list[dict]) -> dict:
    groups = {key: Counter() for key in ("primary", "dll", "host_image", "non_image")}
    unique, covered_unique = {key: set() for key in groups}, {key: set() for key in groups}
    for sample in observed:
        pc = sample.get("pc", 0)
        if not pc:
            continue
        image = next((i for i in images if i["base"] <= pc < i["base"] + i["size"]), None)
        group = image["group"] if image else "non_image"
        groups[group]["samples"] += 1
        unique[group].add(pc)
        if image and "closure" in image:
            groups[group]["static_evaluable_samples"] += 1
        if image and pc - image["base"] + image["preferred_base"] in image.get("closure", set()):
            groups[group]["covered_samples"] += 1
            covered_unique[group].add(pc)
    result = {}
    for group, counts in groups.items():
        total, covered, evaluable = counts["samples"], counts["covered_samples"], counts["static_evaluable_samples"]
        result[group] = dict(samples=total, covered_samples=covered,
            static_evaluable_samples=evaluable,
            sampled_coverage_percent=100 * covered / evaluable if evaluable and group in ("primary", "dll") else None,
            unique_sampled_pcs=len(unique[group]), covered_unique_sampled_pcs=len(covered_unique[group]))
    return result


def locate_image(game: Path, exe: str, name: str, primary: bool) -> Path | None:
    """Prefer the known executable directory over duplicate backup filenames."""
    executable_path = Path(exe)
    candidates = ([executable_path] if primary else [executable_path.parent / name]) + [game / name]
    for path in candidates:
        if path.is_file() and path.resolve().is_relative_to(game):
            return path
    matches = [p for p in game.rglob("*") if p.name.lower() == name.lower() and p.is_file()
               and p.resolve().is_relative_to(game)]
    return matches[0] if len(matches) == 1 else None


def run(directory: Path, report_path: Path, game_relative: str, maps: Path | None = None) -> dict:
    samples = Path(os.environ["ADV_SAMPLES"]).resolve(strict=True)
    game = (samples / game_relative).resolve(strict=True)
    if not game.is_relative_to(samples):
        raise ValueError("game escapes ADV_SAMPLES")
    report = json.loads(report_path.read_text(encoding="utf-8-sig"))
    images, missing, seed_counts, discovered = [], 0, Counter(), Counter()
    for row in report["loaded_modules"]:
        base = int(row["base"], 0)
        image = dict(base=base, size=row["size"], preferred_base=base, group="host_image")
        if row["kind"] == "Image":
            group = "primary" if row["name"].lower() == Path(report["exe"]).name.lower() else "dll"
            image["group"] = group
            path = locate_image(game, report["exe"], row["name"], group == "primary")
            if path is None:
                missing += 1
            else:
                module = Module(path.read_bytes())
                if module.wide:
                    raise ValueError("only x86 PE images are supported")
                roots, counts = seeds(module)
                image.update(preferred_base=module.image_base, closure=closure(instructions(path), roots))
                seed_counts.update(counts)
                discovered[group] += len(image["closure"])
        images.append(image)
    mapping = JitMap(maps or directory)
    observed = [mapping.lookup(s["rip"], s["unix_ns"]) or {} for s in read_samples(directory / "pc-samples.tsv")]
    return {"schema": "adv-static-sampled-coverage-v1", "metric": "tagged_native_wall_clock_PC_samples",
            "runtime_bytes_verified": False, "all_tier_PC_coverage": "unmeasured",
            "sample_count": len(observed), "tagged_samples": sum(bool(s.get("pc")) for s in observed),
            "missing_image_files": missing, "static_discovered_instructions": dict(discovered),
            "static_seed_counts": dict(seed_counts), "groups": aggregate(observed, images),
            "miss_categories": "uncovered_image_PC_or_non_image_PC_cause_unresolved"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--report", required=True, type=Path)
    parser.add_argument("--game-relative", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--maps", type=Path, help="separate ADV_JIT_MAP_DIR, if sampling phase directories differ")
    args = parser.parse_args()
    try:
        result = run(args.directory, args.report, args.game_relative, args.maps)
        args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result, ensure_ascii=False))
    except Exception as error:
        # Input/parser failures can contain private paths and guest addresses.
        print(json.dumps({"status": "error", "error_kind": type(error).__name__}))
        raise SystemExit(1)
