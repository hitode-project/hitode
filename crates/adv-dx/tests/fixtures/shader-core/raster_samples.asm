ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps_siv linear noperspective v0.x, position
dcl_input vCoverage
dcl_output o0.xyzw
dcl_output oMask
dcl_temps 1
ftou r0.x, v0.x
samplepos o0.xy, rasterizer.xyxx, r0.x
sampleinfo o0.z, rasterizer.x
mov o0.w, l(1.000000)
mov oMask, vCoverage.x
ret
