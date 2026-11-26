cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 8, 1, 1
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, vThreadID.x, l(0), t0.xxxx
and r0.x, r0.x, l(7)
mov r0.yz, l(0,0,0,0)
loop
  uge r0.w, r0.z, r0.x
  breakc_nz r0.w
  and r0.w, r0.z, l(1)
  iadd r1.x, r0.z, r0.y
  xor r1.y, r0.x, r0.y
  movc r0.y, r0.w, r1.x, r1.y
  iadd r0.z, r0.z, l(1)
endloop
switch r0.x
  case l(0)
  mov r0.x, l(9)
  break
  case l(1)
  mov r0.x, l(6)
  break
  case l(2)
  ishl r0.x, r0.y, l(2)
  break
  default
  iadd r0.x, r0.y, l(7)
  break
endswitch
store_structured u0.x, vThreadID.x, l(0), r0.x
ret
