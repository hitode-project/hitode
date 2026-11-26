ps_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s0, mode_default
dcl_resource_texture2d (float,float,float,float) t0
dcl_input_ps linear v0.xy
dcl_output o0.xyzw
lod o0.x, v0.xyxx, t0.x, s0
lod o0.y, v0.xyxx, t0.y, s0
mov o0.zw, l(0,0,0,1.000000)
ret
