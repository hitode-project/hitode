ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps_siv linear noperspective v0.xy, position
dcl_output o0.xyzw
mul o0.xy, v0.xyxx, l(0.031250, 0.062500, 0.000000, 0.000000)
mov o0.zw, l(0,0,0.500000,1.000000)
ret
