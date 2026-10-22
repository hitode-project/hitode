#!/bin/sh
set -eu

: "${ADV_WINE_DIST:?Set ADV_WINE_DIST to the Wine installation}"
: "${ADV_WINE_PROBES:?Set ADV_WINE_PROBES to the probe directory}"

device=$(xcrun simctl list devices available --json | python3 -c '
import json, sys
devices = [device for runtime, group in json.load(sys.stdin)["devices"].items()
           if ".iOS-" in runtime for device in group]
devices.sort(key=lambda device: device["state"] != "Booted")
print(devices[0]["udid"])
')
xcrun simctl bootstatus "$device" -b

export SIMCTL_CHILD_ADV_WINE_DIST="$ADV_WINE_DIST"
export SIMCTL_CHILD_ADV_WINE_PROBES="$ADV_WINE_PROBES"
export SIMCTL_CHILD_ADV_AUDIO_MUTE_DEVICE=1
exec xcrun simctl spawn "$device" "$@"
