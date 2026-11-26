cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_raw t0
dcl_uav_raw u0
dcl_input vThreadID.x
dcl_temps 1
dcl_thread_group 8, 1, 1
ishl r0.x, vThreadID.x, l(2)
iadd r0.y, r0.x, l(32)
ld_raw_indexable(raw_buffer)(mixed,mixed,mixed,mixed) r0.y, r0.y, t0.xxxx
ld_raw_indexable(raw_buffer)(mixed,mixed,mixed,mixed) r0.z, r0.x, t0.xxxx
xor r0.y, r0.y, r0.z
store_raw u0.x, r0.x, r0.y
ret
