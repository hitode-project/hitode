"""Disassemble code around call sites of chosen imports in one PE file.

Usage: python callsites.py <pe> <Import>[,<Import>...] [before=48] [after=32]
Output is for local analysis only; do not commit it.
"""
import sys

import capstone
import pefile


def main():
    path, names = sys.argv[1], set(sys.argv[2].split(','))
    before = int(sys.argv[3]) if len(sys.argv) > 3 else 48
    after = int(sys.argv[4]) if len(sys.argv) > 4 else 32
    pe = pefile.PE(path)
    base = pe.OPTIONAL_HEADER.ImageBase
    iat = {}
    for d in getattr(pe, 'DIRECTORY_ENTRY_IMPORT', []):
        for i in d.imports:
            if i.name and i.name.decode() in names:
                iat[i.address] = i.name.decode()
    text = next(s for s in pe.sections if s.Characteristics & 0x20000000)
    code, va0 = text.get_data(), base + text.VirtualAddress
    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32)
    for off in range(len(code) - 6):
        # call dword ptr [abs]
        if code[off] == 0xFF and code[off + 1] == 0x15:
            target = int.from_bytes(code[off + 2:off + 6], 'little')
            if target in iat:
                start = max(0, off - before)
                print(f'--- call {iat[target]} at {va0 + off:#x}')
                for insn in md.disasm(code[start:off + 6 + after], va0 + start):
                    mark = '>>' if insn.address == va0 + off else '  '
                    print(f'{mark} {insn.address:#x}: {insn.mnemonic} {insn.op_str}')
    # Also report indirect calls through a register loaded from the IAT.
    for off in range(len(code) - 6):
        if code[off] == 0x8B and code[off + 1] in (0x35, 0x3D, 0x1D, 0x2D, 0x15, 0x0D, 0x05):
            target = int.from_bytes(code[off + 2:off + 6], 'little')
            if target in iat:
                print(f'--- {iat[target]} loaded into register at {va0 + off:#x}')


if __name__ == '__main__':
    main()
