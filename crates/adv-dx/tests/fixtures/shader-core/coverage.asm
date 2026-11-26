ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps_sgv constant v0.x, sampleIndex
dcl_input vCoverage
dcl_output o0.xyzw
sampleinfo_uint o0.z, rasterizer.x
mov o0.x, v0.x
mov o0.y, vCoverage.x
mov o0.w, l(0)
ret
