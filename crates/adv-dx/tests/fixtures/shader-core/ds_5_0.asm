ds_5_0
dcl_input_control_point_count 3
dcl_tessellator_domain domain_tri
dcl_globalFlags refactoringAllowed
dcl_input vDomain.xyz
dcl_input vicp[3][0].xyzw
dcl_input vicp[3][1].xy
dcl_output_siv o0.xyzw, position
dcl_output o1.xy
dcl_temps 1
mul r0.xyzw, vDomain.yyyy, vicp[1][0].xyzw
mad r0.xyzw, vicp[0][0].xyzw, vDomain.xxxx, r0.xyzw
mad o0.xyzw, vicp[2][0].xyzw, vDomain.zzzz, r0.xyzw
mul r0.xy, vDomain.yyyy, vicp[1][1].xyxx
mad r0.xy, vicp[0][1].xyxx, vDomain.xxxx, r0.xyxx
mad o1.xy, vicp[2][1].xyxx, vDomain.zzzz, r0.xyxx
ret
