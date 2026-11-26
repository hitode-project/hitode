gs_5_0
dcl_globalFlags refactoringAllowed
dcl_input_siv v[3][0].xyzw, position
dcl_input v[3][1].xy
dcl_inputprimitive triangle
dcl_stream m0
dcl_outputtopology trianglestrip
dcl_output_siv o0.xyzw, position
dcl_output o1.xy
dcl_maxout 3
mov o0.xyzw, v[0][0].xyzw
mov o1.xy, v[0][1].xyxx
emit_stream m0
mov o0.xyzw, v[1][0].xyzw
mov o1.xy, v[1][1].xyxx
emit_stream m0
mov o0.xyzw, v[2][0].xyzw
mov o1.xy, v[2][1].xyxx
emit_then_cut_stream m0
ret
