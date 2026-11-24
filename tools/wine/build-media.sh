#!/bin/sh
# Wine 11.0, i386 with scalar SSE: PE media side plus a decode-free Unix build.
# Usage: build-media.sh SOURCE BUILD DIST STRIPPED_DIST
# SOURCE gets tools/wine/patches applied once. Existing builds retain every
# installed binary except the modules listed in $modules.
set -eu
tools=$(cd "$(dirname "$0")" && pwd)
source=$(cd "$1" && pwd)
mkdir -p "$2" "$3" "$4"
build=$(cd "$2" && pwd)
dist=$(cd "$3" && pwd)
stripped=$(cd "$4" && pwd)
for patch in "$tools"/patches/*.patch; do
    patch -d "$source" -p1 -R -s -f --dry-run < "$patch" >/dev/null 2>&1 ||
        patch -d "$source" -p1 -s < "$patch"
done
# winegstreamer is the added PE module; kernel32, dwrite and ntdll change with the patches.
modules="winegstreamer kernel32 dwrite ntdll"
cd "$build"
if [ ! -f Makefile ]; then
    # A relative source path keeps the builder's directories out of __FILE__.
    relative=$(python3 -c 'import os, sys; print(os.path.relpath(*sys.argv[1:]))' "$source" "$build")
    PKG_CONFIG_LIBDIR=/usr/lib/i386-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    "$relative/configure" --host=i686-linux-gnu CC='gcc -m32' \
        CFLAGS='-g -O2 -msse2 -mfpmath=sse -fno-tree-vectorize' \
        CROSSCFLAGS='-g -O2 -msse2 -mfpmath=sse -fno-tree-vectorize' \
        --enable-archs=i386 --with-freetype --with-mingw \
        --without-x --without-wayland --without-pulse --without-alsa --without-oss \
        --without-gstreamer --without-ffmpeg --without-cups --without-dbus \
        --without-sane --without-gphoto --without-v4l2 --without-pcap --without-usb \
        --without-udev --without-unwind --without-krb5 --without-gssapi \
        --without-netapi --without-opencl --without-osmesa --without-vulkan \
        --without-sdl --without-capi --without-xinerama --without-fontconfig \
        --without-gnutls --without-inotify --without-pcsclite --without-smbios
    make -j"$(nproc)"
    make -j"$(nproc)" install-lib DESTDIR="$dist"
fi
PKG_CONFIG_LIBDIR=/usr/lib/i386-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    enable_winegstreamer=yes ./config.status --recheck
./config.status
# Do not build winegstreamer.so; its interface is supplied by the host.
for name in $modules; do
    make -j"$(nproc)" "dlls/$name/i386-windows/$name.dll"
    module=usr/local/lib/wine/i386-windows/$name.dll
    mkdir -p "$dist/$(dirname "$module")"
    cp "dlls/$name/i386-windows/$name.dll" "$dist/$module"
done
if [ ! -d "$stripped/lib/wine" ]; then
    cp -a "$dist/usr/local/lib" "$dist/usr/local/share" "$stripped/"
    # Every file is a PE module (.sys, .cpl, .dll16 and the rest) or an ELF
    # file; their debug information names the build directory.
    find "$stripped/lib/wine/i386-windows" -type f \
        -exec i686-w64-mingw32-objcopy --strip-debug --file-alignment=512 {} \;
    find "$stripped/lib/wine/i386-unix" -type f -exec strip --strip-debug {} +
else
    for name in $modules; do
        module=lib/wine/i386-windows/$name.dll
        cp "$dist/usr/local/$module" "$stripped/$module"
        i686-w64-mingw32-objcopy --strip-debug --file-alignment=512 "$stripped/$module"
    done
fi
python3 "$tools/identity.py" "$dist" "$stripped"
