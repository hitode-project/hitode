ps_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s0, mode_default
dcl_sampler s1, mode_comparison
dcl_resource_texture2d (float,float,float,float) t0
dcl_resource_texture2d (float,float,float,float) t1
dcl_input_ps linear v0.xy
dcl_output o0.xyzw
dcl_temps 2
sample_indexable(texture2d)(float,float,float,float) r0.xyzw, v0.xyxx, t0.xyzw, s0
sample_b_indexable(texture2d)(float,float,float,float) r1.xyzw, v0.xyxx, t0.xyzw, s0, v0.x
add r0.xyzw, r0.xyzw, r1.xyzw
sample_d_indexable(texture2d)(float,float,float,float) r1.xyzw, v0.xyxx, t0.xyzw, s0, v0.xyxx, v0.yxyy
add r0.xyzw, r0.xyzw, r1.xyzw
sample_l_indexable(texture2d)(float,float,float,float) r1.xyzw, v0.xyxx, t0.xyzw, s0, v0.y
add r0.xyzw, r0.xyzw, r1.xyzw
sample_c_indexable(texture2d)(float,float,float,float) r1.x, v0.xyxx, t1.xxxx, s1, v0.x
add r0.xyzw, r0.xyzw, r1.xxxx
sample_c_lz_indexable(texture2d)(float,float,float,float) r1.x, v0.xyxx, t1.xxxx, s1, v0.y
add o0.xyzw, r0.xyzw, r1.xxxx
ret
