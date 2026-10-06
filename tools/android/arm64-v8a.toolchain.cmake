# Test-only Unicorn's cmake-rs build needs the Android ABI and its QEMU probes.
set(ANDROID_ABI arm64-v8a)
set(ANDROID_PLATFORM 29)
include("$ENV{ANDROID_NDK_ROOT}/build/cmake/android.toolchain.cmake")
add_compile_definitions(CONFIG_INT128 CONFIG_POSIX)
