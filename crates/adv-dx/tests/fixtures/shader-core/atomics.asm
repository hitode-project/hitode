cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 3
dcl_thread_group 8, 1, 1
store_structured u0.x, vThreadID.x, l(0), l(0)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, vThreadID.x, l(0), t0.xxxx
mov r1.x, vThreadID.x
mov r1.y, l(0)
imm_atomic_iadd r2.x, u0, r1.xyxx, r0.x
iadd r0.y, r2.x, l(17)
imm_atomic_cmp_exch r0.x, u0, r1.xyxx, r0.x, r0.y
iadd r0.y, vThreadID.x, l(8)
store_structured u0.x, r0.y, l(0), r0.x
ret
