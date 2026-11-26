cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 4
dcl_thread_group 8, 1, 1
imul null, r0.x, vThreadID.x, l(24)
iadd r0.y, vThreadID.x, l(8)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, r0.y, l(0), t0.xxxx
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.z, vThreadID.x, l(0), t0.xxxx
div r0.w, r0.z, r0.y
store_structured u0.x, r0.x, l(0), r0.w
imad r1.xyz, vThreadID.xxxx, l(24, 24, 24, 0), l(1, 3, 7, 0)
dp2 r0.w, r0.zzzz, r0.yyyy
store_structured u0.x, r1.x, l(0), r0.w
imad r2.xyzw, vThreadID.xxxx, l(24, 24, 24, 24), l(2, 4, 5, 6)
eq r0.w, r0.y, r0.z
store_structured u0.x, r2.x, l(0), r0.w
ne r0.w, r0.y, r0.z
store_structured u0.x, r1.y, l(0), r0.w
ge r0.w, r0.z, r0.y
store_structured u0.x, r2.y, l(0), r0.w
lt r0.w, r0.z, r0.y
store_structured u0.x, r2.z, l(0), r0.w
exp r0.w, r0.z
store_structured u0.x, r2.w, l(0), r0.w
log r0.w, r0.y
store_structured u0.x, r1.z, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(24, 24, 24, 24), l(8, 9, 10, 11)
frc r0.w, r0.z
store_structured u0.x, r1.x, l(0), r0.w
min r0.w, r0.y, r0.z
store_structured u0.x, r1.y, l(0), r0.w
max r0.w, r0.y, r0.z
store_structured u0.x, r1.z, l(0), r0.w
round_ni r0.w, r0.z
store_structured u0.x, r1.w, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(24, 24, 24, 24), l(12, 13, 14, 15)
round_pi r0.w, r0.z
store_structured u0.x, r1.x, l(0), r0.w
round_z r0.w, r0.z
store_structured u0.x, r1.y, l(0), r0.w
rsq r0.w, r0.z
store_structured u0.x, r1.z, l(0), r0.w
sqrt r0.w, r0.z
store_structured u0.x, r1.w, l(0), r0.w
sincos r1.x, r2.x, r0.z
imad r3.xyzw, vThreadID.xxxx, l(24, 24, 24, 24), l(16, 17, 18, 19)
store_structured u0.x, r3.x, l(0), r1.x
store_structured u0.x, r3.y, l(0), r2.x
rcp r0.w, r0.y
store_structured u0.x, r3.z, l(0), r0.w
mov_sat r0.w, r0.z
store_structured u0.x, r3.w, l(0), r0.w
mad r0.w, r0.z, r0.y, l(1.000000)
f16tof32 r0.z, r0.z
imad r1.xyzw, vThreadID.xxxx, l(24, 24, 24, 24), l(20, 21, 22, 23)
store_structured u0.x, r1.x, l(0), r0.w
store_structured u0.x, r1.y, l(0), r0.z
ftoi r0.z, r0.y
ftou r0.y, r0.y
store_structured u0.x, r1.z, l(0), r0.z
store_structured u0.x, r1.w, l(0), r0.y
ret
