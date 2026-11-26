vs_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s0, mode_default
dcl_resource_texture2d (float,float,float,float) t0
dcl_input_sgv v0.x, vertex_id
dcl_output_siv o0.xyzw, position
dcl_temps 1
and r0.x, v0.x, l(1)
ushr r0.z, v0.x, l(1)
utof r0.xy, r0.xzxx
sample_l_indexable(texture2d)(float,float,float,float) o0.xyzw, r0.xyxx, t0.xyzw, s0, l(0.000000)
ret
