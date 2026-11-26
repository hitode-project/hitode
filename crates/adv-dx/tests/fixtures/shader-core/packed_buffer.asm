cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_buffer (uint,uint,uint,uint) t0
dcl_uav_typed_buffer (uint,uint,uint,uint) u0
dcl_input vThreadID.x
dcl_temps 1
dcl_thread_group 2, 1, 1
ld_indexable(buffer)(uint,uint,uint,uint) r0.x, vThreadID.xxxx, t0.xyzw
store_uav_typed u0.xyzw, vThreadID.xxxx, r0.xxxx
ret
