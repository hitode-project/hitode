hs_5_0
hs_decls
dcl_input_control_point_count 3
dcl_output_control_point_count 3
dcl_tessellator_domain domain_tri
dcl_tessellator_partitioning partitioning_fractional_even
dcl_tessellator_output_primitive output_triangle_cw
dcl_globalFlags refactoringAllowed
dcl_constantbuffer CB0[1], immediateIndexed
hs_control_point_phase
dcl_input vOutputControlPointID
dcl_input v[3][0].xyzw
dcl_input v[3][1].xy
dcl_output o0.xyzw
dcl_output o1.xy
dcl_temps 1
mov r0.x, vOutputControlPointID
mov o0.xyzw, v[r0.x + 0][0].xyzw
mov o1.xy, v[r0.x + 0][1].xyxx
ret
hs_fork_phase
dcl_hs_fork_phase_instance_count 3
dcl_input vForkInstanceID
dcl_input vicp[3][1].x
dcl_output_siv o0.x, finalTriUeq0EdgeTessFactor
dcl_output_siv o1.x, finalTriVeq0EdgeTessFactor
dcl_output_siv o2.x, finalTriWeq0EdgeTessFactor
dcl_temps 1
dcl_indexrange o0.x 3
mov r0.x, vForkInstanceID.x
mad r0.y, vicp[r0.x + 0][1].x, cb0[0].x, l(1.000000)
mov o[r0.x + 0].x, r0.y
ret
hs_fork_phase
dcl_output_siv o3.x, finalTriInsideTessFactor
mov o3.x, l(3.000000)
ret
