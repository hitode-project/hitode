cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_uav_structured u1, 4
dcl_input vThreadID.x
dcl_temps 3
dcl_thread_group 1, 1, 1
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, vThreadID.x, l(0), t0.xxxx
mov r1.x, vThreadID.x
mov r1.y, l(0)
atomic_and u0, r1.xyxx, r0.x
atomic_or u0, r1.xyxx, r0.x
atomic_xor u0, r1.xyxx, r0.x
atomic_umin u0, r1.xyxx, r0.x
atomic_umax u0, r1.xyxx, r0.x
atomic_cmp_store u0, r1.xyxx, r0.x, l(7)
imm_atomic_and r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(8), l(0), r2.x
imm_atomic_or r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(9), l(0), r2.x
imm_atomic_xor r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(10), l(0), r2.x
imm_atomic_umin r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(11), l(0), r2.x
imm_atomic_umax r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(12), l(0), r2.x
imm_atomic_exch r2.x, u0, r1.xyxx, r0.x
store_structured u0.x, l(13), l(0), r2.x
atomic_imin u1, r1.xyxx, r0.x
atomic_imax u1, r1.xyxx, r0.x
imm_atomic_imin r2.x, u1, r1.xyxx, r0.x
imm_atomic_imax r0.x, u1, r1.xyxx, r0.x
store_structured u0.x, l(14), l(0), r2.x
store_structured u0.x, l(15), l(0), r0.x
ret
