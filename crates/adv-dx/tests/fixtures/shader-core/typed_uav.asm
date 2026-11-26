cs_5_0
dcl_globalFlags refactoringAllowed
dcl_uav_typed_texture2d (uint,uint,uint,uint) u1
dcl_input vThreadID.xy
dcl_temps 1
dcl_thread_group 8, 1, 1
ld_uav_typed_indexable(texture2d)(uint,uint,uint,uint) r0.x, vThreadID.xyyy, u1.xyzw
iadd r0.x, r0.x, vThreadID.x
store_uav_typed u1.xyzw, vThreadID.xyyy, r0.xxxx
ret
