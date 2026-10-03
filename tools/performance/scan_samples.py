"""Read-only PE sample inventory. Requires the same dev-only pefile as tools/survey.

No image is loaded or executed. Import lists are demands, not measured API calls
or compatibility claims. The emitted samples list can feed sample_matrix.py.
"""
import argparse
from collections import Counter, defaultdict
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import sys

INSTALLER = re.compile(r"^(?:setup|uninst(?:all)?|unins\d*|dxsetup)(?:[_. -]|$)", re.I)
UTILITY = re.compile(r"^(?:config(?:_dl)?|filechecker|supporttools|curl|enc|sjistounicode|protect|.*update)(?:[_. -]|$)", re.I)
BACKUP = re.compile(r"备份|backup|全パッチ集|修正补丁|(?:^|[/\\])补丁(?:[/\\]|$)", re.I)
INJECT = {"CreateRemoteThread", "WriteProcessMemory", "ReadProcessMemory", "VirtualAllocEx", "VirtualProtectEx"}
ENGINE = re.compile(r"kirikiri|krkr|TVP\(|PJADV|SoftPAL|ADVプレイヤ|WillPlus|Artemis|Cat\s*System|EntisGLS|Siglus|RealLive|YU-RIS|Buriko|Ethornell|Majiro|Unity", re.I)
MEDIA = {"ddraw.dll", "d3d8.dll", "d3d9.dll", "d3d11.dll", "dsound.dll", "opengl32.dll", "xaudio2_7.dll", "xaudio2_9.dll"}
PE_EXTENSIONS = {".exe", ".dll", ".tpm", ".ax", ".ocx"}


def load_pefile(parser_path):
    if parser_path:
        sys.path.insert(0, str(parser_path))
    try:
        import pefile
        return pefile
    except ImportError:
        raise SystemExit("Install the dev-only pefile package used by tools/survey, or pass --parser-path PATH")


def text(value):
    if not isinstance(value, bytes):
        return str(value)
    for encoding in ("utf-8", "cp932", "cp1252"):
        try:
            return value.decode(encoding)
        except UnicodeError:
            pass
    return value.decode("utf-8", "replace")


def inspect(path, base, parser):
    row = {"path": str(path.resolve()), "relative": str(path.relative_to(base)),
           "size": path.stat().st_size}
    try:
        pe = parser.PE(str(path), fast_load=True)
        pe.parse_data_directories(directories=[0, 1, 2, 13])
        raw = pe.__data__
        row["sha256"] = hashlib.sha256(raw).hexdigest()
        row["architecture"] = {0x14c: "x86", 0x8664: "x64", 0xaa64: "arm64"}.get(pe.FILE_HEADER.Machine, hex(pe.FILE_HEADER.Machine))
        row["kind"] = "dll" if pe.FILE_HEADER.Characteristics & 0x2000 else "exe"
        row["subsystem"] = pe.OPTIONAL_HEADER.Subsystem
        row["image_base"] = pe.OPTIONAL_HEADER.ImageBase
        row["entry_rva"] = pe.OPTIONAL_HEADER.AddressOfEntryPoint
        row["sections"] = [{"name": text(s.Name.rstrip(b"\0")), "virtual_size": s.Misc_VirtualSize,
                            "raw_size": s.SizeOfRawData} for s in pe.sections]
        row["clr"] = bool(len(pe.OPTIONAL_HEADER.DATA_DIRECTORY) > 14 and pe.OPTIONAL_HEADER.DATA_DIRECTORY[14].VirtualAddress)
        row["imports"], row["delay_imports"] = {}, {}
        for attr, dest in [("DIRECTORY_ENTRY_IMPORT", row["imports"]), ("DIRECTORY_ENTRY_DELAY_IMPORT", row["delay_imports"])]:
            for module in getattr(pe, attr, []):
                dest[text(module.dll).lower()] = sorted({text(i.name) if i.name else f"#{i.ordinal}" for i in module.imports})
        row["delay_descriptors"] = [{"attributes": d.struct.grAttrs, "name_field": d.struct.szName,
                                      "iat_field": d.struct.pIAT, "int_field": d.struct.pINT}
                                     for d in getattr(pe, "DIRECTORY_ENTRY_DELAY_IMPORT", [])]
        row["import_count"] = sum(map(len, row["imports"].values())) + sum(map(len, row["delay_imports"].values()))
        exp = getattr(pe, "DIRECTORY_ENTRY_EXPORT", None)
        row["exports"] = [] if not exp else [{"name": text(s.name) if s.name else None, "ordinal": s.ordinal,
                                               "forwarder": text(s.forwarder) if s.forwarder else None} for s in exp.symbols]
        row["export_module"] = text(exp.name) if exp else None
        version, languages = {}, []
        for group in getattr(pe, "FileInfo", []):
            for info in group:
                for table in getattr(info, "StringTable", []):
                    languages.append(text(table.LangID))
                    version.update({text(k): text(v) for k, v in table.entries.items()})
        row["version"], row["version_languages"] = version, sorted(set(languages))
        blob = raw[:min(len(raw), 32 * 1024 * 1024)]
        strings = []
        for pattern, encoding in [(rb"[\x20-\x7e]{6,240}", "ascii"), (rb"(?:[\x20-\x7e]\x00){6,240}", "utf-16le")]:
            for m in re.finditer(pattern, blob):
                s = m.group().decode(encoding)
                if ENGINE.search(s) and s not in strings:
                    strings.append(s)
                    if len(strings) >= 30:
                        break
        row["engine_strings"] = strings[:30]
        row["dynamic_module_hints"] = sorted({m.group(1).decode("ascii").lower() for m in re.finditer(rb"(?i)(?<![a-z0-9_.-])([a-z][a-z0-9_.-]{1,70}\.(?:dll|tpm|ax))(?![a-z0-9_.-])", blob)})
        row["packing_hints"] = [s["name"] for s in row["sections"] if re.search(r"upx|enigma|aspack|\.vmp|\.themida", s["name"], re.I)]
        row["parser_warnings"] = pe.get_warnings()[:12]
        pe.close()
    except Exception as exc:
        row["error"] = str(exc)
    return row


def engine(row, companions):
    modules = set(row.get("imports", {})) | set(row.get("delay_imports", {}))
    version = row.get("version", {})
    identity = " ".join(version.values())
    evidence = []
    family, confidence = "unknown", "unknown"
    if "pal.dll" in modules:
        family, confidence = "SoftPAL", "direct"
        evidence.append({"kind": "import", "value": "pal.dll"})
    elif re.search(r"PJADV", identity, re.I):
        family, confidence = "PJADV", "direct"
    elif re.search(r"TVP\(KIRIKIRI\)\s*Z", identity, re.I):
        family, confidence = "Kirikiri Z", "direct"
    elif re.search(r"TVP\(KIRIKIRI\)|Kirikiri", identity, re.I):
        family, confidence = "Kirikiri 2", "direct"
    elif re.search(r"ADVプレイヤHD|AdvHD", identity, re.I):
        family, confidence = "WillPlus AdvHD", "direct"
    elif re.search(r"EntisGLS", identity, re.I):
        family, confidence = "EntisGLS", "direct"
    elif re.search(r"Artemis", identity, re.I):
        family, confidence = "Artemis", "direct"
    elif re.search(r"Cat\s*System", identity, re.I):
        family, confidence = "CatSystem", "direct"
    elif re.search(r"F&C", identity, re.I) and re.search(r"32\s*bits.*Game System", identity, re.I):
        family, confidence = "F&C Win32 Game System", "direct"
    if confidence == "direct":
        evidence += [{"kind": "version_resource", "field": k, "value": v} for k, v in version.items() if ENGINE.search(v)]
        if family == "F&C Win32 Game System":
            evidence += [{"kind": "version_resource", "field": k, "value": version[k]} for k in ("CompanyName", "ProductName", "LegalCopyright") if version.get(k)]
    elif any(re.search(r"Artemis Engine Rev\.", s) for s in row.get("engine_strings", [])):
        family, confidence = "Artemis", "direct_identity_string"
        evidence += [{"kind": "engine_banner", "value": s} for s in row["engine_strings"] if "Artemis Engine Rev." in s]
    elif any(re.search(r"Ethornell\s*-\s*BURIKO General Interpreter", s, re.I) for s in row.get("engine_strings", [])):
        family, confidence = "Ethornell BGI", "direct_identity_string"
        evidence += [{"kind": "engine_banner", "value": s} for s in row["engine_strings"]
                     if re.search(r"Ethornell\s*-\s*BURIKO General Interpreter", s, re.I)]
    elif any(re.search(r"\bMAJIRO\s*\(C\)|\bMAJIRO_RUNTIME\b", s, re.I) for s in row.get("engine_strings", [])):
        family, confidence = "Majiro", "direct_identity_string"
        evidence += [{"kind": "engine_identity", "value": s} for s in row["engine_strings"]
                     if re.search(r"MajiroObj[XV]|\bMAJIRO_RUNTIME\b|\bMAJIRO\s*\(C\)", s, re.I)]
    elif any(n.endswith(".xp3") for n in companions):
        family, confidence = "Kirikiri family (inferred)", "inferred"
        evidence.append({"kind": "adjacent_package", "value": "*.xp3"})
    elif any(n.endswith(".pfs") for n in companions):
        family, confidence = "Artemis family (inferred)", "inferred"
        evidence.append({"kind": "adjacent_package", "value": "*.pfs"})
    if confidence != "direct":
        evidence += [{"kind": "string_hint", "value": s} for s in row.get("engine_strings", [])[:8]]
    return {"family": family, "confidence": confidence, "evidence": evidence}


def classify(row, base):
    if "error" in row:
        return "invalid_pe", [row["error"]]
    if row["kind"] != "exe":
        return "component", []
    filename = Path(row["path"]).name
    identity = " ".join(row["version"].values())
    if INSTALLER.search(filename) or re.search(r"Inno Setup|NSIS|Uninstall|InstallShield", identity, re.I):
        return "installer_uninstaller", ["utility filename or version identity"]
    if re.search(r"Digital signature/file integrity|Update for Windows|自己展開|DWiff", identity, re.I):
        return "utility_or_patch_package", ["version identifies checker/updater/self-extracting package"]
    if row["engine"]["confidence"] == "direct":
        return "game", []
    functions = {f for names in [*row["imports"].values(), *row["delay_imports"].values()] for f in names}
    if functions & INJECT and functions & {"CreateProcessA", "CreateProcessW"}:
        return "launcher_injector", ["CreateProcess plus process-memory/injection imports", *sorted(functions & INJECT)]
    if {"NtWriteVirtualMemory", "NtSetContextThread", "NtResumeThread"} <= functions:
        return "launcher_injector", ["NT process-memory/context/resume imports; actual child creation needs tracing"]
    if re.search(r"汉化补丁", identity, re.I) and "CreateDirectoryA" in functions and not (set(row["imports"]) & MEDIA):
        return "utility_or_patch_package", ["patch-package identity plus file-install imports; no directly identified engine core"]
    if UTILITY.search(filename) or BACKUP.search(row["relative"]):
        return "utility_or_backup", ["generic utility/backup path"]
    # Sparse imports are retained as candidates, not mislabeled as simple APIs.
    if len(Path(row["relative"]).parts) == 1 and row["subsystem"] == 2:
        return "candidate_game", ["root GUI executable; engine identity not directly established"]
    return "auxiliary", ["nested or non-GUI executable without direct engine identity"]


def codec_hint(row):
    # Resource language/filename are launch hints, not proof of ANSI text encoding.
    name = Path(row["path"]).stem
    chinese = bool(re.search(r"(?:^|[_ -])(?:chs|cn|cht|chatgpt\w*)(?:$|[_ -])", name, re.I))
    if chinese:
        return 936, {"confidence": "inferred", "basis": "Chinese patch naming convention; verify guest text at runtime"}
    return 932, {"confidence": "default", "basis": "Japanese ADV launch default; PE version resource UTF-16 codepage is not an ANSI codepage"}


def scan(root, parser):
    groups = []
    for directory in sorted((p for p in root.iterdir() if p.is_dir()), key=lambda p: p.name.casefold()):
        # Retain binary backups so they can be explicitly excluded rather than
        # disappearing from the audit.
        paths = [p for p in directory.rglob("*") if p.is_file() and p.suffix.lower() in PE_EXTENSIONS]
        companions = sorted(p.name.lower() for p in directory.iterdir() if p.is_file())
        images = [inspect(p, directory, parser) for p in sorted(paths, key=lambda p: str(p).casefold())]
        for image in images:
            image["engine"] = engine(image, companions)
            image["role"], image["role_evidence"] = classify(image, directory)
            image["codepage"], image["codepage_evidence"] = codec_hint(image)
            if image.get("kind") == "exe":
                image["scope"] = "x86-win32" if image.get("architecture") == "x86" and not image.get("clr") else "outside-current-x86-scope"
        groups.append({"name": directory.name, "directory": str(directory.resolve()), "top_level_files": companions, "images": images})
    all_images = [image for group in groups for image in group["images"]]
    hashes = defaultdict(list)
    for image in all_images:
        if "sha256" in image:
            hashes[image["sha256"]].append(image)
    duplicates = []
    for digest, images in hashes.items():
        if len(images) > 1:
            duplicates.append({"sha256": digest, "paths": [i["path"] for i in images]})
            for image in images:
                image["duplicate_paths"] = [i["path"] for i in images if i is not image]
    samples, wrappers, seen = [], [], set()
    for group in groups:
        candidates = [r for r in group["images"] if r["role"] in ("game", "candidate_game") and r.get("scope") == "x86-win32" and not BACKUP.search(r["relative"])]
        # Original filename and root location rank native cores ahead of copied
        # backups or injectors. Both original and modified cores remain in JSON.
        candidates.sort(key=lambda r: (r["engine"]["confidence"] != "direct",
                                      len(Path(r["relative"]).parts), r["codepage"] != 932,
                                      Path(r["path"]).name.casefold() != r["version"].get("OriginalFilename", "").casefold(),
                                      -r["import_count"], r["relative"].casefold()))
        selected = None
        for image in candidates:
            if image["sha256"] in seen:
                continue
            seen.add(image["sha256"])
            selected = image
            slug = re.sub(r"[^a-z0-9]+", "-", image["engine"]["family"].lower()).strip("-")
            samples.append({"id": f"{slug}-{image['sha256'][:10]}", "exe": image["path"],
                            "engine": image["engine"]["family"], "engine_evidence": image["engine"],
                            "codepage": image["codepage"], "codepage_evidence": image["codepage_evidence"],
                            "game_directory": group["directory"], "sha256": image["sha256"],
                            "static_imports": image["import_count"], "packing_hints": image["packing_hints"],
                            "selection_basis": "ranked primary x86 core; runtime startup still untested"})
            break
        group["selected_exe"] = selected["path"] if selected else None
        group["candidate_executables"] = [r["path"] for r in candidates]
        wrappers += [{"exe": r["path"], "engine": r["engine"], "reason": r["role_evidence"]}
                     for r in group["images"] if r["role"] == "launcher_injector"]
    representatives = []
    by_family = defaultdict(list)
    for sample in samples:
        by_family[sample["engine"]].append(sample)
    for family, entries in by_family.items():
        representative = min(entries, key=lambda s: (bool(s["packing_hints"]), s["static_imports"], s["id"]))
        representatives.append({"engine": family, "sample_id": representative["id"],
                                "exe": representative["exe"], "basis": "one unpacked representative per static family; import count is only a tie-break, not runtime cost"})
    return {"schema_version": 1, "generated_utc": datetime.now(timezone.utc).isoformat(),
            "root": str(root.resolve()), "method": "Read-only static PE headers/imports/delay imports/exports/version resources/bounded strings. No native or emulated execution.",
            "limitations": ["Import presence is not a runtime call or startup/playability result", "DLL dynamic-name strings are hints, not established dependencies", "Packing or sparse imports can hide later demands", "ANSI codepages are launch proposals, not verified by Unicode resource language", "Adjacent package layout is inference and is never a compatibility switch"],
            "samples": samples, "family_representatives": representatives,
            "launcher_candidates": wrappers, "groups": groups,
            "all_executables": [r for r in all_images if Path(r["path"]).suffix.lower() == ".exe"],
            "duplicates": duplicates, "counts": {"directories": len(groups), "candidate_files": len(all_images),
            "PE_images": sum(r["role"] != "invalid_pe" for r in all_images),
            "executables": sum(Path(r["path"]).suffix.lower() == ".exe" for r in all_images),
            "selected_samples": len(samples), "roles": dict(Counter(r["role"] for r in all_images))}}


def summary(inventory):
    lines = ["Read-only cross-engine sample inventory", inventory["method"], "", str(inventory["counts"]), "", "Selected x86 cores (startup is untested):"]
    for sample in inventory["samples"]:
        lines.append(f"- {sample['engine']} [{sample['engine_evidence']['confidence']}] cp={sample['codepage']} ({sample['codepage_evidence']['confidence']}): {sample['exe']}")
        lines += [f"    {e['kind']}: {e.get('field', '')} {e['value']}" for e in sample["engine_evidence"]["evidence"][:3]]
        if sample["packing_hints"]:
            lines.append(f"    packing hints: {sample['packing_hints']}; import count is not a cost estimate")
    lines += ["", "Launch/injection shells retained separately:"]
    lines += [f"- {s['exe']}: {'; '.join(s['reason'])}" for s in inventory["launcher_candidates"]]
    lines += ["", "Excluded/nonselected EXEs:"]
    chosen = {s["exe"] for s in inventory["samples"]}
    lines += [f"- {r['path']}: {r['role']} / {r.get('scope', 'invalid')}" for r in inventory["all_executables"] if r["path"] not in chosen]
    lines += ["", *inventory["limitations"]]
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("root", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--parser-path", type=Path)
    args = p.parse_args()
    if args.output.resolve().is_relative_to(args.root.resolve()):
        p.error("--output must stay outside the read-only sample root")
    result = scan(args.root.resolve(), load_pefile(args.parser_path))
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "inventory.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    (args.output / "inventory.txt").write_text(summary(result), encoding="utf-8")
    print(json.dumps({"counts": result["counts"], "samples": [{k: s[k] for k in ("id", "exe", "engine", "codepage")} for s in result["samples"]]}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
