ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps linear noperspective v0.xy
dcl_input_ps_siv linear noperspective v1.x, position
dcl_output o0.xyzw
dcl_temps 1
ftou r0.x, v1.x
and r0.x, r0.x, l(3)
eval_sample_index r0.x, v0.y, r0.x
eval_snapped r0.xw, v0.xxxy, l(3, -2, 0, 0)
eval_centroid r0.yz, v0.xxyx
mov o0.xyzw, r0.wxyz
ret
