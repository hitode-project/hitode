Berkeley SoftFloat Release 3e, upstream commit
`a0c6494cdc11865811dec815d5c0049fba9d82a8` from
https://github.com/ucb-bar/berkeley-softfloat-3.

`source/` and upstream headers in `include/` are selected by the
dependency closure of binary32, binary64, extended80 and binary128 operations. The upstream
8086-SSE specialization supplies Intel invalid results and NaN propagation.
COPYING.txt contains the BSD-3-Clause license and must ship with distributions.

`include/platform.h` and `shim.c` are local configuration/ABI adapters. Mutable
rounding, precision and exception settings use C thread-local storage on Windows
(MSVC and clang-cl) and C11 thread-local storage on Linux/Android. Every CPU
instruction sets controls from its canonical state and reads flags back, so
contexts and parallel tests cannot share rounding or exception state. No host
floating operation implements basic arithmetic.

Five upstream rounding helpers have a local TLS hook recording significand
increment direction for x87 C1. The hook does not alter arithmetic results or
IEEE exception flags; upstream license notices are retained.

The Rust `floating/transcendental.rs` module implements x87 logarithm, exponent,
scale and trigonometric operations. Logarithms use mantissa range reduction and
an atanh series; exp2-minus-one uses a direct exponential series that avoids
cancellation near zero. Trigonometric operations reduce arguments using a
binary128 pi constant and evaluate convergent series. Intermediate arithmetic is
SoftFloat binary128 (113 significant bits); the final result uses the guest
rounding direction at full extended80 precision, independent of x87 PC. FSCALE
uses integer exponent adjustment and SoftFloat's extended80 rounding helper,
including exact denormal scaling and final overflow/underflow.

These software transcendental results are mathematical approximations, not a
claim of bit identity with all CPU microcode. Intel SDM Volume 1 section 8.3.10
documents transcendental accuracy and argument-reduction limitations. F2XM1
accepts the architectural [-1,1] input range; sine/cosine/tangent set C2 and leave
the stack unchanged for finite magnitudes >=2^63. The implementation has host
x87 comparisons across all guest rounding directions in the small argument
range, explicit special-value/stack tests, and a Vorbis MDCT startup regression
covering FXCH at every logical stack position. Binary128 intermediates also keep
tiny FYL2XP1 operands without substituting host binary64 arithmetic. Tests do not
promise identical large-argument trigonometric reduction across CPU models.
