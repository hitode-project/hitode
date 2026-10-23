#!/usr/bin/env python3
"""Generate the Rust form of the wineserver requests adv-nt sends.

  python3 tools/wine/protocol.py          # rewrite crates/adv-nt/src/protocol.rs
  python3 tools/wine/protocol.py --check  # exit 1 if the file is out of date

Field names and types come from the vendored server_protocol.h. Offsets and
sizes come from the C_ASSERTs in server/request_handlers.h, which the server
build checks, so every generated layout matches the server's by
construction. A field whose type is not a plain integer becomes a byte array
that spans it. Only the requests in REQUESTS are generated; add a name when
adv-nt starts sending it.
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
WINE = ROOT / "vendor/wine"
PROTOCOL = WINE / "include/wine/server_protocol.h"
HANDLERS = WINE / "server/request_handlers.h"
OUTPUT = ROOT / "crates/adv-nt/src/protocol.rs"

REQUESTS = [
    "init_first_thread",
    "init_process_done",
    "terminate_process",
    "create_file",
    "open_file_object",
    "get_file_info",
    "get_handle_unix_name",
    "set_fd_eof_info",
    "set_fd_disp_info",
    "set_fd_name_info",
    "create_named_pipe",
    "read",
    "write",
    "add_fd_completion",
    "alloc_file_handle",
    "get_handle_fd",
    "close_handle",
    "create_mapping",
    "get_mapping_info",
    "map_image_view",
    "map_view",
    "is_same_mapping",
    "unmap_view",
    "open_key",
    "create_key",
    "enum_key",
    "get_key_value",
    "set_key_value",
    "get_token_sid",
    "open_directory",
    "open_mapping",
    "open_process",
    "open_thread",
    "open_token",
    "get_process_info",
    "get_next_thread",
    "get_thread_info",
    "get_thread_times",
    "select",
    "create_event",
    "event_op",
    "new_thread",
    "init_thread",
    "suspend_thread",
    "resume_thread",
    "terminate_thread",
    "queue_apc",
    "delete_key",
    "delete_key_value",
    "enum_key_value",
    "create_mutex",
    "release_mutex",
    "create_semaphore",
    "open_semaphore",
    "query_semaphore",
    "release_semaphore",
    "create_timer",
    "open_timer",
    "set_timer",
    "cancel_timer",
    "get_timer_info",
    "add_atom",
    "delete_atom",
    "find_atom",
    "get_atom_information",
    "allocate_locally_unique_id",
    "dup_handle",
    "get_object_info",
    "get_object_name",
    "get_object_type",
    "set_queue_fd",
]

INTEGERS = {
    "int": "i32", "unsigned int": "u32", "short": "i16", "unsigned short": "u16",
    "char": "i8", "unsigned char": "u8", "__int64": "i64", "unsigned __int64": "u64",
}


def typedefs(text: str) -> dict[str, str]:
    """Integer typedefs, resolved to Rust types."""
    types = dict(INTEGERS)
    for base, name in re.findall(r"^typedef ([\w ]+?) (\w+);", text, re.M):
        if base in types:
            types[name] = types[base]
    return types


def fields(text: str, struct: str) -> list[tuple[str, str]]:
    body = re.search(rf"^struct {struct}\n{{\n(.*?)^}};", text, re.M | re.S)
    if not body:
        sys.exit(f"struct {struct} not found in {PROTOCOL.name}")
    found = []
    for line in body.group(1).splitlines():
        match = re.match(r"\s+(\w[\w ]*?)\s+(\w+)(\[\d+\])?;", line)
        if match and not match.group(2).startswith("__"):
            found.append((match.group(2), match.group(1)))
    return found


def layout(handlers: str, struct: str, header_size: int) -> tuple[dict[str, int], int]:
    offsets = {name: int(offset) for name, offset in re.findall(
        rf"C_ASSERT\( offsetof\(struct {struct}, (\w+)\) == (\d+) \);", handlers)}
    size = re.search(rf"C_ASSERT\( sizeof\(struct {struct}\) == (\d+) \);", handlers)
    if size:
        return offsets, int(size.group(1))
    # The server checks no size of a structure that is only its header.
    if offsets:
        sys.exit(f"no size of struct {struct} in {HANDLERS.name}")
    return offsets, header_size


KEYWORDS = {"async", "type", "match", "ref", "move", "loop", "box", "fn", "mod", "use", "where", "impl"}


# Keywords that cannot be raw identifiers take a trailing underscore.
UNRAW = {"self", "super", "crate", "Self"}


def ident(name: str) -> str:
    if name in UNRAW:
        return f"{name}_"
    return f"r#{name}" if name in KEYWORDS else name


def camel(name: str) -> str:
    return "".join(part.capitalize() for part in name.split("_"))


def rust_struct(protocol: str, handlers: str, types: dict[str, str], name: str, kind: str) -> list[str]:
    struct = f"{name}_{kind}"
    header, header_size = ("RequestHeader", 12) if kind == "request" else ("ReplyHeader", 8)
    offsets, size = layout(handlers, struct, header_size)
    members = [(field, ctype, offsets[field]) for field, ctype in fields(protocol, struct)]
    lines = ["#[repr(C)]", "#[derive(Clone, Copy, Debug, Default)]",
             f"pub struct {camel(name)}{kind.capitalize()} {{", f"    pub header: {header},"]
    defaults = ["header: Default::default()"]
    large_array = False
    position, pad = header_size, 0
    for index, (field, ctype, offset) in enumerate(members):
        if offset > position:
            lines.append(f"    pub _pad{pad}: [u8; {offset - position}],")
            defaults.append(f"_pad{pad}: [0; {offset - position}]")
            pad += 1
        end = members[index + 1][2] if index + 1 < len(members) else size
        rust = types.get(ctype)
        width = int(rust[1:]) // 8 if rust else end - offset
        lines.append(f"    pub {ident(field)}: {rust or f'[u8; {width}]'},")
        defaults.append(f"{ident(field)}: " + ("0" if rust else f"[0; {width}]"))
        large_array |= not rust and width > 32
        position = offset + width
    if size > position:
        lines.append(f"    pub _pad{pad}: [u8; {size - position}],")
        defaults.append(f"_pad{pad}: [0; {size - position}]")
    lines.append("}")
    if large_array:
        lines[1] = "#[derive(Clone, Copy, Debug)]"
        lines += [f"impl Default for {camel(name)}{kind.capitalize()} {{",
                  "    fn default() -> Self {",
                  "        Self { " + ", ".join(defaults) + " }", "    }", "}"]
    lines.append(f"const _: () = assert!(size_of::<{camel(name)}{kind.capitalize()}>() == {size});")
    for field, _, offset in members:
        lines.append(
            f"const _: () = assert!(std::mem::offset_of!({camel(name)}{kind.capitalize()}, {ident(field)}) == {offset});")
    return lines


def generate() -> str:
    protocol = PROTOCOL.read_text()
    handlers = HANDLERS.read_text()
    types = typedefs(protocol)
    numbers = re.search(r"^enum request\n{\n(.*?)^};", protocol, re.M | re.S).group(1)
    order = re.findall(r"REQ_(\w+),", numbers)
    version = re.search(r"#define SERVER_PROTOCOL_VERSION (\d+)", protocol).group(1)
    out = [
        "//! wineserver request layouts. Generated by tools/wine/protocol.py; do not edit.",
        "#![allow(dead_code)]",
        "",
        f"pub const PROTOCOL_VERSION: u32 = {version};",
        "",
        "#[repr(C)]",
        "#[derive(Clone, Copy, Debug, Default)]",
        "pub struct RequestHeader {",
        "    pub req: i32,",
        "    pub request_size: u32,",
        "    pub reply_size: u32,",
        "}",
        "",
        "#[repr(C)]",
        "#[derive(Clone, Copy, Debug, Default)]",
        "pub struct ReplyHeader {",
        "    pub error: u32,",
        "    pub reply_size: u32,",
        "}",
        "",
        "/// Every request and reply is sent in a buffer of this size, followed by",
        "/// its variable-length data.",
        "pub const FIXED_SIZE: usize = 64;",
        "",
        "/// A fixed request part; `NUMBER` is its index in the server's table.",
        "pub trait Request: Copy + Default {",
        "    const NUMBER: i32;",
        "    type Reply: Copy + Default;",
        "    fn header(&mut self) -> &mut RequestHeader;",
        "}",
    ]
    for name in REQUESTS:
        out.append("")
        out += rust_struct(protocol, handlers, types, name, "request")
        out.append("")
        out += rust_struct(protocol, handlers, types, name, "reply")
        out += [
            "",
            f"impl Request for {camel(name)}Request {{",
            f"    const NUMBER: i32 = {order.index(name)};",
            f"    type Reply = {camel(name)}Reply;",
            "    fn header(&mut self) -> &mut RequestHeader {",
            "        &mut self.header",
            "    }",
            "}",
        ]
    source = "\n".join(out) + "\n"
    formatted = subprocess.run(["rustfmt", "--edition", "2024", "--emit", "stdout"],
                               input=source.encode(), capture_output=True, check=True)
    return formatted.stdout.decode()


def main(argv: list[str]) -> int:
    source = generate()
    if "--check" in argv:
        if not OUTPUT.exists() or OUTPUT.read_text() != source:
            print(f"{OUTPUT.relative_to(ROOT)} is out of date: run python3 tools/wine/protocol.py")
            return 1
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(source)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
