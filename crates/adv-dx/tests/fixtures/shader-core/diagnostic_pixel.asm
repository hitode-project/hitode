ps_5_0
dcl_globalFlags refactoringAllowed
dcl_input_ps_siv linear noperspective v0.xyzw, position
dcl_output o0.xyzw
dcl_temps 1
lt r0.x, v0.x, l(1.000000)
if_nz r0.x
  abort
endif
mov o0.xyzw, v0.xyzw
ret
