"""Read i386 COFF objects and the PE/PDB output of MSVC link-time code generation."""

from bisect import bisect_right
from dataclasses import dataclass, field
import re
import struct
import subprocess

from capstone import CS_ARCH_X86, CS_GRP_CALL, CS_GRP_JUMP, CS_GRP_RET, CS_MODE_32, Cs
from capstone.x86 import X86_OP_IMM


DECODER = Cs(CS_ARCH_X86, CS_MODE_32)
DECODER.detail = True


def word(data, offset=0):
    return struct.unpack_from("<I", data, offset)[0]


@dataclass
class Node:
    name: str
    address: int
    kind: str
    data: bytes
    relocations: list = field(default_factory=list)
    errors: list = field(default_factory=list)

    def instructions(self):
        return DECODER.disasm(self.data, self.address) if self.kind == "code" else ()


class Graph:
    def __init__(self):
        self.nodes = {}
        self.symbols = {}
        self.addresses = []

    def add(self, node):
        self.nodes.setdefault(node.address, node)
        if node.name:
            self.symbols.setdefault(node.name, node.address)

    def index(self):
        self.addresses = sorted(self.nodes)

    def containing(self, address):
        index = bisect_right(self.addresses, address) - 1
        if index >= 0:
            node = self.nodes[self.addresses[index]]
            if address == node.address or address < node.address + len(node.data):
                return node
        return None

    def link(self, node, offset, kind, address, anchor=None):
        target = self.nodes.get(anchor) if anchor is not None else self.containing(address)
        if target is None:
            node.errors.append(f"{node.name}+{offset}: relocation target has no extent")
            return
        node.relocations.append((offset, kind, target.address, address - target.address))


@dataclass
class Section:
    address: int
    data: bytes
    flags: int
    relocations: list

    @property
    def code(self):
        return bool(self.flags & 0x20)

    @property
    def kind(self):
        return "code" if self.code else "slot" if self.flags & 0x80000000 else "data"


def coff_sections(data, first, count, image_base=None):
    sections = [None]  # COFF section numbers start at one.
    for number in range(count):
        vsize, rva, size, start, rel, _, nrel, _, flags = struct.unpack_from(
            "<6I2HI", data, first + 40 * number + 8
        )
        address = image_base + rva if image_base is not None else (first + number) * 0x10000
        raw = data[start:start + (min(size, vsize) if image_base is not None else size)] if start else b""
        relocations = [struct.unpack_from("<IIH", data, rel + 10 * i) for i in range(nrel)]
        sections.append(Section(address, raw, flags, relocations))
    return sections


def coff(paths):
    graph = Graph()
    pending = []
    external = {}
    for unit, path in enumerate(paths):
        data = path.read_bytes()
        machine, count, _, table, length, _, _ = struct.unpack_from("<HHIIIHH", data)
        if machine != 0x14C:
            raise ValueError(f"{path.name}: expected an i386 COFF object")
        strings = data[table + 18 * length:]
        sections = coff_sections(data, 20, count)
        for section in sections[1:]:
            section.address += unit * 0x1000000
        symbols = {}
        extents = {}
        section_sizes = {}
        index = 0
        while index < length:
            start = table + 18 * index
            name, value, section_number, typ, storage, auxiliary = struct.unpack_from(
                "<8sIhHBB", data, start
            )
            if name.startswith(b"\0\0\0\0"):
                name = strings[word(name, 4):].split(b"\0", 1)[0]
            name = name.rstrip(b"\0").decode("ascii", errors="replace")
            if section_number > 0:
                section = sections[section_number]
                symbols[index] = (section.address + value, name)
                if storage == 3 and auxiliary and name.startswith("."):
                    section_sizes[section_number] = word(data, start + 18)
                elif storage in (2, 3, 105) and (not section.code or typ & 0x20):
                    extents.setdefault(section_number, {})[value] = name
                    if storage == 2:
                        external.setdefault(name, section.address + value)
            elif section_number == 0 and storage == 2:
                symbols[index] = (None, name)
            index += 1 + auxiliary
        for number, section in enumerate(sections[1:], 1):
            if not section.flags & 0xE0000000 or section.flags & 0x02000800:
                continue
            size = section_sizes.get(number, len(section.data))
            labels = extents.get(number, {})
            if size and (not section.code or not labels):
                labels.setdefault(0, "")
            starts = sorted(labels)
            for start, end in zip(starts, starts[1:] + [size]):
                name = labels[start]
                body = section.data[start:end] if section.kind != "slot" else b""
                kind = "data" if section.code and not name else section.kind
                graph.add(Node(undecorate(name), section.address + start, kind, body))
            for offset, symbol, typ in section.relocations:
                pending.append((section, offset, symbols.get(symbol), typ))
    graph.index()
    slots = {}
    for section, offset, symbol, typ in pending:
        source = graph.containing(section.address + offset)
        if source is None or source.kind == "slot":
            continue
        if typ not in (6, 20) or symbol is None:
            source.errors.append(f"{source.name}: unsupported i386 COFF relocation {typ:#x}")
            continue
        address, name = symbol
        if address is None:
            address = external.get(name)
        if address is None:
            if name not in slots:
                slots[name] = 0xF0000000 + len(slots)
                graph.add(Node(undecorate(name), slots[name], "slot", b""))
            address = slots[name]
        addend = struct.unpack_from("<i", section.data, offset)[0]
        source.relocations.append((section.address + offset - source.address,
                                   "abs" if typ == 6 else "rel", address + addend,
                                   address if addend < 0 or (not name.startswith(".") and address in graph.nodes) else None))
    graph.index()
    for node in graph.nodes.values():
        pending = node.relocations
        node.relocations = []
        for offset, kind, address, anchor in pending:
            graph.link(node, offset, kind, address, anchor)
    add_branches(graph)
    return graph


def undecorate(name):
    return name.removeprefix("__imp_").removeprefix("_")


def add_branches(graph):
    # Jump-table relocations provide additional instruction boundaries after an
    # indirect branch. Never interpret inline pointer tables as instructions.
    starts = {}
    for node in graph.nodes.values():
        for _, kind, target, addend in node.relocations:
            if kind == "abs" and 0 < addend < len(graph.nodes[target].data):
                starts.setdefault(target, set()).add(addend)
    for node in graph.nodes.values():
        if node.kind != "code":
            continue
        known = {offset for offset, *_ in node.relocations}
        pointer_words = {offset for offset, kind, *_ in node.relocations if kind == "abs"}
        todo = [0, *sorted(starts.get(node.address, ()))]
        visited = set()
        while todo:
            offset = todo.pop()
            for instruction in DECODER.disasm(node.data[offset:], node.address + offset):
                start = instruction.address - node.address
                if start in visited or start in pointer_words:
                    break
                visited.add(start)
                jump = instruction.group(CS_GRP_JUMP)
                if jump or instruction.group(CS_GRP_CALL):
                    operand = instruction.operands[0]
                    field = start + instruction.imm_offset
                    if operand.type == X86_OP_IMM and field not in known:
                        address = operand.imm & 0xFFFFFFFF
                        if node.address <= address < node.address + len(node.data):
                            todo.append(address - node.address)
                        elif instruction.imm_size == 4:
                            graph.link(node, field, "rel", address)
                        else:
                            node.errors.append(f"{node.name}+{field}: cross-function branch is not rel32")
                if instruction.mnemonic == "jmp" or instruction.group(CS_GRP_RET):
                    break


PROC = re.compile(
    r"S_[LG]PROC32(?:_ID)? \[.*?\] `([^`]+)`\s+.*?addr = (\d+):(\d+), code size = (\d+)"
)
CONTRIB = re.compile(r"SC\[.*?\].*?mod = (\d+), (\d+):(\d+), size = (\d+)")
DATA = re.compile(r"S_[LG]DATA32 \[.*?\] `([^`]+)`\s+.*?addr = (\d+):(\d+)")
PUBLIC = re.compile(r"S_PUB32 \[.*?\] `([^`]+)`\s+.*?addr = (\d+):(\d+)")


def pe(path, objects, biased_symbols):
    """Use MSVC's final COFF section contributions and procedure records after /GL."""
    data = path.read_bytes()
    header = word(data, 0x3C)
    machine, count, _, _, _, optional_size, _ = struct.unpack_from("<HHIIIHH", data, header + 4)
    if data[header:header + 4] != b"PE\0\0" or machine != 0x14C:
        raise ValueError("expected an i386 PE image")
    optional = header + 24
    base = word(data, optional + 28)
    sections = coff_sections(data, optional + optional_size, count, base)
    pdb = subprocess.check_output([
        "llvm-pdbutil", "dump", "-symbols", "-publics", "-section-contribs",
        str(path.with_suffix(".pdb")),
    ], text=True, encoding="utf-8", errors="replace")
    graph = Graph()
    modules = set()
    biased_targets = {}
    for module in re.split(r"\n(?=Mod )", pdb)[1:]:
        match = re.match(r"Mod (\d+) \| `([^`]+)`", module)
        if match is None or match[2].replace("\\", "/").split("/")[-1] not in objects:
            continue
        modules.add(int(match[1]))
        for name, number, offset, size in PROC.findall(module):
            section, offset, size = sections[int(number)], int(offset), int(size)
            graph.add(Node(name, section.address + offset, "code", section.data[offset:offset + size]))
        for name, number, offset in DATA.findall(module):
            if name in biased_symbols:
                address = sections[int(number)].address + int(offset)
                biased_targets[address + biased_symbols[name]] = address
    graph.index()
    for module, number, offset, size in CONTRIB.findall(pdb):
        section, offset, size = sections[int(number)], int(offset), int(size)
        if int(module) not in modules or section.kind == "slot" or not size:
            continue
        address = section.address + offset
        proc = graph.containing(address)
        skip = len(proc.data) if proc else 0
        if size > skip:
            graph.add(Node("", address + skip, "data", section.data[offset + skip:offset + size]))
    graph.index()
    # Public symbols outside the compiled source set are CRT or linker endpoints.
    for name, number, offset in PUBLIC.findall(pdb):
        number = int(number)
        if not 0 < number < len(sections):
            continue
        address = sections[number].address + int(offset)
        if graph.containing(address) is None:
            graph.add(Node(undecorate(name), address, "slot", b""))
    graph.index()
    rva, size = struct.unpack_from("<II", data, optional + 96 + 5 * 8)
    reloc = next(section for section in sections[1:]
                 if section.address <= base + rva < section.address + len(section.data))
    offset = base + rva - reloc.address
    end = offset + size
    while offset < end:
        page, size = struct.unpack_from("<II", reloc.data, offset)
        for (item,) in struct.iter_unpack("<H", reloc.data[offset + 8:offset + size]):
            if item >> 12 != 3:  # IMAGE_REL_BASED_HIGHLOW
                continue
            address = base + page + (item & 0xFFF)
            node = graph.containing(address)
            if node is not None and node.data:
                operand = address - node.address
                target = word(node.data, operand)
                graph.link(node, operand, "abs", target, biased_targets.get(target))
        offset += size
    add_branches(graph)
    return graph
