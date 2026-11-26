cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 4
dcl_thread_group 1, 1, 1
iadd r0.x, vThreadID.x, vThreadID.x
iadd r0.y, r0.x, l(1)
ld_structured r1.x, r0.x, l(0), t0.xxxx
ld_structured r1.y, r0.y, l(0), t0.xxxx
mov r2.x, l(0)
loop
  iadd r2.x, r2.x, l(1)
  uge r2.z, r2.x, l(3)
  breakc_nz r2.z
  ieq r2.z, r2.x, r1.x
  continuec_nz r2.z
  store_structured u0.x, vThreadID.x, l(0), r2.x
endloop
retc_nz r1.y
store_structured u0.x, vThreadID.x, l(0), l(9)
ret
