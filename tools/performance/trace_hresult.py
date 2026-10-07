#!/usr/bin/env python3
"""Summarize failed HRESULT calls in an API trace using the generated ABI.

Repeated render failures can hide behind a successful Present. Expected
capability queries also return failures, so assess counts with their API names.
Only call metadata is retained: no guest arguments or runtime text.
"""
import argparse
from collections import defaultdict
import json
from pathlib import Path


def contracts(root):
    functions, methods = set(), set()
    for filename, target, method in (("signatures.tsv", functions, False),
                                     ("com.tsv", methods, True)):
        for row in (root / "data" / "abi" / filename).read_text(encoding="utf8").splitlines():
            if not row or row.startswith("#"):
                continue
            fields = row.split("\t")
            if fields[4].rsplit(".", 1)[-1] == "HRESULT":
                target.add(fields[0] + ("::" if method else "!") + fields[2 if method else 1])
    return functions, methods


def summarize(trace, root):
    functions, methods = contracts(root)
    counts = defaultdict(lambda: {"calls": 0, "failures": {}, "first": None})
    with trace.open(encoding="utf8", errors="replace") as stream:
        for row in stream:
            fields = row.rstrip("\r\n").split("\t")
            if len(fields) != 5 or not fields[0].isdigit() or "!" not in fields[2]:
                continue
            api = fields[2]
            if api not in functions and api.split("!", 1)[1] not in methods:
                continue
            try:
                result = int(fields[4], 16)
                context, frame = int(fields[0]), int(fields[1])
            except ValueError:
                continue
            if not 0 <= result <= 0xffffffff:
                continue
            entry = counts[api]
            entry["calls"] += 1
            if result & 0x80000000:
                code = f"0x{result:08x}"
                entry["failures"][code] = entry["failures"].get(code, 0) + 1
                if entry["first"] is None:
                    entry["first"] = {"context": context, "frame": frame, "hresult": code}
    failures = [{"api": api, **entry} for api, entry in counts.items() if entry["failures"]]
    failures.sort(key=lambda entry: (-sum(entry["failures"].values()), entry["api"]))
    return {"failed_hresult_calls": failures}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = json.dumps(summarize(args.trace, Path(__file__).resolve().parents[2]), indent=2)
    if args.output:
        args.output.write_text(result + "\n", encoding="utf8")
    else:
        print(result)


if __name__ == "__main__":
    main()
