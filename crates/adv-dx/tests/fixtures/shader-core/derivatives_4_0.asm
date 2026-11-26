ps_4_0
dcl_input_ps linear v0.xyzw
dcl_output o0.xyzw
dcl_temps 2
deriv_rtx r0.xyzw, v0.xyzw
deriv_rty r1.xyzw, v0.xyzw
add o0.xyzw, r0.xyzw, -r1.xyzw
ret
