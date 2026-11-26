cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 1, 1, 1
iadd r0.x, vThreadID.x, l(1)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, r0.x, l(0), t0.xxxx
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, vThreadID.x, l(0), t0.xxxx
and r0.z, vThreadID.x, l(1)
swapc r0.x, r1.x, r0.z, r0.y, r0.x
ishl r0.y, vThreadID.x, l(1)
store_structured u0.x, r0.y, l(0), r1.x
imad r0.z, vThreadID.x, l(2), l(1)
store_structured u0.x, r0.z, l(0), r0.x
ret
