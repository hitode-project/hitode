cs_5_0
dcl_globalFlags refactoringAllowed | enableMinimumPrecision
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 4
dcl_thread_group 1, 1, 1
utof r0.x, vThreadID.x
mul r0.x, r0.x, l(0.250000)
mul r1.x {min16f}, r0.x {def32 as min16f}, r0.x {def32 as min16f}
mov r1.x, r1.x {min16f as def32}
imul null, r0.x, vThreadID.x, l(3)
store_structured u0.x, r0.x, l(0), r1.x
iadd r0.y, vThreadID.x, l(-4)
imul null, r2.x {min16i}, r0.y, r0.y
imad r0.yz, vThreadID.xxxx, l(0, 3, 3, 0), l(0, 1, 2, 0)
store_structured u0.x, r0.y, l(0), r2.x {min16i}
imul null, r3.x {min16u}, vThreadID.x, vThreadID.x
store_structured u0.x, r0.z, l(0), r3.x {min16u}
ret
