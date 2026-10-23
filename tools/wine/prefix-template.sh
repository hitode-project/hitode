#!/bin/sh
# Build the prefix template that Wine sessions start from: what Wine's own
# `wineboot --init` makes of an empty prefix, run with the Wine installation
# that sessions use.
#
#   tools/wine/prefix-template.sh <install> <output>
#
# <install> is the Wine installation (`bin/wine`, `lib/wine`, `share/wine`);
# the template goes in <output>, normally <install>/share/hitode/prefix. Run it
# where that Wine runs (Linux), headless.
#
# The template holds the registry files and the files wine.inf installs in
# drive_c. A file that is a copy of one of the installation's modules is not
# kept: `links.tsv` names the module, and a session links it, as it links
# system32. Values naming this machine or the installation's location are
# dropped; Wine writes them again when a session starts.
set -eu

install=$(cd "$1" && pwd)
output=$2
modules=$install/lib/wine/i386-windows
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/home"

wine() {
    env -i PATH=/usr/bin:/bin USER=Player HOME="$work/home" WINEPREFIX="$work/prefix" \
        WINEDEBUG=-all WINEDLLOVERRIDES=mscoree,mshtml= "$install/bin/wine" "$@"
}
wine wineboot --init
wine reg add 'HKCU\Software\Wine\Drivers' /v Audio /d advaudio /f >/dev/null
env -i PATH=/usr/bin:/bin WINEPREFIX="$work/prefix" "$install/bin/wineserver" --wait

rm -rf "$output"
mkdir -p "$output"
# Wine's drive for the host's root, which only font paths use.
host_root='="Z:\\\\' # privacy: allow the values naming it are dropped
for file in system.reg user.reg userdef.reg; do
    # Font paths in the installation and the build machine's host name.
    grep -v -e "$host_root" -e '^"Hostname"=' -e '^"NV Hostname"=' \
        "$work/prefix/$file" >"$output/$file"
done

cd "$work/prefix"
: >"$output/links.tsv"
find drive_c -path drive_c/windows/system32 -prune -o -path drive_c/windows/syswow64 -prune \
    -o -path drive_c/windows/globalization -prune -o -type d -print |
    while IFS= read -r directory; do mkdir -p "$output/$directory"; done
find drive_c -path drive_c/windows/system32 -prune -o -path drive_c/windows/syswow64 -prune \
    -o -path drive_c/windows/globalization -prune -o -type f -print | sort |
    while IFS= read -r file; do
        name=$(basename "$file" | tr '[:upper:]' '[:lower:]')
        module=
        for candidate in "$modules/$name" "$modules/${name%.dll}_v6.dll"; do
            if [ -f "$candidate" ] && cmp -s "$file" "$candidate"; then
                module=$(basename "$candidate")
                break
            fi
        done
        if [ -n "$module" ]; then
            printf '%s\t%s\n' "$file" "$module" >>"$output/links.tsv"
        else
            cp "$file" "$output/$file"
        fi
    done
