cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 1
dcl_thread_group 8, 1, 1
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, vThreadID.x, l(0), t0.xxxx
ieq r0.y, r0.x, l(7)
if_nz r0.y
  store_structured u0.x, vThreadID.x, l(0), l(17)
  ret
else
  store_structured u0.x, vThreadID.x, l(0), l(13)
endif
and r0.x, r0.x, l(7)
mov r0.y, l(0)
loop
  uge r0.z, r0.y, r0.x
  breakc_nz r0.z
  and r0.z, r0.y, l(1)
  if_nz r0.z
    iadd r0.z, r0.y, l(1)
    mov r0.y, r0.z
    continue
  endif
  ieq r0.z, r0.y, l(4)
  if_nz r0.z
    break
  endif
  ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.z, vThreadID.x, l(0), u0.xxxx
  iadd r0.z, r0.y, r0.z
  store_structured u0.x, vThreadID.x, l(0), r0.z
  iadd r0.y, r0.y, l(1)
endloop
ret
