// CASE ps11_texture
ps_1_1
tex t0
mov r0, t0

// CASE ps12_coissue
ps_1_2
def c0, 0.5, 0.25, -0.5, 1.0
tex t0
mul r0.rgb, t0_bx2, c0
+mov r0.a, t0.a

// CASE ps13_dependent
ps_1_3
tex t0
texreg2ar t1, t0
mov r0, t1

// CASE ps14_phase
ps_1_4
texld r0, t0
phase
mul r0.rgb, r0, v0
mov r0.a, v0.a

// CASE ps11_bump
ps_1_1
tex t0
texbem t1, t0
mov r0, t1

// CASE ps11_bump_luminance
ps_1_1
tex t0
texbeml t1, t0
mov r0, t1

// CASE ps14_bem
ps_1_4
def c0, 0.25, 0.25, 0.0, 0.0
texcrd r0.rgb, t0
mov r1, c0
bem r0.xy, r0, r1
phase
texld r0, r0

// CASE ps13_matrix2
ps_1_3
tex t0
texm3x2pad t1, t0
texm3x2tex t2, t0
mov r0, t2

// CASE ps13_dot_texture
ps_1_3
tex t0
texdp3tex t1, t0
mov r0, t1

// The full RGB8 range survives the intermediate x8 step; alpha is rewritten
// after phase, as required by native validation.
// CASE ps14_range
ps_1_4
def c0, 1.0, 1.0, 1.0, 1.0
mul_x8 r0, c0, c0
phase
mul r0.rgb, r0, c0
mov r0.a, c0
mul_d8 r0, r0, c0

// CASE vs20_passthrough
vs_2_0
dcl_position v0
dcl_color v1
dcl_texcoord0 v2
dcl_texcoord1 v3
dcl_texcoord2 v4
mov oPos, v0
mov oD0, v1
mov oT0, v2
mov oT1, v3
mov oT2, v4

// CASE vs11_passthrough
vs_1_1
dcl_position v0
dcl_color v1
dcl_texcoord0 v2
dcl_texcoord1 v3
dcl_texcoord2 v4
mov oPos, v0
mov oD0, v1
mov oT0, v2
mov oT1, v3
mov oT2, v4

// CASE vs30_passthrough
vs_3_0
dcl_position v0
dcl_color v1
dcl_texcoord0 v2
dcl_texcoord1 v3
dcl_texcoord2 v4
dcl_position o0
dcl_color o1
dcl_texcoord0 o2
dcl_texcoord1 o3
dcl_texcoord2 o4
mov o0, v0
mov o1, v1
mov o2, v2
mov o3, v3
mov o4, v4

// CASE ps30_flow
ps_3_0
dcl_texcoord0 v0
def c0, 0.125, 0.125, 0.125, 0.125
defi i0, 3, 0, 1, 0
mov r0, c0
loop aL, i0
add r0, r0, c0
endloop
setp_gt p0, v0.x, c0.x
(p0) mov r0, v0
mov oC0, r0

// CASE ps30_misc
ps_3_0
dcl vPos.xy
dcl vFace
def c0, 0.0625, 0.0833333333, 1.0, 0.0
mul r0.xy, vPos, c0
cmp r0.z, vFace, c0.z, c0.w
mov r0.w, c0.z
mov oC0, r0

// CASE ps30_lod
ps_3_0
dcl_texcoord0 v0
dcl_2d s0
def c0, 0.0, 0.0, 0.0, 0.0
mov r0, v0
mov r0.w, c0.x
texldl oC0, r0, s0

// CASE ps30_color
ps_3_0
dcl_color v0
mov oC0, v0

// CASE ps30_derivatives
ps_3_0
dcl_texcoord0 v0
def c0, 16.0, 12.0, 0.0, 1.0
dsx r0, v0
dsy r1, v0
mul r0.x, r0.x, c0.x
mul r0.y, r1.y, c0.y
mov r0.z, c0.z
mov r0.w, c0.w
mov oC0, r0

// CASE ps30_specials
ps_3_0
def c0, 0.0, 1.0, -1.0, 0.25
def c1, 1.0, 0.0, 0.0, 1.0
rcp r0, c0.x
mul r1, r0, c0.x
setp_eq p0, r1, r1
mov r2, c1
(p0) mov r2, c0
setp_gt p0, r0, c0.y
(p0) mov r2.y, c0.y
log r0, c0.x
setp_lt p0, r0, c0.z
(p0) mov r2.z, c0.y
rsq r0, c0.z
setp_eq p0, r0, c0.y
(p0) mov r2.w, c0.y
mov oC0, r2

// CASE vs30_texture
vs_3_0
dcl_position v0
dcl_color v1
dcl_texcoord0 v2
dcl_position o0
dcl_color o1
dcl_texcoord0 o2
dcl_2d s0
def c0, 0.0, 0.0, 0.0, 0.0
mov o0, v0
texldl r0, c0, s0
mul o1, v1, r0
mov o2, v2

// CASE vs30_generic
vs_3_0
dcl_position v0
dcl_color v1
dcl_position o0
dcl_normal o3
mov o0, v0
mov o3, v1

// CASE ps30_generic
ps_3_0
dcl_normal v2
mov oC0, v2
