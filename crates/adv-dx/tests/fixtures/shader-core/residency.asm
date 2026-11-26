ps_5_0
dcl_globalFlags refactoringAllowed
dcl_sampler s0, mode_default
dcl_sampler s1, mode_comparison
dcl_resource_texture2d (float,float,float,float) t0
dcl_resource_texture2d (float,float,float,float) t1
dcl_resource_texture2dms(4) (float,float,float,float) t2
dcl_resource_raw t3
dcl_resource_structured t4, 16
dcl_uav_typed_texture2d (uint,uint,uint,uint) u1
dcl_input_ps_siv linear noperspective v0.xy, position
dcl_input_ps linear v1.xy
dcl_output o0.xyzw
dcl_temps 7
mov r0.zw, l(0,0,0,0)
ftoi r0.xy, v0.xyxx
ldms_s_aoffimmi_indexable(1,1,0)(texture2dms)(float,float,float,float) r1.xyzw, r2.x, r0.xyzw, t2.xyzw, l(0)
ld_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r3.xyzw, r4.x, r0.xyww, t0.xyzw
check_access_fully_mapped r0.z, r2.x
movc r0.z, r0.z, l(1.000000), l(0)
mul r1.xyzw, r0.zzzz, r1.xyzw
check_access_fully_mapped r0.z, r4.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r3.xyzw, r0.zzzz, r1.xyzw
sample_cl_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t0.xyzw, s0, v1.y
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
sample_b_cl_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t0.xyzw, s0, l(0.500000), v1.y
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
sample_d_cl_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t0.xyzw, s0, l(0.015625, 0.015625, 0.000000, 0.000000), l(0.015625, 0.015625, 0.000000, 0.000000), v1.y
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
sample_l_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t0.xyzw, s0, v1.x
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
sample_c_cl_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.x, r3.x, v1.xyxx, t1.xxxx, s1, l(0.500000), v1.y
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, r2.x, l(0)
add r1.xyzw, r0.zzzz, r1.xyzw
sample_c_lz_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.x, r3.x, v1.xyxx, t1.xxxx, s1, l(0.500000)
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, r2.x, l(0)
add r1.xyzw, r0.zzzz, r1.xyzw
gather4_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t0.xyzw, s0.y
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
gather4_po_s_indexable(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, r0.xyxx, t0.xyzw, s0.x
check_access_fully_mapped r0.z, r3.x
movc r0.z, r0.z, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.zzzz, r1.xyzw
gather4_c_s_aoffimmi_indexable(1,1,0)(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, t1.xyzw, s1.x, l(0.500000)
check_access_fully_mapped r0.z, r3.x
movc r2.xyzw, r0.zzzz, r2.xyzw, l(0,0,0,0)
add r1.xyzw, r1.xyzw, r2.xyzw
gather4_po_c_s_indexable(texture2d)(float,float,float,float) r2.xyzw, r3.x, v1.xyxx, r0.xyxx, t1.xyzw, s1.x, l(0x3f000000)
ld_uav_typed_s_indexable(texture2d)(uint,uint,uint,uint) r0.x, r4.x, r0.xyyy, u1.xyzw
check_access_fully_mapped r0.y, r3.x
movc r2.xyzw, r0.yyyy, r2.xyzw, l(0,0,0,0)
add r1.xyzw, r1.xyzw, r2.xyzw
ftou r0.y, v0.x
ishl r0.z, r0.y, l(2)
ld_structured_s_indexable(structured_buffer, stride=16)(mixed,mixed,mixed,mixed) r2.xyzw, r3.x, r0.y, l(0), t4.xyzw
ld_raw_s_indexable(raw_buffer)(mixed,mixed,mixed,mixed) r5.x, r6.x, r0.z, t3.xxxx
check_access_fully_mapped r0.y, r6.x
movc r0.y, r0.y, l(1), l(0)
imul null, r0.y, r0.y, r5.x
utof r0.y, r0.y
add r1.xyzw, r0.yyyy, r1.xyzw
check_access_fully_mapped r0.y, r3.x
movc r0.y, r0.y, l(1.000000), l(0)
mad r1.xyzw, r2.xyzw, r0.yyyy, r1.xyzw
check_access_fully_mapped r0.y, r4.x
movc r0.y, r0.y, l(1), l(0)
imul null, r0.x, r0.y, r0.x
utof r0.x, r0.x
add o0.xyzw, r0.xxxx, r1.xyzw
ret
