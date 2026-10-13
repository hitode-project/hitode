#!/usr/bin/env python3
"""Compare the version hierarchy model with native Windows mixed conditions.

This is a read-only host ABI probe, not an emulated CPU test. The model follows
adv-win32/src/kernel32/osversion.rs, using the version observed by this process so
the comparison does not assume that Windows is the guest's Windows 7 persona.
Inputs: this process's native version and mixed major/minor/service-pack comparisons.
Output: JSON at target/version-condition-probe/report.json by default, with mismatch examples.
Exit codes: 0 for agreement, 1 for mismatches, 2 for native API or setup failure.
Run on Windows: python tools/performance/version_condition_probe.py
Test: python -m unittest discover -s tools/performance/tests -p test_version_condition_probe.py -v
The probe checks Boolean results and LastError without modifying host configuration.
"""
from __future__ import annotations

import argparse
import ctypes
import itertools
import json
import os
from pathlib import Path


class VersionInfo(ctypes.Structure):
    """OSVERSIONINFOEXW with fixed Windows integer/UTF-16 widths."""

    _fields_ = [
        ("size", ctypes.c_uint32),
        ("major", ctypes.c_uint32),
        ("minor", ctypes.c_uint32),
        ("build", ctypes.c_uint32),
        ("platform", ctypes.c_uint32),
        ("service_pack_text", ctypes.c_uint16 * 128),
        ("sp_major", ctypes.c_uint16),
        ("sp_minor", ctypes.c_uint16),
        ("suite", ctypes.c_uint16),
        ("product", ctypes.c_uint8),
        ("reserved", ctypes.c_uint8),
    ]


FIELDS = ("minor", "major", "build", "platform", "sp_minor", "sp_major", "suite", "product")
HIERARCHY = (1, 0, 5, 4)
TYPE_MASK = sum(1 << bit for bit in HIERARCHY)


def compare(actual: int, requested: int, condition: int) -> bool:
    """Windows VER_EQUAL, GREATER, GREATER_EQUAL, LESS, LESS_EQUAL."""
    return {
        1: actual == requested,
        2: actual > requested,
        3: actual >= requested,
        4: actual < requested,
        5: actual <= requested,
    }.get(condition, False)


def hierarchy_matches(actual: list[int], requested: list[int], condition_mask: int) -> bool:
    """Mirror the selected hierarchy branch, including inherited conditions."""
    operation = 0
    for bit in HIERARCHY:
        if operation in (0, 1):
            operation = (condition_mask >> (3 * bit)) & 7
        if operation == 0:
            return False
        if actual[bit] != requested[bit] or bit == HIERARCHY[-1]:
            return compare(actual[bit], requested[bit], operation)
    return True


def probe(max_examples: int = 24) -> dict:
    if os.name != "nt":
        raise RuntimeError("The native version-condition probe requires Windows.")
    if ctypes.sizeof(VersionInfo) != 284:
        raise RuntimeError("Unexpected OSVERSIONINFOEXW layout.")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetVersionExW.argtypes = [ctypes.POINTER(VersionInfo)]
    kernel.GetVersionExW.restype = ctypes.c_int
    kernel.VerifyVersionInfoW.argtypes = [ctypes.POINTER(VersionInfo), ctypes.c_uint32, ctypes.c_uint64]
    kernel.VerifyVersionInfoW.restype = ctypes.c_int
    kernel.VerSetConditionMask.argtypes = [ctypes.c_uint64, ctypes.c_uint32, ctypes.c_uint8]
    kernel.VerSetConditionMask.restype = ctypes.c_uint64
    observed = VersionInfo()
    observed.size = ctypes.sizeof(VersionInfo)
    if not kernel.GetVersionExW(ctypes.byref(observed)):
        raise OSError(ctypes.get_last_error(), "GetVersionExW failed")
    actual = [getattr(observed, field) for field in FIELDS]
    tested = mismatch_count = 0
    examples = []
    for offsets in itertools.product((-1, 0, 1), repeat=4):
        for operators in itertools.product(range(1, 6), repeat=4):
            requested = actual.copy()
            info = VersionInfo.from_buffer_copy(observed)
            condition = 0
            for bit, offset, operator in zip(HIERARCHY, offsets, operators):
                requested[bit] = max(0, actual[bit] + offset)
                setattr(info, FIELDS[bit], requested[bit])
                condition = kernel.VerSetConditionMask(condition, 1 << bit, operator)
            ctypes.set_last_error(0x1234)
            native = bool(kernel.VerifyVersionInfoW(ctypes.byref(info), TYPE_MASK, condition))
            error = ctypes.get_last_error()
            modeled = hierarchy_matches(actual, requested, condition)
            expected_error = 0x1234 if modeled else 1150  # ERROR_OLD_WIN_VERSION
            tested += 1
            if native != modeled or error != expected_error:
                mismatch_count += 1
                if len(examples) < max_examples:
                    examples.append({
                        "offsets": offsets, "operators": operators,
                        "native": native, "modeled": modeled,
                        "last_error": error, "expected_last_error": expected_error,
                        "requested": dict(zip(FIELDS, requested)),
                    })
    return {
        "schema_version": 1,
        "probe": "native Windows VerifyVersionInfoW mixed hierarchy conditions",
        "qualification": "Python model/native ABI comparison; not a guest CPU or fixed Windows 7 identity test",
        "actual": dict(zip(FIELDS, actual)),
        "type_mask": TYPE_MASK,
        "tested": tested,
        "mismatch_count": mismatch_count,
        "first_mismatches": examples,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[2] / "target/version-condition-probe/report.json")
    parser.add_argument("--max-examples", type=int, default=24)
    args = parser.parse_args()
    if args.max_examples < 0:
        parser.error("--max-examples must be nonnegative")
    try:
        report = probe(args.max_examples)
    except (OSError, RuntimeError) as error:
        parser.exit(2, f"{error}\n")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"{report['tested']} cases, {report['mismatch_count']} mismatches; {args.output.resolve()}")
    return int(report["mismatch_count"] != 0)


if __name__ == "__main__":
    raise SystemExit(main())
