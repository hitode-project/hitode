#!/usr/bin/env python3
"""Add TrollStore's JIT entitlement (IOS-JIT-3) to a copy of an unsigned Hitode IPA."""

import argparse
import plistlib
from pathlib import Path
import subprocess
import tempfile


def package(source: Path, output: Path) -> None:
    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        root = Path(temporary)
        subprocess.run(['ditto', '-x', '-k', source, root], check=True)
        app = root / 'Payload/Hitode.app'
        with (app / 'Info.plist').open('rb') as info:
            executable = plistlib.load(info)['CFBundleExecutable']
        entitlements = Path(__file__).with_name('trollstore.entitlements')
        subprocess.run(['ldid', f'-S{entitlements}', app / executable], check=True)
        staged = root / output.name
        subprocess.run(['ditto', '-c', '-k', '--keepParent', root / 'Payload', staged], check=True)
        staged.replace(output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path, help='unsigned Hitode IPA')
    parser.add_argument('output', type=Path, help='TrollStore IPA')
    args = parser.parse_args()
    package(args.source, args.output)
