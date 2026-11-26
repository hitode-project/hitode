ps_5_0
dcl_globalFlags refactoringAllowed
dcl_function_body fb0
dcl_function_body fb1
dcl_function_table ft0 = {fb0}
dcl_function_table ft1 = {fb1}
dcl_interface fp0[1][1] = {ft0, ft1}
dcl_input_ps_siv linear noperspective v0.xy, position
dcl_output o0.xyzw
dcl_temps 1
fcall fp0[0][0]
mov o0.xz, r0.xxyx
mov o0.yw, l(0,0,0,1.000000)
ret
label fb0
mov r0.x, l(0)
mov r0.y, v0.y
ret
label fb1
mov r0.x, v0.x
mov r0.y, l(0)
ret
