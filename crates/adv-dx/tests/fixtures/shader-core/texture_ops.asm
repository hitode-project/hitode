ps_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s0, mode_default
dcl_sampler s1, mode_comparison
dcl_resource_texture2d (float,float,float,float) t0
dcl_resource_texture2d (float,float,float,float) t1
dcl_input_ps linear v0.xy
dcl_output o0.xyzw
dcl_temps 3
gather4_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r0.xyzw, v0.xyxx, t0.xyzw, s0.y
gather4_c_indexable(texture2d)(float,float,float,float) r1.xyzw, v0.xyxx, t1.xyzw, s1.x, l(0.500000)
add r0.xyzw, r0.xyzw, r1.xyzw
resinfo_indexable(texture2d)(float,float,float,float)_uint r1.xyz, l(1), t0.xywz
utof r1.xyz, r1.xyzx
mul r1.xy, r1.xyxx, v0.xyxx
ftoi r2.xy, r1.xyxx
mov r2.zw, l(0,0,1,1)
ld_indexable(texture2d)(float,float,float,float) r2.xyzw, r2.xyzw, t0.xyzw
add r0.xyzw, r0.xyzw, r2.xyzw
add o0.xyzw, r1.zzzz, r0.xyzw
ret
