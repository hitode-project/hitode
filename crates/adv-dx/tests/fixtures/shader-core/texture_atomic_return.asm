cs_5_0
dcl_globalFlags refactoringAllowed
dcl_uav_typed_texture2d (uint,uint,uint,uint) u1
dcl_input vThreadID.xy
dcl_temps 1
dcl_thread_group 8, 1, 1
iadd r0.x, vThreadID.x, l(1)
imm_atomic_iadd r0.x, u1, vThreadID.xyxx, r0.x
ishl r0.y, vThreadID.x, l(1)
iadd r0.x, r0.y, r0.x
store_uav_typed u1.xyzw, vThreadID.xyyy, r0.xxxx
ret
