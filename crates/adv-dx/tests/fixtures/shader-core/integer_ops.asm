cs_5_0
dcl_globalFlags refactoringAllowed | enable11_1ShaderExtensions
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 8, 1, 1
ishl r0.x, vThreadID.x, l(4)
iadd r0.y, vThreadID.x, l(8)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, r0.y, l(0), t0.xxxx
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.z, vThreadID.x, l(0), t0.xxxx
and r0.w, r0.y, r0.z
store_structured u0.x, r0.x, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(16, 16, 16, 16), l(1, 2, 3, 4)
or r0.w, r0.y, r0.z
store_structured u0.x, r1.x, l(0), r0.w
xor r0.w, r0.y, r0.z
store_structured u0.x, r1.y, l(0), r0.w
ieq r0.w, r0.y, r0.z
store_structured u0.x, r1.z, l(0), r0.w
ine r0.w, r0.y, r0.z
store_structured u0.x, r1.w, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(16, 16, 16, 16), l(5, 6, 7, 8)
ige r0.w, r0.z, r0.y
store_structured u0.x, r1.x, l(0), r0.w
uge r0.w, r0.z, r0.y
store_structured u0.x, r1.y, l(0), r0.w
imax r0.w, r0.y, r0.z
store_structured u0.x, r1.z, l(0), r0.w
umin r0.w, r0.y, r0.z
store_structured u0.x, r1.w, l(0), r0.w
imad r0.w, r0.z, r0.y, l(7)
imad r1.xyzw, vThreadID.xxxx, l(16, 16, 16, 16), l(9, 10, 11, 12)
store_structured u0.x, r1.x, l(0), r0.w
movc r0.w, r0.z, r0.y, l(7)
store_structured u0.x, r1.y, l(0), r0.w
mov r0.w, |r0.z|
store_structured u0.x, r1.z, l(0), r0.w
mov r0.w, -r0.z
store_structured u0.x, r1.w, l(0), r0.w
dp3 r0.w, r0.zzzz, r0.yyyy
imad r1.xyz, vThreadID.xxxx, l(16, 16, 16, 0), l(13, 14, 15, 0)
store_structured u0.x, r1.x, l(0), r0.w
dp4 r0.w, r0.zzzz, r0.yyyy
msad r0.y, r0.z, r0.y, l(3)
store_structured u0.x, r1.y, l(0), r0.w
store_structured u0.x, r1.z, l(0), r0.y
ret
