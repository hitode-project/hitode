ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps_siv linear noperspective v0.xy, position
dcl_output o0.xyzw
dcl_temps 1
lt r0.x, l(4.000000), v0.y
discard_nz r0.x
mov o0.xzw, l(0.250000,0,0.750000,1.000000)
mul o0.y, v0.x, l(0.031250)
ret
