#!/usr/bin/env python3
"""Derive the D3D11 tessellator fixture from a d3d11_tess.c probe dump.

Usage: python3 tools/fixtures/d3d11_tess.py d3d11_tess.tsv.gz > fixture.tsv
"""

import argparse
import gzip
import hashlib
import struct


# One complete small case per domain.
LITERAL_CASES = {
    ("tri", "integer", "1,1,1,1,1,1"),
    ("quad", "integer", "1,1,1,1,1,1"),
    ("isoline", "fractional_even", "1,1,1,1,1,1"),
}


def canonical_primitives(vertices, arity):
    for start in range(0, len(vertices), arity):
        primitive = tuple(vertices[start : start + arity])
        if arity == 3:
            primitive = min(primitive[i:] + primitive[:i] for i in range(arity))
        yield primitive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dump")
    args = parser.parse_args()
    cases = {}
    with gzip.open(args.dump, "rt", encoding="ascii") as dump:
        for row in dump:
            fields = row.rstrip("\r\n").split("\t")
            if fields[0] == "device" and fields[2] == "00000000":
                continue
            tag, driver, domain, partitioning, factors, written, needed, stream = fields
            assert tag == "tess", row
            key = (domain, partitioning, factors)
            arity = 2 if domain == "isoline" else 3
            vertices = [
                tuple(int(bits, 16) for bits in location.split("."))
                for location in (stream.split(",") if stream else ())
            ]
            assert written == needed and len(vertices) == int(written) * arity, key
            primitives = sorted(canonical_primitives(vertices, arity))
            packed = b"".join(struct.pack("<III", *v) for p in primitives for v in p)
            expectations = (
                str(len(set(vertices))), written, hashlib.sha256(packed).hexdigest()
            )
            if key in LITERAL_CASES:
                literals = ",".join(
                    ".".join(f"{c:08x}" for c in v) for p in primitives for v in p
                )
                expectations += (literals,)
            drivers = cases.setdefault(key, {})
            assert driver not in drivers, key
            drivers[driver] = expectations

    assert len(cases) == 252, len(cases)
    print("# Generated from d3d11_tess.c by tools/fixtures/d3d11_tess.py.")
    print("# Domain, partitioning, factors, distinct locations, primitives, SHA-256, optional locations")
    for key, drivers in sorted(cases.items()):
        assert drivers.keys() == {"hw", "warp"}, key
        assert drivers["hw"] == drivers["warp"], key
        print(*key, *drivers["hw"], sep="\t")


if __name__ == "__main__":
    main()
