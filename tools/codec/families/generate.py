#!/usr/bin/env python3
"""Build fingerprints from released Xiph sources in an x86 compiler environment."""

import argparse
from collections import deque
from hashlib import sha256
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import tarfile
import tomllib
from urllib.request import urlretrieve

from capstone.x86 import X86_OP_IMM, X86_OP_MEM, X86_OP_REG, X86_REG_EBP, X86_REG_ESP

from objects import coff, pe


HERE = Path(__file__).resolve().parent
LIBRARIES = ("theora", "vorbis", "ogg")
LINK_FLAGS = ("/DEBUG:FULL", "/OPT:REF,NOICF", "/INCREMENTAL:NO")


def recipe(library):
    return tomllib.loads((HERE / f"{library}.toml").read_text(encoding="utf-8"))


def sources(work, library, version):
    name = f"lib{library}-{version}"
    archive = work / f"{name}.tar.xz"
    url = f"https://downloads.xiph.org/releases/{library}/{archive.name}"
    if not archive.is_file():
        urlretrieve(url, archive)
    digest = sha256(archive.read_bytes()).hexdigest()
    if digest != recipe(library)["versions"][version]:
        raise ValueError(f"{archive.name}: source checksum mismatch")
    if not (work / name).is_dir():
        with tarfile.open(archive) as release:
            release.extractall(work, filter="data")
    return work / name, {"version": version, "url": url, "sha256": digest}


def build_inputs(work, library, version, compiler, spec):
    source, release = sources(work, library, version)
    ogg, _ = sources(work, "ogg", version if library == "ogg" else "1.3.6")
    includes = [source / "include", source / "lib", ogg / "include"]
    defines = ["NDEBUG"]
    filenames = spec["sources"]
    if library == "theora":
        vc = compiler != "mingw"
        arch = "x86_vc" if vc else "x86"
        filenames = [f"lib/{name}.c" for name in filenames]
        # Clang's Microsoft asm parser rejects the upstream MMX IDCT macro.
        if compiler != "clang-cl":
            defines.append("OC_X86_ASM")
            filenames += [f"lib/{arch}/{name}.c" for name in ("mmxfrag", "mmxidct", "mmxstate", "x86state")]
            if version != "1.1.1":
                filenames.append(f"lib/{arch}/x86cpu.c")
                if not vc:
                    filenames.append("lib/x86/sse2idct.c")
    return [source / name for name in filenames], includes, defines, release


def run(command, build):
    result = subprocess.run(command, cwd=build, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode:
        # Keep compiler diagnostics local. The manifest records the build phase only.
        (build / "error.log").write_bytes(result.stdout)
        raise ValueError(f"{Path(command[0]).name} exited {result.returncode}")


def mingw_command(directory=None):
    prefix = ["wsl.exe", *(["--cd", str(directory)] if directory else []), "--"] if os.name == "nt" else []
    return prefix + ["i686-w64-mingw32-gcc"]


def version_of(compiler):
    executable = {"msvc": "cl", "clang-cl": "clang-cl", "mingw": "i686-w64-mingw32-gcc"}[compiler]
    option = "/?" if compiler == "msvc" else "--version"
    command = mingw_command() if compiler == "mingw" else [executable]
    banner = subprocess.check_output([*command, option], stderr=subprocess.STDOUT).decode(errors="replace")
    if compiler == "msvc":
        return re.search(r"\b19\.\d+\.\d+(?:\.\d+)?\b", banner)[0]
    return banner.splitlines()[0]


def matrix_flags(compiler):
    if compiler == "msvc":
        for optimize in ("/O1", "/O2", "/Ox"):
            for whole_program in (False, True):
                yield [optimize, "/GL" if whole_program else "/GL-"]
    elif compiler == "clang-cl":
        yield ["-m32", "/O2"]
    else:
        yield ["-O2"]


def compile_objects(args, build, inputs, includes, defines, flags):
    def relative(path):
        return Path(os.path.relpath(path, build)).as_posix()
    if args.compiler == "mingw":
        base = ["i686-w64-mingw32-gcc", "-ffunction-sections", "-fdata-sections"]
        command = mingw_command(build) + base[1:] + [f"-I{relative(path)}" for path in includes] + [f"-D{d}" for d in defines]
        for source in inputs:
            run(command + flags + ["-c", relative(source), "-o", f"{source.stem}.obj"], build)
    else:
        base = ["cl", "/Gy", "/Gw"] if args.compiler == "msvc" else ["clang-cl", "/Gy"]
        base += ["/MD", "/Gd", "/Z7"]
        command = base + [f"/I{relative(path)}" for path in includes] + [f"/D{d}" for d in defines]
        for source in inputs:
            run(command + flags + ["/nologo", "/c", relative(source)], build)
    return base[1:]


def prologue(node):
    shape = []
    for ins in node.instructions():
        operands = []
        for operand in ins.operands:
            if operand.type == X86_OP_REG:
                operands.append(ins.reg_name(operand.reg))
            elif operand.type == X86_OP_MEM:
                registers = [ins.reg_name(reg) for reg in (operand.mem.base, operand.mem.index) if reg]
                operands.append("[" + "+".join(registers or ["abs"]) + "]")
            elif operand.type == X86_OP_IMM:
                operands.append("imm")
        shape.append(ins.mnemonic + ("(" + ",".join(operands) + ")" if operands else ""))
        size = ins.address + ins.size - node.address
        if size >= 5:
            normalized = bytearray(node.data[:size])
            for offset, *_ in node.relocations:
                if offset < size:
                    normalized[offset:offset + 4] = bytes(4)
            return size, sha256(normalized).hexdigest(), shape
    raise ValueError(f"{node.name}: hook is shorter than five bytes")


def render(graph, spec, fields, values):
    roots = {role: graph.symbols[name] for role, name in spec["roots"].items()}
    hooks = {role: graph.symbols[name] for role, name in spec.get("hooks", {}).items()
             if name in graph.symbols}
    names = {address: role for role, address in (roots | hooks).items()}
    for role, symbol in spec.get("roles", {}).items():
        if symbol in graph.symbols:
            names.setdefault(graph.symbols[symbol], role)
    if "malloc" in graph.symbols:
        names.setdefault(graph.symbols["malloc"], "malloc")
    output, notes, visited = [], [], {}
    queue = deque(roots.values())
    alternate_roots = []
    negative_addends = 0
    for role, address in roots.items():
        size, digest, shape = prologue(graph.nodes[address])
        notes.append(f"# prologue {role} {digest} {' '.join(shape)}")
        output.append(f"root {role} {size}")
        if shape[:2] != ["push(ebp)", "mov(ebp,esp)"]:
            alternate_roots.append(role)
    while queue:
        address = queue.popleft()
        if address in visited:
            continue
        role = names[address]
        node = graph.nodes[address]
        if node.errors:
            raise ValueError(node.errors[0])
        visited[address] = node
        if role in hooks:
            size, _, _ = prologue(node)
            output.append(f"hook {role} {size}")
        if node.kind == "slot":
            output.append(f"slot {role}")
            continue
        normalized = bytearray(node.data)
        for offset, kind, target, addend in sorted(set(node.relocations)):
            target_role = names.setdefault(target, f"n{len(names)}")
            negative_addends += addend < 0
            output.append(f"link {role} {offset} {kind} {target_role} {addend}")
            normalized[offset:offset + 4] = bytes(4)
            queue.append(target)
        output.append(f"{node.kind} {role} {len(normalized)} {sha256(normalized).hexdigest()}")
    literal_fields = []
    instructions = {node.name: list(node.instructions()) for node in visited.values() if node.name}
    for (name, (_, *symbols)), value in zip(fields.items(), values, strict=True):
        location = None
        for symbol in symbols:
            for instruction in instructions.get(symbol, ()):
                for index, operand in enumerate(instruction.operands):
                    if (instruction.modrm_offset and operand.type == X86_OP_MEM
                            and operand.mem.disp == value
                            and operand.mem.base not in (0, X86_REG_ESP, X86_REG_EBP)):
                        address = graph.symbols[symbol]
                        location = f"{names[address]} {instruction.address - address:#x} {index}"
                        break
                if location:
                    break
            if location:
                break
        if not location and symbols:
            literal_fields.append(name)
        output.append(f"field {name} {location or value}")
    detail = {"alternate_roots": alternate_roots,
              "absent_hooks": sorted(role for role, address in hooks.items() if address not in visited)
              + sorted(set(spec.get("hooks", {})) - hooks.keys()),
              "literal_fields": literal_fields, "negative_addends": negative_addends}
    return "\n".join(notes + sorted(output)) + "\n", detail


def generate(args, library, version, flags, build):
    build.mkdir(parents=True, exist_ok=True)
    (build / "error.log").unlink(missing_ok=True)
    spec = recipe(library)
    inputs, includes, defines, release = build_inputs(args.work, library, version, args.compiler, spec)
    fields = spec["fields"] | spec.get("overrides", {}).get(version, {})
    layout_source = build / "layout.c"
    header = f"#include <stddef.h>\n#include {spec['header']}\n"
    expressions = ",\n".join(field[0] for field in fields.values())
    layout_source.write_text(header + f"const unsigned codec_layout[] = {{\n{expressions}\n}};\n")
    fixed_flags = compile_objects(args, build, inputs, includes, defines, flags)
    host_flags = [flag for flag in flags if flag != "/GL"]
    compile_objects(args, build, [layout_source], includes, defines, host_flags)
    layout = coff([build / "layout.obj"])
    values = struct.unpack_from(f"<{len(fields)}I", layout.nodes[layout.symbols["codec_layout"]].data)
    paths = [build / (source.stem + ".obj") for source in inputs]
    link_flags, driver_flags = [], []
    if "/GL" in flags:
        driver = build / "driver.c"
        driver.write_text(header + spec["driver"])
        driver_flags = host_flags
        compile_objects(args, build, [driver], includes, defines, driver_flags)
        link_flags = ["/LTCG", *LINK_FLAGS]
        image = build / "driver.exe"
        run(["link", "/nologo", *link_flags, "/OUT:driver.exe", "driver.obj", *[p.name for p in paths]], build)
        graph = pe(image, {p.name for p in paths}, spec.get("biased_symbols", {}))
    else:
        graph = coff(paths)
    family, detail = render(graph, spec, fields, values)
    return family, {**release, **detail, "flags": [*fixed_flags, *flags], "defines": defines,
                    "link_flags": link_flags, "driver_flags": driver_flags}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", choices=("msvc", "clang-cl", "mingw"), required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--library", choices=LIBRARIES)
    parser.add_argument("--version", help="one pinned release of --library")
    parser.add_argument("--flag", action="append", help="override matrix flags (use --flag=OPTION)")
    args = parser.parse_args()
    if args.version and not args.library:
        parser.error("--version requires --library")
    compiler_version = version_of(args.compiler)
    args.work = args.work.resolve()
    args.work.mkdir(parents=True, exist_ok=True)
    args.output.mkdir(parents=True, exist_ok=True)
    rows = []
    for library in [args.library] if args.library else LIBRARIES:
        for version in [args.version] if args.version else recipe(library)["versions"]:
            for flags in [args.flag] if args.flag else matrix_flags(args.compiler):
                key = sha256(str((compiler_version, library, version, flags)).encode()).hexdigest()[:12]
                build = args.work / "build" / key
                row = {"library": library, "version": version, "compiler": args.compiler,
                       "compiler_version": compiler_version, "flags": flags}
                try:
                    family, provenance = generate(args, library, version, flags, build)
                    identity = sha256("\n".join(line for line in family.splitlines()
                                                if not line.startswith("#")).encode()).hexdigest()[:16]
                    name = f"{library}-upstream-x86-{identity}.txt"
                    (args.output / name).write_text(family, encoding="utf-8", newline="\n")
                    row.update(provenance, family=name)
                    print(f"{library} {version} {' '.join(flags)} {name}", flush=True)
                except (KeyError, ValueError) as error:
                    row["failure"] = str(error)
                    print(f"{library} {version} {' '.join(flags)} {key}: {error}", flush=True)
                rows.append(row)
    (args.output / f"manifest-{args.compiler}-{compiler_version.split()[0]}.json").write_text(
        json.dumps(rows, indent=2) + "\n", encoding="utf-8", newline="\n"
    )
    return int(any("failure" in row for row in rows))


if __name__ == "__main__":
    raise SystemExit(main())
