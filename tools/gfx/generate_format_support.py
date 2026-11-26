#!/usr/bin/env python3
"""Project the published FL 11_0 format support columns."""
import argparse
import hashlib
import json
from pathlib import Path
import re

DATA = Path(__file__).resolve().parents[2] / "data/gfx"
STATUS = {"✔": "required", "❔": "optional", "〰": "conditional", "❌": "unavailable"}
TARGETS = {
    "Format Support": "FORMAT",
    "Buffer": "BUFFER",
    "Input Assembler Vertex Buffer": "IA_VERTEX_BUFFER",
    "Input Assembler Index Buffer": "IA_INDEX_BUFFER",
    "Stream Output Buffer": "SO_BUFFER",
    "Texture1D": "TEXTURE1D",
    "Texture2D": "TEXTURE2D",
    "Texture3D": "TEXTURE3D",
    "TextureCube": "TEXTURECUBE",
    "Shader ld": "SHADER_LOAD",
    "Shader sample (any filter)": "SHADER_SAMPLE",
    "Shader sample_c (comparison filter)": "SHADER_SAMPLE_COMPARISON",
    "Shader sample (mono 1_bit_filter)": "SHADER_SAMPLE_MONO_TEXT",
    "Shader gather4": "SHADER_GATHER",
    "Shader gather4_c": "SHADER_GATHER_COMPARISON",
    "Mipmap": "MIP",
    "Mipmap Auto-Generation": "MIP_AUTOGEN",
    "RenderTarget": "RENDER_TARGET",
    "Blendable RenderTarget": "BLENDABLE",
    "Output Merger Logic Op": "OUTPUT_MERGER_LOGIC_OP",
    "Depth/Stencil Target": "DEPTH_STENCIL",
    "Raw UAV and SRV": "RAW_UAV_AND_SRV",
    "Structured UAV and SRV": "STRUCTURED_UAV_AND_SRV",
    "Typed UAV": "TYPED_UNORDERED_ACCESS_VIEW",
    "UAV Typed Store": "UAV_TYPED_STORE",
    "UAV Typed Load": "UAV_TYPED_LOAD",
    "UAV Atomic Add": "UAV_ATOMIC_ADD",
    "UAV Atomic Bitwise Ops": "UAV_ATOMIC_BITWISE_OPS",
    "UAV Atomic Cmp&Store/ Cmp&Exch": "UAV_ATOMIC_COMPARE_STORE_OR_COMPARE_EXCHANGE",
    "UAV Atomic Exchange": "UAV_ATOMIC_EXCHANGE",
    "UAV Atomic Signed Min/Max": "UAV_ATOMIC_SIGNED_MIN_OR_MAX",
    "UAV Atomic Unsigned Min/Max": "UAV_ATOMIC_UNSIGNED_MIN_OR_MAX",
    "CPU Lockable": "CPU_LOCKABLE",
    "4x Multisample RenderTarget": "MSAA_4X",
    "8x Multisample RenderTarget": "MSAA_8X",
    "Other Multisample Count RT": "MSAA_OTHER",
    "Multisample Resolve": "MULTISAMPLE_RESOLVE",
    "Multisample Load": "MULTISAMPLE_LOAD",
    "Display Scan-Out": "DISPLAY",
    "Cast Within Bit Layout": "CAST_WITHIN_BIT_LAYOUT",
    "Video Decoder Support": "DECODER_OUTPUT",
    "Video Processor Input": "VIDEO_PROCESSOR_INPUT",
    "Video Processor Output": "VIDEO_PROCESSOR_OUTPUT",
    "Shared Resource": "SHAREABLE",
    "BackBuffer Castable Even Fully Typed": "BACK_BUFFER_CAST",
    "Tiled Resource": "TILED",
}


def project(source):
    records = {}
    sections = re.split(r"^## (DXGI_FORMAT_\S+?)\s*<sup>[^<]+</sup>\s*\((\d+)\)\s*$",
                        source, flags=re.M)
    for name, number, body in zip(sections[1::3], sections[2::3], sections[3::3]):
        columns = {status: [] for status in STATUS.values()}
        bits = None
        for row in body.splitlines():
            if not row.startswith("|"):
                continue
            target, status = (field.strip().replace("\\_", "_") for field in row.strip("|").split("|"))
            if target == "Bits Per Element (BPE)":
                bits = int(status)
            elif target not in {"Target", "-"}:
                columns[STATUS[status[0]]].append(TARGETS[target])
        records[int(number)] = (name.replace("\\_", "_"), str(bits),
                                *[",".join(columns[key]) or "-"
                                  for key in ("required", "optional", "conditional")])
    header = ("# Microsoft Learn FL 11_0 table. Formats absent from the source have no row.\n"
              "format\tname\tbits_per_element\trequired\toptional\tconditional\n")
    return header + "".join(f"{number}\t" + "\t".join(row) + "\n"
                            for number, row in sorted(records.items()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    source = args.source.read_bytes()
    manifest = json.loads((DATA / "format-support-source.json").read_text())
    if hashlib.sha256(source).hexdigest() != manifest["source_sha256"]:
        raise SystemExit("Format support source hash mismatch")
    expected = project(source.decode())
    path = DATA / "format-support-fl11_0.tsv"
    if args.check:
        if path.read_text() != expected:
            raise SystemExit(f"Stale {path.name}")
    else:
        path.write_text(expected)


if __name__ == "__main__":
    main()
