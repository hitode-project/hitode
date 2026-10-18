`optical.iso` is an authored 28-sector ISO9660/Joliet test image, with no game
data. It is shared by the VFS and Win32 regression tests.

- Volume label: `OPTICAL_TEST`; logical sector size: 2048 bytes.
- Primary and Joliet descriptors: sectors 16 and 17; terminator: sector 18.
- Primary root: sector 20; Joliet root: sector 21; `Sub`: sector 22.
- Primary little/big endian root path tables: sectors 23 and 24.
- `ReadMe.txt`: sector 25, `hello world` (11 bytes).
- `Sub/日本.txt`: sector 26, `abcde` (5 bytes).
- `Sub/split.bin`: sectors 26 and 27, `abcde world` (5 + 6 bytes).
- Records use October 7, 2026, 12:34:56 UTC+8; identifiers use `;1` versions.

The primary namespace exposes `README.TXT`; the Joliet namespace exposes mixed
case and Unicode names. File extents deliberately share bytes to verify that
the VFS identities belong to file records, rather than only to physical extents.
