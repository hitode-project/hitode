#!/usr/bin/env python3
"""Compare DirectSound Play snapshots without treating capture time as performance.

Capture with ADV_AUDIO_TRACE=<run>/audio.tsv and ADV_AUDIO_DUMP_DIR=<run>/pcm.
Do not enable ADV_AUDIO_DUMP_CHUNKS for this snapshot-only comparison. Each input
directory must contain the trace and its PCM dumps. --inventory writes an index
for choosing fixed scene buffers. A pair manifest can name different indices and
frame ranges; every unpaired start requires an explicit exclusion reason.

This checks source PCM at Play, not complete tracks, mixer output or a DAC.
--max-error 0 is bit exact. Nonzero tolerances are an explicit codec contract,
never a relaxation of the generic CPU execution contract.
"""
from __future__ import annotations

import argparse
import array
import hashlib
import json
import math
from pathlib import Path
import re
import sys

REPO = Path(__file__).resolve().parents[2]
FIELDS = ("us", "id", "generation", "bytes", "hash", "base", "loop", "rate", "channels", "bits")
FORMAT_FIELDS = ("bytes", "base", "loop", "rate", "channels", "bits")


def fnv1a64(data: bytes) -> str:
    value = 0xCBF29CE484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def samples(data: bytes, bits: int) -> array.array:
    result = array.array("h" if bits == 16 else "B")
    result.frombytes(data)
    if bits == 16 and sys.byteorder != "little":
        result.byteswap()
    # DirectSound 8-bit PCM is unsigned with silence at 128.
    return result if bits == 16 else array.array("h", (value - 128 for value in result))


def inventory(directory: Path, trace_name: str, pcm_name: str) -> list[dict]:
    trace = directory / trace_name
    pcm_dir = (directory / pcm_name).resolve(strict=True)
    if not pcm_dir.is_dir():
        raise ValueError(f"PCM directory is missing: {pcm_dir}")
    rows = []
    keys = set()
    for line_number, line in enumerate(trace.read_text(encoding="utf-8").splitlines(), 1):
        if not line.startswith("start "):
            continue
        try:
            fields = dict(token.split("=", 1) for token in line.split()[1:])
            if any(name not in fields for name in FIELDS):
                raise ValueError("missing start field")
            info = {name: fields[name] for name in FIELDS}
            for name in FIELDS:
                if name not in ("id", "hash"):
                    info[name] = int(info[name])
            if not re.fullmatch(r"[0-9a-f]{8}", info["id"]):
                raise ValueError("invalid buffer id")
            if not re.fullmatch(r"[0-9a-f]{16}", info["hash"]):
                raise ValueError("invalid FNV hash")
            if any(info[name] < 0 for name in FIELDS if name not in ("id", "hash")):
                raise ValueError("negative start field")
            if info["bytes"] == 0:
                continue  # Primary buffer has no source PCM snapshot.
            if info["bits"] not in (8, 16) or not 1 <= info["channels"] <= 8 or info["rate"] <= 0:
                raise ValueError("unsupported or invalid PCM format")
            frame_bytes = info["channels"] * (info["bits"] // 8)
            if info["bytes"] % frame_bytes or info["base"] >= info["bytes"]:
                raise ValueError("incomplete frame or invalid playback base")
            name = f"{info['us']}-{info['id']}-{info['generation']}-{info['hash']}.pcm"
            if name in keys:
                raise ValueError("duplicate start identity")
            keys.add(name)
            path = pcm_dir / name
            if path.is_symlink() or not path.resolve(strict=True).is_relative_to(pcm_dir):
                raise ValueError("snapshot escapes the PCM directory")
            data = path.read_bytes()
            if len(data) != info["bytes"]:
                raise ValueError(f"snapshot length {len(data)} differs from trace {info['bytes']}")
            if fnv1a64(data) != info["hash"]:
                raise ValueError("snapshot FNV differs from its trace")
            values = samples(data, info["bits"])
            rows.append({"index": len(rows), **info, "file": name,
                         "sha256": hashlib.sha256(data).hexdigest(),
                         "frames": info["bytes"] // frame_bytes,
                         "active_samples": sum(value != 0 for value in values)})
        except (OSError, ValueError) as error:
            raise ValueError(f"{trace}:{line_number}: {error}") from error
    if not rows:
        raise ValueError(f"{trace}: no nonempty Play snapshots")
    return rows


def exact_integer(value: object, name: str, minimum: int = 0) -> int:
    if type(value) is not int or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    return value


def pair_manifest(path: Path | None, reference: list[dict], candidate: list[dict]) -> tuple[list, list]:
    if path is None:
        if len(reference) != len(candidate):
            raise ValueError("different start counts; supply explicit pairs and exclusions")
        return [{"label": f"start-{i}", "reference": i, "candidate": i}
                for i in range(len(reference))], []
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or not isinstance(manifest.get("pairs"), list):
        raise ValueError("manifest needs a pairs array")
    pairs = manifest["pairs"]
    exclusions = manifest.get("exclusions", [])
    if not pairs or not isinstance(exclusions, list):
        raise ValueError("manifest needs nonempty pairs and an exclusions array")
    seen = {"reference": set(), "candidate": set()}
    counts = {"reference": len(reference), "candidate": len(candidate)}
    labels = set()
    for pair in pairs:
        if not isinstance(pair, dict) or set(pair) - {"label", "reference", "candidate", "first_frame", "frames"}:
            raise ValueError("unsupported pair fields")
        label = pair.get("label")
        if not isinstance(label, str) or not label.strip() or label in labels:
            raise ValueError("each pair needs a unique nonempty label")
        labels.add(label)
        for side in seen:
            index = exact_integer(pair.get(side), side)
            if index >= counts[side] or index in seen[side]:
                raise ValueError(f"invalid or repeated {side} pair index {index}")
            seen[side].add(index)
        exact_integer(pair.get("first_frame", 0), "first_frame")
        if "frames" in pair:
            exact_integer(pair["frames"], "frames", 1)
    for row in exclusions:
        if not isinstance(row, dict) or set(row) != {"side", "index", "reason"}:
            raise ValueError("exclusion needs side, index and reason")
        side = row["side"]
        if side not in seen or not isinstance(row["reason"], str) or not row["reason"].strip():
            raise ValueError("invalid exclusion side or missing reason")
        index = exact_integer(row["index"], "exclusion index")
        if index >= counts[side] or index in seen[side]:
            raise ValueError(f"invalid or repeated {side} exclusion index {index}")
        seen[side].add(index)
    for side, indices in seen.items():
        if indices != set(range(counts[side])):
            raise ValueError(f"every {side} start must be paired or explicitly excluded")
    return pairs, exclusions


def compare(reference: dict, candidate: dict, pair: dict,
            reference_dir: Path, candidate_dir: Path, pcm_name: str) -> dict:
    for field in FORMAT_FIELDS:
        if reference[field] != candidate[field]:
            raise ValueError(f"{pair['label']}: {field} differs ({reference[field]} vs {candidate[field]})")
    channels = reference["channels"]
    first = pair.get("first_frame", 0)
    frames = pair.get("frames", reference["frames"] - first)
    if first >= reference["frames"] or first + frames > reference["frames"]:
        raise ValueError(f"{pair['label']}: frame range is outside the snapshot")
    frame_bytes = channels * (reference["bits"] // 8)
    start, end = first * frame_bytes, (first + frames) * frame_bytes

    def selected(info: dict, directory: Path) -> array.array:
        path = directory / pcm_name / info["file"]
        if path.is_symlink():
            raise ValueError(f"{pair['label']}: snapshot became a symlink after inventory")
        raw = path.read_bytes()
        if len(raw) != info["bytes"] or hashlib.sha256(raw).hexdigest() != info["sha256"]:
            raise ValueError(f"{pair['label']}: snapshot changed after inventory")
        return samples(raw[start:end], info["bits"])

    left = selected(reference, reference_dir)
    right = selected(candidate, candidate_dir)
    if len(left) != len(right) or not left:
        raise ValueError(f"{pair['label']}: empty or unequal sample lengths")
    changed = active = reference_active = peak = error_energy = reference_energy = 0
    for old, new in zip(left, right, strict=True):
        error = int(new) - int(old)
        changed += error != 0
        active += old != 0 or new != 0
        reference_active += old != 0
        peak = max(peak, abs(error))
        error_energy += error * error
        reference_energy += int(old) * int(old)
    relative = math.sqrt(error_energy / reference_energy) if reference_energy else None
    snr = 10 * math.log10(reference_energy / error_energy) if error_energy and reference_energy else None
    return {"label": pair["label"], "reference_index": reference["index"],
            "candidate_index": candidate["index"], "bits": reference["bits"],
            "rate": reference["rate"], "channels": channels, "first_frame": first,
            "frames": frames, "samples": len(left), "active_samples": active,
            "reference_active_samples": reference_active,
            "different_samples": changed, "max_error_lsb": peak,
            "rms_lsb": math.sqrt(error_energy / len(left)),
            "rms_active_lsb": math.sqrt(error_energy / active) if active else 0.0,
            "relative_rms": relative, "snr_db": snr,
            "error_energy_lsb_squared": error_energy,
            "reference_energy_lsb_squared": reference_energy,
            "bit_exact": changed == 0}


def emit(result: dict, output: Path | None) -> None:
    encoded = json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n"
    if output is not None:
        if not output.resolve().is_relative_to(REPO / "target"):
            raise ValueError("--output must be inside this repository's ignored target directory")
        output.parent.mkdir(parents=True, exist_ok=True)
        temporary = output.with_suffix(output.suffix + ".tmp")
        temporary.write_text(encoded, encoding="utf-8")
        temporary.replace(output)
    print(encoded, end="")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--reference", type=Path, required=True)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--inventory", action="store_true")
    parser.add_argument("--trace", default="audio.tsv")
    parser.add_argument("--pcm-dir", default="pcm")
    parser.add_argument("--pairs", type=Path, help="explicit fixed-scene pairs and exclusions JSON")
    parser.add_argument("--max-error", type=int, help="required maximum absolute PCM error in LSB; 0 is bit exact")
    parser.add_argument("--max-rms-active", type=float, help="optional maximum RMS on the union of nonzero samples")
    parser.add_argument("--min-snr-db", type=float, help="optional strict SNR lower bound over the selected samples")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.max_error is not None and args.max_error < 0:
        parser.error("--max-error must be >= 0")
    for value, name in ((args.max_rms_active, "--max-rms-active"), (args.min_snr_db, "--min-snr-db")):
        if value is not None and (not math.isfinite(value) or value < 0):
            parser.error(f"{name} must be finite and >= 0")
    if not args.inventory and (args.candidate is None or args.max_error is None):
        parser.error("comparison requires --candidate and an explicit --max-error")
    try:
        reference = inventory(args.reference, args.trace, args.pcm_dir)
        result = {"scope": "DirectSound Play source snapshots only; no full-track or DAC claim",
                  "reference": str(args.reference.resolve()), "reference_trace_sha256":
                  hashlib.sha256((args.reference / args.trace).read_bytes()).hexdigest(),
                  "reference_inventory": reference}
        if args.inventory:
            emit(result, args.output)
            return 0
        candidate = inventory(args.candidate, args.trace, args.pcm_dir)
        pairs, exclusions = pair_manifest(args.pairs, reference, candidate)
        rows = [compare(reference[pair["reference"]], candidate[pair["candidate"]], pair,
                        args.reference, args.candidate, args.pcm_dir) for pair in pairs]
        failures = []
        for row in rows:
            if row["max_error_lsb"] > args.max_error:
                failures.append(f"{row['label']}: maximum error exceeds {args.max_error} LSB")
            if args.max_rms_active is not None and row["rms_active_lsb"] > args.max_rms_active:
                failures.append(f"{row['label']}: active RMS exceeds {args.max_rms_active} LSB")
            if args.min_snr_db is not None and not row["bit_exact"] and (
                    row["snr_db"] is None or row["snr_db"] <= args.min_snr_db):
                failures.append(f"{row['label']}: SNR is undefined or <= {args.min_snr_db} dB")
        if not sum(row["reference_active_samples"] for row in rows):
            failures.append("selected reference PCM is entirely silent; no active codec evidence")
        result.update(candidate=str(args.candidate.resolve()), candidate_inventory=candidate,
                      candidate_trace_sha256=hashlib.sha256((args.candidate / args.trace).read_bytes()).hexdigest(),
                      pairs_manifest_sha256=hashlib.sha256(args.pairs.read_bytes()).hexdigest() if args.pairs else None,
                      exclusions=exclusions, rows=rows, contract={"max_error_lsb": args.max_error,
                      "max_rms_active_lsb": args.max_rms_active, "min_snr_db": args.min_snr_db},
                      total_samples=sum(row["samples"] for row in rows),
                      total_different_samples=sum(row["different_samples"] for row in rows),
                      eligible=not failures, failures=failures)
        emit(result, args.output)
        return 0 if not failures else 1
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"codec PCM evidence error: {error}", file=sys.stderr)
        # A failed rerun must not leave an older eligible report at --output.
        try:
            emit({"scope": "DirectSound Play source snapshots only; no full-track or DAC claim",
                  "eligible": False, "evidence_errors": [str(error)]}, args.output)
        except (OSError, ValueError) as output_error:
            print(f"cannot save failed codec evidence: {output_error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
