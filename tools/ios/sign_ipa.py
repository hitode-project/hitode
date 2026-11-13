#!/usr/bin/env python3
"""Ad-hoc sign a copy of an unsigned Hitode IPA with the app's entitlements.

The signature names no signer; it carries the entitlements that installers
re-signing the app enable on their own App ID, such as the memory extension
(IOS-MEM-4). --trollstore adds TrollStore's JIT entitlement (IOS-JIT-3).
"""

import argparse
import plistlib
from pathlib import Path
import subprocess
import tempfile

ENTITLEMENTS = Path(__file__).resolve().parents[2] / (
    'apps/adv-launcher/src-tauri/gen/apple/adv-launcher_iOS/adv-launcher_iOS.entitlements')


def sign(source: Path, output: Path, trollstore: bool) -> None:
    with ENTITLEMENTS.open('rb') as file:
        entitlements = plistlib.load(file)
    if trollstore:
        entitlements['get-task-allow'] = True
    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        root = Path(temporary)
        subprocess.run(['ditto', '-x', '-k', source, root], check=True)
        app = root / 'Payload/Hitode.app'
        with (app / 'Info.plist').open('rb') as info:
            executable = app / plistlib.load(info)['CFBundleExecutable']
        declared = root / 'entitlements.plist'
        declared.write_bytes(plistlib.dumps(entitlements))
        subprocess.run(['ldid', f'-S{declared}', executable], check=True)
        signed = subprocess.run(['ldid', '-e', executable], check=True, capture_output=True).stdout
        if plistlib.loads(signed) != entitlements:
            raise SystemExit(f'{executable.name} does not carry the entitlements')
        staged = root / output.name
        subprocess.run(['ditto', '-c', '-k', '--keepParent', root / 'Payload', staged], check=True)
        staged.replace(output)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path, help='unsigned Hitode IPA')
    parser.add_argument('output', type=Path, help='signed IPA')
    parser.add_argument('--trollstore', action='store_true', help='add the JIT entitlement for TrollStore')
    args = parser.parse_args()
    sign(args.source, args.output, args.trollstore)
