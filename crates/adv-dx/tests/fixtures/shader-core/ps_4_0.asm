ps_4_0
dcl_immediateConstantBuffer { { 1.000000, 0, 0, 0},
                              { 0, 1.000000, 0, 1.000000},
                              { 0, 0, 1.000000, 1.000000},
                              { 1.000000, 1.000000, 0, 0} }
dcl_sampler s0, mode_default
dcl_resource_texture2d (float,float,float,float) t0
dcl_input_ps_siv linear noperspective v0.x, position
dcl_input_ps linear v1.xy
dcl_output o0.xyzw
dcl_temps 2
ftou r0.x, v0.x
and r0.x, r0.x, l(3)
sample_aoffimmi(1,-2,0) r1.xyzw, v1.xyxx, t0.xyzw, s0
mul o0.xyzw, r1.xyzw, icb[r0.x + 0].xyzw
ret
