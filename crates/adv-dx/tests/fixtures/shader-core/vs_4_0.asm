vs_4_0
dcl_constantbuffer CB0[2], immediateIndexed
dcl_input v0.xyzw
dcl_input v1.xy
dcl_output_siv o0.xyzw, position
dcl_output o1.xy
mad o0.xyzw, v0.xyzw, cb0[0].xyzw, cb0[1].xyzw
mov o1.xy, v1.xyxx
ret
