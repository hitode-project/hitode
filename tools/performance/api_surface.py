#!/usr/bin/env python3
"""Rank unresolved API families from recorded startup matrices.

This is planning evidence, not a compatibility score: deeper execution may load
more DLLs and reveal more imports. Missing imports are not necessarily called.
"""
from __future__ import annotations

import argparse
import json
import re
from collections import Counter, defaultdict
from pathlib import Path


def summarize(path: Path) -> dict:
    campaign = json.loads(path.read_text(encoding="utf-8-sig"))
    unresolved = defaultdict(lambda: {"samples": set(), "engines": set()})
    blockers = defaultdict(set)
    for sample in campaign["samples"]:
        for api in set(sample.get("missing_apis") or []):
            unresolved[api]["samples"].add(sample["id"])
            unresolved[api]["engines"].add(sample.get("engine", "unknown"))
        blocker = sample.get("blocker_api")
        if not blocker:
            match = re.search(r"unimplemented:\s+([A-Za-z0-9_.-]+\.dll![A-Za-z0-9_@?#.$-]+)",
                              sample.get("failure") or "")
            blocker = match.group(1) if match else None
        if blocker:
            blockers[blocker].add(sample["id"])
    rows = []
    for api, observations in unresolved.items():
        rows.append({"api": api, "sample_count": len(observations["samples"]),
                     "engine_count": len(observations["engines"]),
                     "samples": sorted(observations["samples"]),
                     "engines": sorted(observations["engines"]),
                     "called_blocker_samples": sorted(blockers.get(api, set()))})
    rows.sort(key=lambda row: (-row["engine_count"], -row["sample_count"], row["api"]))
    return {"source": str(path.resolve()), "binary_sha256": campaign.get("binary_sha256"),
            "samples": len(campaign["samples"]), "statuses": dict(Counter(
                sample.get("status", "unknown") for sample in campaign["samples"])),
            "unresolved_apis": rows,
            "called_blockers": {api: sorted(ids) for api, ids in sorted(blockers.items())}}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("matrices", nargs="+", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    summaries = [summarize(path) for path in args.matrices]
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "api-surface.json").write_text(
        json.dumps({"qualification": __doc__, "campaigns": summaries}, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8")
    lines = ["# Unresolved API surface", "", "Ranked by distinct engine families, then samples.",
             "Missing imports are planning evidence, not proof that a call executed.",
             "A later campaign may discover more dependencies; counts are not a compatibility percentage.", ""]
    for summary in summaries:
        lines += [f"## {Path(summary['source']).parent.name}", "",
                  f"Runner SHA-256: `{summary['binary_sha256']}`. Samples: {summary['samples']}.", "",
                  "| API | Engines | Samples | Actually called blocker samples |",
                  "|---|---:|---:|---|"]
        for row in summary["unresolved_apis"]:
            lines.append(f"| `{row['api']}` | {row['engine_count']} | {row['sample_count']} | "
                         + ", ".join(row["called_blocker_samples"]) + " |")
        lines.append("")
    (args.output / "api-surface.md").write_text("\n".join(lines), encoding="utf-8")
    print(json.dumps([{"source": row["source"], "samples": row["samples"],
                       "unresolved_names": len(row["unresolved_apis"])} for row in summaries], indent=2))


if __name__ == "__main__":
    main()
