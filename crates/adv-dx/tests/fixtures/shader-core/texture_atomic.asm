cs_5_0
dcl_globalFlags refactoringAllowed
dcl_uav_typed_texture2d (uint,uint,uint,uint) u1
dcl_input vThreadID.xy
dcl_temps 1
dcl_thread_group 8, 1, 1
iadd r0.x, vThreadID.x, l(1)
atomic_iadd u1, vThreadID.xyxx, r0.x
ret
