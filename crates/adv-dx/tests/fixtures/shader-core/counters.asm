cs_5_0
dcl_globalFlags refactoringAllowed
dcl_uav_structured u1, 4
dcl_uav_structured u2, 4
dcl_uav_structured_opc u3, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 1, 1, 1
imm_atomic_consume r0.x, u2
imm_atomic_alloc r1.x, u1
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, r0.x, l(0), u2.xxxx
iadd r0.x, r0.x, vThreadID.x
store_structured u1.x, r1.x, l(0), r0.x
imm_atomic_alloc r0.x, u3
iadd r0.y, r0.x, l(19)
store_structured u3.x, r0.x, l(0), r0.y
ret
