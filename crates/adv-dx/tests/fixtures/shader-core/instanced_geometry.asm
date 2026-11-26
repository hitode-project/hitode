gs_5_0
dcl_globalFlags refactoringAllowed
dcl_input_siv v[1][0].xyzw, position
dcl_input vGSInstanceID
dcl_temps 1
dcl_gsinstances 2
dcl_inputprimitive point
dcl_stream m0
dcl_outputtopology linestrip
dcl_output_siv o0.xyzw, position
dcl_output_sgv o1.x, primitive_id
dcl_maxout 2
mov o0.xyzw, v[0][0].xyzw
mov o1.x, vGSInstanceID.x
emit_stream m0
add r0.x, l(0.500000), v[0][0].x
mov o0.x, r0.x
mov o0.yzw, v[0][0].yyzw
mov o1.x, vGSInstanceID.x
emit_stream m0
cut_stream m0
ret
