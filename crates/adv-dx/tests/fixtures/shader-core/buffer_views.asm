cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_buffer (uint,uint,uint,uint) t0
dcl_uav_typed_buffer (uint,uint,uint,uint) u0
dcl_input vThreadID.x
dcl_temps 1
dcl_thread_group 1, 1, 1
bufinfo_indexable(buffer)(uint,uint,uint,uint) r0.x, t0.xyzw
ld_indexable(buffer)(uint,uint,uint,uint) r0.yz, vThreadID.xxxx, t0.zxyw
iadd r0.y, r0.x, r0.y
ld_indexable(buffer)(uint,uint,uint,uint) r0.x, r0.xxxx, t0.xyzw
store_uav_typed u0.xyzw, l(0,0,0,0), r0.yyyy
store_uav_typed u0.xyzw, l(1,1,1,1), r0.zzzz
ld_uav_typed_indexable(buffer)(uint,uint,uint,uint) r0.y, vThreadID.xxxx, u0.yxzw
iadd r0.x, r0.x, r0.y
store_uav_typed u0.xyzw, l(2,2,2,2), r0.xxxx
ret
