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
    env -i PATH=/usr/bin:/bin LC_CTYPE=C.UTF-8 USER=Player HOME="$work/home" WINEPREFIX="$work/prefix" \
        WINEDEBUG=-all WINEDLLOVERRIDES=mscoree,mshtml= "$install/bin/wine" "$@"
}
wine wineboot --init
# Register the media filters and transforms, including Wine's filter mapper entries.
wine regsvr32 /s winegstreamer.dll
wine reg add 'HKCU\Software\Wine\Drivers' /v Audio /d advaudio /f >/dev/null
# The fonts Windows ships stand for the host's CJK fonts (families as their
# name tables give them, weight included): win32u takes the
# first family of each list that the host has (macOS and iOS, then Android).
replace() {
    face=$1
    shift
    families=$(printf '%s\\0' "$@")
    wine reg add 'HKCU\Software\Wine\Fonts\Replacements' /v "$face" /t REG_MULTI_SZ \
        /d "${families%\\0}" /f >/dev/null
}
for face in 'MS Gothic' 'ＭＳ ゴシック' 'MS PGothic' 'ＭＳ Ｐゴシック' 'MS UI Gothic' \
    'Meiryo' 'メイリオ' 'Meiryo UI' 'Yu Gothic' '游ゴシック'; do
    replace "$face" 'Hiragino Sans W3' 'Hiragino Kaku Gothic ProN W3' 'Noto Sans CJK JP'
done
for face in 'MS Mincho' 'ＭＳ 明朝' 'MS PMincho' 'ＭＳ Ｐ明朝' 'Yu Mincho' '游明朝'; do
    replace "$face" 'Hiragino Mincho ProN W3' 'Noto Serif CJK JP' 'Noto Sans CJK JP'
done
for face in 'SimHei' '黑体' 'Microsoft YaHei' '微软雅黑'; do
    replace "$face" 'PingFang SC' 'Hiragino Sans GB W3' 'Noto Sans CJK SC'
done
for face in 'SimSun' '宋体' 'NSimSun' '新宋体'; do
    replace "$face" 'Songti SC' 'Hiragino Sans GB W3' 'Noto Serif CJK SC' 'Noto Sans CJK SC'
done
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
# Sessions link system32's top-level modules; wine.inf also installs subdirectories.
find drive_c -path drive_c/windows/syswow64 -prune \
    -o -path drive_c/windows/globalization -prune -o -type d -print |
    while IFS= read -r directory; do mkdir -p "$output/$directory"; done
find drive_c -path 'drive_c/windows/system32/*' ! -path 'drive_c/windows/system32/*/*' ! -type d -prune \
    -o -path drive_c/windows/syswow64 -prune \
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
