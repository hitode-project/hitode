ps_5_0
dcl_globalFlags refactoringAllowed
dcl_constantbuffer CB0[8], dynamicIndexed
dcl_input_ps linear v0.xyzw
dcl_input_ps linear v1.xyzw
dcl_input_ps constant v2.x
dcl_output o0.xyzw
dcl_temps 2
dcl_indexableTemp x0[2], 4
mov x0[0].xyzw, v0.xyzw
mov x0[1].xyzw, v1.xyzw
and r0.xy, v2.xxxx, l(1, 7, 0, 0)
mov r1.xyzw, x0[r0.x + 0].xyzw
add o0.xyzw, r1.xyzw, cb0[r0.y + 0].xyzw
ret
