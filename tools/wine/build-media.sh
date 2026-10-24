#!/bin/sh
# Wine 11.0, i386 with scalar SSE: PE media side plus a decode-free Unix build.
# Usage: build-media.sh SOURCE BUILD DIST STRIPPED_DIST
# Existing builds retain every installed binary except the added PE module.
set -eu
tools=$(cd "$(dirname "$0")" && pwd)
source=$(cd "$1" && pwd)
mkdir -p "$2" "$3" "$4"
build=$(cd "$2" && pwd)
dist=$(cd "$3" && pwd)
stripped=$(cd "$4" && pwd)
cd "$build"
if [ ! -f Makefile ]; then
    PKG_CONFIG_LIBDIR=/usr/lib/i386-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    "$source/configure" --host=i686-linux-gnu CC='gcc -m32' \
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
make -j"$(nproc)" dlls/winegstreamer/i386-windows/winegstreamer.dll
module=usr/local/lib/wine/i386-windows/winegstreamer.dll
mkdir -p "$dist/$(dirname "$module")"
cp dlls/winegstreamer/i386-windows/winegstreamer.dll "$dist/$module"
if [ ! -d "$stripped/lib/wine" ]; then
    cp -a "$dist/usr/local/lib" "$dist/usr/local/share" "$stripped/"
    find "$stripped" -type f \( -name '*.dll' -o -name '*.exe' -o -name '*.drv' \) \
        -exec i686-w64-mingw32-objcopy --strip-debug --file-alignment=512 {} \;
    find "$stripped" -type f -name '*.so' -exec strip --strip-debug {} +
else
    cp "$dist/$module" "$stripped/${module#usr/local/}"
    i686-w64-mingw32-objcopy --strip-debug --file-alignment=512 "$stripped/${module#usr/local/}"
fi
python3 "$tools/identity.py" "$dist" "$stripped"
