# x86 MSVC TLS code addresses TEB.ThreadLocalStoragePointer through FS:0x2c.
.globl __tls_array
.set __tls_array, 0x2c
