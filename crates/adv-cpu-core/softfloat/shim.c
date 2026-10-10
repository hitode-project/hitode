/* ABI adapter only. Arithmetic is Berkeley SoftFloat 3e (see COPYING.txt). */
#include "platform.h"
#include "softfloat.h"
#include "internals.h"
THREAD_LOCAL unsigned char adv_sf_rounded_up = 0;
void adv_sf_config(uint8_t rounding, uint8_t precision) {
    softfloat_roundingMode = rounding;
    extF80_roundingPrecision = precision;
    softfloat_detectTininess = softfloat_tininess_beforeRounding;
    softfloat_exceptionFlags = 0;
    adv_sf_rounded_up = 0;
}
uint8_t adv_sf_flags(void) { return (uint8_t)softfloat_exceptionFlags; }
uint8_t adv_sf_round_up(void) { return adv_sf_rounded_up; }
void adv_sf_ext_binary(const extFloat80_t *a, const extFloat80_t *b, extFloat80_t *out, uint8_t op) {
    switch (op) {
    case 0: *out = extF80_add(*a, *b); break;
    case 1: *out = extF80_sub(*a, *b); break;
    case 2: *out = extF80_mul(*a, *b); break;
    case 3: *out = extF80_div(*a, *b); break;
    default: *out = extF80_rem(*a, *b); break;
    }
}
void adv_sf_ext_unary(const extFloat80_t *a, extFloat80_t *out, uint8_t op) {
    if (op == 0) *out = extF80_sqrt(*a);
    else {
        *out = extF80_roundToInt(*a, softfloat_roundingMode, true);
        if (softfloat_exceptionFlags & softfloat_flag_inexact)
            adv_sf_rounded_up = (out->signExp & 0x7fff) > (a->signExp & 0x7fff)
                || ((out->signExp & 0x7fff) == (a->signExp & 0x7fff) && out->signif > a->signif);
    }
}
void adv_sf_ext_from(uint64_t bits, uint8_t kind, extFloat80_t *out) {
    switch (kind) {
    case 0: { float32_t a = { (uint32_t)bits }; *out = f32_to_extF80(a); break; }
    case 1: { float64_t a = { bits }; *out = f64_to_extF80(a); break; }
    case 2: *out = i32_to_extF80((int32_t)bits); break;
    default: *out = i64_to_extF80((int64_t)bits); break;
    }
}
uint64_t adv_sf_ext_to(const extFloat80_t *a, uint8_t kind, uint8_t rounding) {
    switch (kind) {
    case 0: return extF80_to_f32(*a).v;
    case 1: return extF80_to_f64(*a).v;
    case 2: return (uint32_t)(int32_t)extF80_to_i32(*a, rounding, true);
    default: return (uint64_t)(int64_t)extF80_to_i64(*a, rounding, true);
    }
}
uint8_t adv_sf_ext_compare(const extFloat80_t *a, const extFloat80_t *b, uint8_t signal_nan) {
    bool an = ((a->signExp & 0x7fff) == 0x7fff) && (a->signif & UINT64_C(0x7fffffffffffffff));
    bool bn = ((b->signExp & 0x7fff) == 0x7fff) && (b->signif & UINT64_C(0x7fffffffffffffff));
    if (an || bn) {
        if (signal_nan || extF80_isSignalingNaN(*a) || extF80_isSignalingNaN(*b)) softfloat_exceptionFlags |= softfloat_flag_invalid;
        return 3;
    }
    if (extF80_eq(*a, *b)) return 0;
    return extF80_lt(*a, *b) ? 1 : 2;
}
uint64_t adv_sf_sse_binary(uint64_t left, uint64_t right, uint8_t op, uint8_t width) {
    if (width == 4) {
        float32_t a = {(uint32_t)left}, b = {(uint32_t)right}, out;
        switch (op) {
        case 0: out = f32_add(a,b); break;
        case 1: out = f32_sub(a,b); break;
        case 2: out = f32_mul(a,b); break;
        case 3: out = f32_div(a,b); break;
        default: out = f32_sqrt(b); break;
        }
        return out.v;
    } else {
        float64_t a = {left}, b = {right}, out;
        switch (op) {
        case 0: out = f64_add(a,b); break;
        case 1: out = f64_sub(a,b); break;
        case 2: out = f64_mul(a,b); break;
        case 3: out = f64_div(a,b); break;
        default: out = f64_sqrt(b); break;
        }
        return out.v;
    }
}
uint64_t adv_sf_sse_convert(uint64_t bits, uint8_t from, uint8_t to, uint8_t rounding) {
    if (from == 2) return to == 0 ? i32_to_f32((int32_t)bits).v : i32_to_f64((int32_t)bits).v;
    if (from == 0) {
        float32_t a = {(uint32_t)bits};
        return to == 2 ? (uint32_t)(int32_t)f32_to_i32(a, rounding, true) : f32_to_f64(a).v;
    } else {
        float64_t a = {bits};
        return to == 2 ? (uint32_t)(int32_t)f64_to_i32(a, rounding, true) : f64_to_f32(a).v;
    }
}
void adv_sf_quad_binary(const float128_t *a, const float128_t *b, float128_t *out, uint8_t op) {
    switch (op) {
    case 0: *out = f128_add(*a, *b); break;
    case 1: *out = f128_sub(*a, *b); break;
    case 2: *out = f128_mul(*a, *b); break;
    default: *out = f128_div(*a, *b); break;
    }
}
void adv_sf_quad_from_ext(const extFloat80_t *a, float128_t *out) { *out = extF80_to_f128(*a); }
void adv_sf_quad_to_ext(const float128_t *a, extFloat80_t *out) { *out = f128_to_extF80(*a); }
void adv_sf_quad_from_i32(int32_t a, float128_t *out) { *out = i32_to_f128(a); }
int32_t adv_sf_quad_to_i32(const float128_t *a, uint8_t rounding) { return (int32_t)f128_to_i32(*a, rounding, false); }
int64_t adv_sf_quad_to_i64(const float128_t *a, uint8_t rounding) { return (int64_t)f128_to_i64(*a, rounding, false); }
void adv_sf_quad_round(const float128_t *a, float128_t *out, uint8_t rounding) { *out = f128_roundToInt(*a, rounding, false); }
uint8_t adv_sf_quad_lt(const float128_t *a, const float128_t *b) { return f128_lt(*a, *b); }
void adv_sf_ext_scale(const extFloat80_t *a, int32_t count, extFloat80_t *out) {
    int32_t exp = a->signExp & 0x7fff;
    uint64_t sig = a->signif;
    if (!sig || exp == 0x7fff) { *out = *a; return; }
    if (!exp) {
        struct exp32_sig64 normalized = softfloat_normSubnormalExtF80Sig(sig);
        exp = normalized.exp + 1;
        sig = normalized.sig;
    }
    if (count > 32768) count = 32768;
    if (count < -32768) count = -32768;
    *out = softfloat_roundPackToExtF80(a->signExp >> 15, exp + count, sig, 0, 80);
}
