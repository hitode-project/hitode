cs_5_0
dcl_globalFlags refactoringAllowed | enableDoublePrecisionFloatOps | enable11_1DoubleExtensions
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 6
dcl_thread_group 8, 1, 1
imul null, r0.x, vThreadID.x, l(26)
iadd r0.y, vThreadID.x, l(8)
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, r0.y, l(0), t0.xxxx
ftod r0.zw, r0.y
utod r1.xy, r0.y
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.y, vThreadID.x, l(0), t0.xxxx
ftod r1.zw, r0.y
dmul r2.xy, r0.zwzw, r1.zwzw
store_structured u0.x, r0.x, l(0), r2.x
imad r2.x, vThreadID.x, l(26), l(1)
store_structured u0.x, r2.x, l(0), r2.y
imad r2.xyzw, vThreadID.xxxx, l(26, 26, 26, 26), l(2, 4, 6, 8)
dadd r3.xy, r0.zwzw, r1.zwzw
mov r3.zw, r3.xxxy
dtof r3.x, r3.xyxy
store_structured u0.x, r2.x, l(0), r3.z
iadd r4.xyzw, r2.xyzw, l(1, 1, 1, 1)
store_structured u0.x, r4.x, l(0), r3.w
dmin r3.zw, r0.zwzw, r1.zwzw
mov r3.yz, r3.zzwz
store_structured u0.x, r2.y, l(0), r3.y
store_structured u0.x, r4.y, l(0), r3.z
dmax r2.xy, r0.zwzw, r1.zwzw
store_structured u0.x, r2.z, l(0), r2.x
store_structured u0.x, r4.z, l(0), r2.y
ddiv r2.xy, r1.zwzw, r0.zwzw
store_structured u0.x, r2.w, l(0), r2.x
store_structured u0.x, r4.w, l(0), r2.y
drcp r2.xy, r1.zwzw
imad r4.xyzw, vThreadID.xxxx, l(26, 26, 26, 26), l(10, 12, 14, 16)
store_structured u0.x, r4.x, l(0), r2.x
iadd r5.xyzw, r4.xyzw, l(1, 1, 1, 1)
store_structured u0.x, r5.x, l(0), r2.y
dfma r2.xy, r1.zwzw, r0.zwzw, d(1.000000l, 0.000000l)
store_structured u0.x, r4.y, l(0), r2.x
store_structured u0.x, r5.y, l(0), r2.y
dmovc r2.xy, r0.yyyy, r1.zwzw, r0.zwzw
itod r2.zw, r0.y
store_structured u0.x, r4.z, l(0), r2.x
store_structured u0.x, r5.z, l(0), r2.y
store_structured u0.x, r4.w, l(0), r2.z
store_structured u0.x, r5.w, l(0), r2.w
imad r2.xyzw, vThreadID.xxxx, l(26, 26, 26, 26), l(18, 20, 21, 22)
store_structured u0.x, r2.x, l(0), r1.x
iadd r0.y, r2.x, l(1)
store_structured u0.x, r0.y, l(0), r1.y
deq r0.y, r0.zwzw, r1.zwzw
and r0.y, r0.y, l(1)
store_structured u0.x, r2.y, l(0), r0.y
dne r0.y, r0.zwzw, r1.zwzw
and r0.y, r0.y, l(1)
store_structured u0.x, r2.z, l(0), r0.y
dge r0.y, r1.zwzw, r0.zwzw
and r0.y, r0.y, l(1)
store_structured u0.x, r2.w, l(0), r0.y
dlt r0.y, r1.zwzw, r0.zwzw
dtou r0.z, r0.zwzw
xor r0.z, r3.x, r0.z
dtoi r0.w, r1.zwzw
and r0.y, r0.y, l(1)
imad r1.xyz, vThreadID.xxxx, l(26, 26, 26, 0), l(23, 24, 25, 0)
store_structured u0.x, r1.x, l(0), r0.y
store_structured u0.x, r1.y, l(0), r0.w
store_structured u0.x, r1.z, l(0), r0.z
ret
