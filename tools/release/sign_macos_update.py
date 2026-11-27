#!/usr/bin/env python3
"""Sign or verify a Hitode macOS in-app update archive with Ed25519.

The signature covers a domain-separated manifest tying the GitHub release tag,
the archive's SHA-256 digest and its exact byte length:

    hitode-macos-update-v1\n
    <tag>\n
    <sha256-hex-lowercase>\n
    <byte-size>\n

Uses Python `cryptography` when installed and falls back to OpenSSL 3's
`pkeyutl -rawin` so CI runners need no extra Python packages.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import os
import subprocess
import sys
import tempfile
from pathlib import Path

DOMAIN = "hitode-macos-update-v1"
# Must match UPDATE_PUBLIC_KEY in apps/adv-launcher/src-tauri/src/update.rs.
DEFAULT_PUBLIC_KEY_B64 = "Uv0X75R0ILj3sPpWLo1NfgPRapsWUmsAUEM2q3ydf64="
# DER SubjectPublicKeyInfo prefix for a 32-byte raw Ed25519 public key.
ED25519_SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def archive_payload(tag: str, archive: Path) -> bytes:
    digest = hashlib.sha256()
    size = 0
    with archive.open("rb") as handle:
        while chunk := handle.read(1 << 20):
            digest.update(chunk)
            size += len(chunk)
    return f"{DOMAIN}\n{tag}\n{digest.hexdigest()}\n{size}\n".encode("ascii")


def sign_bytes(payload: bytes, key_pem: bytes) -> bytes:
    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        from cryptography.hazmat.primitives.serialization import load_pem_private_key

        key = load_pem_private_key(key_pem, password=None)
        if not isinstance(key, Ed25519PrivateKey):
            raise ValueError("private key is not Ed25519")
        return key.sign(payload)
    except ImportError:
        pass

    with tempfile.TemporaryDirectory() as tmp:
        key_path = Path(tmp) / "key.pem"
        msg_path = Path(tmp) / "msg.bin"
        sig_path = Path(tmp) / "sig.bin"
        fd = os.open(key_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(key_pem)
        msg_path.write_bytes(payload)
        subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-sign",
                "-rawin",
                "-inkey",
                str(key_path),
                "-in",
                str(msg_path),
                "-out",
                str(sig_path),
            ],
            check=True,
        )
        return sig_path.read_bytes()


def verify_bytes(payload: bytes, signature: bytes, public_key_raw: bytes) -> None:
    if len(public_key_raw) != 32:
        raise ValueError("Ed25519 public key must be 32 bytes")
    if len(signature) != 64:
        raise ValueError("Ed25519 signature must be 64 bytes")

    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

        Ed25519PublicKey.from_public_bytes(public_key_raw).verify(signature, payload)
        return
    except ImportError:
        pass

    spki_pem = (
        b"-----BEGIN PUBLIC KEY-----\n"
        + base64.encodebytes(ED25519_SPKI_PREFIX + public_key_raw)
        + b"-----END PUBLIC KEY-----\n"
    )
    with tempfile.TemporaryDirectory() as tmp:
        pub_path = Path(tmp) / "pub.pem"
        msg_path = Path(tmp) / "msg.bin"
        sig_path = Path(tmp) / "sig.bin"
        pub_path.write_bytes(spki_pem)
        msg_path.write_bytes(payload)
        sig_path.write_bytes(signature)
        subprocess.run(
            [
                "openssl",
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                str(pub_path),
                "-in",
                str(msg_path),
                "-sigfile",
                str(sig_path),
            ],
            check=True,
            stdout=subprocess.DEVNULL,
        )


def read_key_pem(args: argparse.Namespace) -> bytes:
    if args.key:
        return Path(args.key).read_bytes()
    if args.key_env:
        value = os.environ.get(args.key_env, "").strip()
        if not value:
            raise SystemExit(f"environment variable {args.key_env} is empty")
        if "BEGIN " in value:
            return (value + "\n").encode("utf-8")
        return base64.b64decode(value)
    raise SystemExit("pass --key or --key-env when signing")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Release tag, e.g. v0.3.1")
    parser.add_argument("--archive", required=True, help="Path to Hitode-*-macos-arm64.app.tar.gz")
    parser.add_argument("--sig", required=True, help="Signature file path (.sig)")
    parser.add_argument("--key", help="Path to Ed25519 PKCS#8 PEM private key")
    parser.add_argument("--key-env", help="Env var holding the PEM (or base64 PEM) private key")
    parser.add_argument("--verify", action="store_true", help="Verify an existing signature")
    parser.add_argument(
        "--public-key-b64",
        default=DEFAULT_PUBLIC_KEY_B64,
        help="Base64 32-byte raw Ed25519 public key for verification",
    )
    args = parser.parse_args()

    archive = Path(args.archive)
    sig_path = Path(args.sig)
    payload = archive_payload(args.tag, archive)
    public_key = base64.b64decode(args.public_key_b64)

    if args.verify:
        signature = base64.b64decode(sig_path.read_text(encoding="ascii").strip())
        verify_bytes(payload, signature, public_key)
        print(f"verified {sig_path.name} for {args.tag}")
        return 0

    key_pem = read_key_pem(args)
    signature = sign_bytes(payload, key_pem)
    verify_bytes(payload, signature, public_key)
    sig_path.write_text(base64.b64encode(signature).decode("ascii") + "\n", encoding="ascii")
    print(f"signed and verified {sig_path.name} for {args.tag}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
