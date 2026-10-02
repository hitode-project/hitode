/* Local configuration for the BSD-3-Clause SoftFloat core. */
#ifndef ADV_SOFTFLOAT_PLATFORM_H
#define ADV_SOFTFLOAT_PLATFORM_H
#define LITTLEENDIAN 1
#define INLINE static inline
#if defined(_MSC_VER)
#define THREAD_LOCAL __declspec(thread)
#else
#define THREAD_LOCAL _Thread_local
#endif
extern THREAD_LOCAL unsigned char adv_sf_rounded_up;
#if defined(__GNUC__) || defined(__clang__)
#define SOFTFLOAT_BUILTIN_CLZ 1
#include "opts-GCC.h"
#endif
#endif
