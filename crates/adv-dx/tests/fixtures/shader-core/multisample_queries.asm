ps_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_texture2dms(4) (float,float,float,float) t0
dcl_input_ps_siv linear noperspective v0.xy, position
dcl_input_ps_sgv constant v1.x, sampleIndex
dcl_output o0.xyzw
dcl_temps 2
ftoi r0.xy, v0.xyxx
mov r0.zw, l(0,0,0,0)
ldms_indexable(texture2dms)(float,float,float,float) r0.xyzw, r0.xyzw, t0.xyzw, v1.x
resinfo_indexable(texture2dms)(float,float,float,float)_uint r1.xy, l(0), t0.xyzw
iadd r1.x, r1.y, r1.x
utof r1.z, r1.x
samplepos r1.xy, t0.xyxx, v1.x
mov r1.w, l(4.000000)
add o0.xyzw, r0.xyzw, r1.xyzw
ret
