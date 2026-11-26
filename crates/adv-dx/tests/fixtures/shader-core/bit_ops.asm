cs_5_0
dcl_globalFlags refactoringAllowed
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 2
dcl_thread_group 8, 1, 1
imul null, r0.x, vThreadID.x, l(18)
iadd r0.y, vThreadID.x, l(8)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, r0.y, l(0), t0.xxxx
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.z, vThreadID.x, l(0), t0.xxxx
ilt r0.w, r0.z, r0.y
store_structured u0.x, r0.x, l(0), r0.w
imad r0.w, vThreadID.x, l(18), l(1)
ult r1.x, r0.z, r0.y
store_structured u0.x, r0.w, l(0), r1.x
imad r1.xyzw, vThreadID.xxxx, l(18, 18, 18, 18), l(2, 3, 4, 5)
imin r0.w, r0.y, r0.z
store_structured u0.x, r1.x, l(0), r0.w
umax r0.w, r0.y, r0.z
store_structured u0.x, r1.y, l(0), r0.w
ineg r0.w, r0.z
store_structured u0.x, r1.z, l(0), r0.w
not r0.w, r0.z
store_structured u0.x, r1.w, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(18, 18, 18, 18), l(6, 7, 8, 9)
ishr r0.w, r0.z, r0.y
store_structured u0.x, r1.x, l(0), r0.w
ushr r0.w, r0.z, r0.y
store_structured u0.x, r1.y, l(0), r0.w
itof r0.w, r0.z
store_structured u0.x, r1.z, l(0), r0.w
utof r0.w, r0.y
store_structured u0.x, r1.w, l(0), r0.w
imad r1.xyzw, vThreadID.xxxx, l(18, 18, 18, 18), l(10, 11, 12, 13)
countbits r0.w, r0.z
store_structured u0.x, r1.x, l(0), r0.w
firstbit_hi r0.w, r0.z
iadd r0.w, -r0.w, l(31)
movc r0.w, r0.z, r0.w, l(-1)
store_structured u0.x, r1.y, l(0), r0.w
firstbit_shi r0.w, r0.z
ieq r1.x, r0.w, l(-1)
iadd r0.w, -r0.w, l(31)
movc r0.w, r1.x, l(-1), r0.w
store_structured u0.x, r1.z, l(0), r0.w
firstbit_lo r0.w, r0.z
store_structured u0.x, r1.w, l(0), r0.w
ubfe r0.w, l(8), l(4), r0.z
imad r1.xyzw, vThreadID.xxxx, l(18, 18, 18, 18), l(14, 15, 16, 17)
store_structured u0.x, r1.x, l(0), r0.w
ibfe r0.w, l(8), l(4), r0.z
store_structured u0.x, r1.y, l(0), r0.w
bfi r0.w, l(8), l(4), r0.y, r0.z
udiv null, r0.y, r0.z, r0.y
store_structured u0.x, r1.z, l(0), r0.w
store_structured u0.x, r1.w, l(0), r0.y
ret
