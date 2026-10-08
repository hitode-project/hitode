"""Project PE32 export metadata into the fixed system personality catalog.

Usage: python tools/abi/system_exports.py --system-dir <x86-system-directory>
       --api-set-schema <native-apisetschema.dll>
Input: ntdll.dll, kernel32.dll, kernelbase.dll and apisetschema.dll metadata.
Output: sorted data/abi/system-exports.tsv and provenance JSON; stdout contains
counts only. No instruction bytes, export addresses or local paths are emitted.
The catalog records existence and routing, never a calling convention or an
implementation promise. API-set rows contain only contracts referenced by the
three core modules, resolved through the namespace's importer host mapping.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct


class PE:
    def __init__(self, data):
        self.data = data
        if data[:2] != b"MZ":
            raise ValueError("missing DOS header")
        pe = self.u32(0x3C)
        if data[pe:pe + 4] != b"PE\0\0":
            raise ValueError("missing PE header")
        self.optional = pe + 24
        if self.u16(self.optional) not in (0x10B, 0x20B):
            raise ValueError("unsupported PE optional header")
        self.pe32 = self.u16(self.optional) == 0x10B
        sections = self.optional + self.u16(pe + 20)
        self.sections = []
        for at in range(sections, sections + self.u16(pe + 6) * 40, 40):
            self.sections.append((
                data[at:at + 8].rstrip(b"\0").decode("ascii"),
                self.u32(at + 12), self.u32(at + 8), self.u32(at + 20),
                self.u32(at + 16), self.u32(at + 36),
            ))

    def u16(self, at):
        return struct.unpack_from("<H", self.data, at)[0]

    def u32(self, at):
        return struct.unpack_from("<I", self.data, at)[0]

    def offset(self, rva):
        for _, start, virtual, raw, size, _ in self.sections:
            if start <= rva < start + max(virtual, size):
                at = raw + rva - start
                if at >= raw + size or at >= len(self.data):
                    break
                return at
        raise ValueError("unbacked metadata RVA")

    def string(self, rva):
        at = self.offset(rva)
        return self.data[at:self.data.index(0, at)].decode("ascii")

    def exports(self):
        if not self.pe32:
            raise ValueError("core exports require PE32")
        directory = self.optional + 96
        rva, size = self.u32(directory), self.u32(directory + 4)
        if not rva or size < 40:
            raise ValueError("missing export directory")
        at = self.offset(rva)
        base, count = self.u32(at + 16), self.u32(at + 24)
        functions = self.offset(self.u32(at + 28))
        names = self.offset(self.u32(at + 32))
        ordinals = self.offset(self.u32(at + 36))
        result = []
        for index in range(count):
            name = self.string(self.u32(names + index * 4))
            ordinal_index = self.u16(ordinals + index * 2)
            target = self.u32(functions + ordinal_index * 4)
            ordinal = base + ordinal_index
            if not target or not 0 < ordinal <= 65535:
                raise ValueError("invalid named export")
            if rva <= target < rva + size:
                kind, forwarder = "forwarder", self.string(target)
                if "." not in forwarder:
                    raise ValueError("invalid forwarder")
            else:
                executable = any(start <= target < start + max(virtual, raw_size)
                                 and flags & 0x20000000
                                 for _, start, virtual, _, raw_size, flags in self.sections)
                kind, forwarder = ("code" if executable else "data"), ""
            result.append((ordinal, name, kind, forwarder))
        if len({name for _, name, _, _ in result}) != count:
            raise ValueError("duplicate export name")
        return sorted(result, key=lambda entry: entry[1])

    def version(self):
        for name, _, _, raw, size, _ in self.sections:
            if name == ".rsrc":
                resource = self.data[raw:raw + size]
                signature = struct.pack("<II", 0xFEEF04BD, 0x10000)
                at = resource.find(signature)
                if at >= 0:
                    major, minor = struct.unpack_from("<II", resource, at + 8)
                    return f"{major >> 16}.{major & 65535}.{minor >> 16}.{minor & 65535}"
        raise ValueError("missing fixed file version")

    def api_sets(self):
        raw, size = next((raw, size) for name, _, _, raw, size, _ in self.sections
                         if name == ".apiset")
        data = self.data[raw:raw + size]
        u32 = lambda at: struct.unpack_from("<I", data, at)[0]
        if u32(0) != 6 or u32(4) > len(data):
            raise ValueError("unsupported API-set namespace")
        text = lambda at, length: data[at:at + length].decode("utf-16-le")
        result = {}
        entries = u32(16)
        for index in range(u32(12)):
            entry = entries + index * 24
            # Windows hashes the contract prefix, excluding its last version.
            contract = text(u32(entry + 4), u32(entry + 12)).lower()
            values, count = u32(entry + 16), u32(entry + 20)
            hosts = {}
            for item in range(count):
                value = values + item * 20
                importer = text(u32(value + 4), u32(value + 8)).lower()
                host = text(u32(value + 12), u32(value + 16)).lower()
                if host:
                    hosts[importer] = host
            if contract in result and result[contract] != hosts:
                raise ValueError("ambiguous API-set contract")
            result[contract] = hosts
        return result


def project(system_dir, api_set_schema):
    rows, source, references = [], {}, []
    for module in ("ntdll.dll", "kernel32.dll", "kernelbase.dll"):
        data = (system_dir / module).read_bytes()
        pe = PE(data)
        exports = pe.exports()
        source[module] = {"file_version": pe.version(),
                          "sha256": hashlib.sha256(data).hexdigest(),
                          "named_exports": len(exports)}
        for ordinal, name, kind, target in exports:
            rows.append((module, ordinal, name, kind, target))
            if target.lower().startswith(("api-ms-", "ext-ms-")):
                contract, symbol = target.rsplit(".", 1)
                references.append((contract.lower() + ".dll", symbol, module))
    data = api_set_schema.read_bytes()
    schema = PE(data)
    mapping = schema.api_sets()
    source["apisetschema.dll"] = {"file_version": schema.version(),
                                  "sha256": hashlib.sha256(data).hexdigest()}
    for contract, symbol, importer in sorted(set(references)):
        hosts = mapping.get(contract[:-4].rsplit("-", 1)[0], {})
        host = hosts.get(importer, hosts.get(""))
        if host not in ("ntdll.dll", "kernel32.dll", "kernelbase.dll"):
            raise ValueError("referenced API-set has no fixed core host")
        rows.append((contract, 0, symbol, "forwarder", host[:-4] + "." + symbol))
    rows.sort(key=lambda row: (row[0], row[2]))
    return rows, source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--system-dir", type=Path, required=True)
    parser.add_argument("--api-set-schema", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path,
                        default=Path(__file__).resolve().parents[2] / "data" / "abi")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        rows, source = project(args.system_dir, args.api_set_schema)
    except (OSError, ValueError, struct.error, StopIteration):
        parser.error("system export metadata could not be validated")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    header = ("# Generated by tools/abi/system_exports.py; metadata only, no code or ABI.\n"
              "# module\tordinal (0 for API-set projection)\tname\tkind\tforwarder\n")
    outputs = {
        "system-exports.tsv": header + "".join("\t".join(map(str, row)) + "\n" for row in rows),
        "system-exports-source.json": json.dumps({"format": 1, "source": source}, indent=2, sort_keys=True) + "\n",
    }
    for name, output in outputs.items():
        if args.check:
            if (args.output_dir / name).read_text(encoding="utf-8") != output:
                parser.error("fixed export metadata differs from the selected source")
        else:
            (args.output_dir / name).write_text(output, encoding="utf-8", newline="\n")
    print(f"exports={len(rows)} core={sum(source[m]['named_exports'] for m in source if m != 'apisetschema.dll')} "
          f"forwarders={sum(row[3] == 'forwarder' for row in rows)} data={sum(row[3] == 'data' for row in rows)}")


if __name__ == "__main__":
    main()
