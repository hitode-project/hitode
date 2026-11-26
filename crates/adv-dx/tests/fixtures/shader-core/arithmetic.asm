cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 8, 1, 1
ishl r0.x, vThreadID.x, l(2)
iadd r0.y, vThreadID.x, l(8)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, r0.y, l(0), t0.xxxx
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.z, vThreadID.x, l(0), t0.xxxx
round_ne r0.w, -|r0.z|
add r0.w, r0.y, r0.w
store_structured u0.x, r0.x, l(0), r0.w
f32tof16 r0.w, r0.z
imad r1.xyz, vThreadID.xxxx, l(4, 4, 4, 0), l(1, 2, 3, 0)
store_structured u0.x, r1.x, l(0), r0.w
ishl r0.w, r0.z, r0.y
udiv r0.y, null, r0.z, r0.y
bfrev r0.z, r0.z
xor r0.z, r0.z, r0.w
store_structured u0.x, r1.y, l(0), r0.z
store_structured u0.x, r1.z, l(0), r0.y
ret
