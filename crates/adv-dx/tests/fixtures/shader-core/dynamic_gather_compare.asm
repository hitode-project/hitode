ps_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s1, mode_comparison
dcl_resource_texture2d (float,float,float,float) t1
dcl_input_ps linear v0.xy
dcl_input_ps_siv linear noperspective v1.xy, position
dcl_output o0.xyzw
dcl_temps 1
ftoi r0.xy, v1.xyxx
iadd r0.xy, r0.xyxx, l(31, -32, 0, 0)
gather4_po_c_indexable(texture2d)(float,float,float,float) o0.xyzw, v0.xyxx, r0.xyxx, t1.xyzw, s1.x, l(0x3f000000)
ret
