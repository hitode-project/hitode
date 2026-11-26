ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps linear v0.xyzw
dcl_output o0.xyzw
deriv_rtx_coarse o0.x, v0.x
deriv_rty_coarse o0.y, v0.y
deriv_rtx_fine o0.z, v0.z
deriv_rty_fine o0.w, v0.w
ret
