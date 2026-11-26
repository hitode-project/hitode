cs_5_0
dcl_globalFlags refactoringAllowed | enableDoublePrecisionFloatOps
dcl_resource_structured t0, 4
dcl_uav_structured u0, 4
dcl_input vThreadID.x
dcl_temps 1
dcl_thread_group 8, 1, 1
ld_structured_indexable(structured_buffer, stride=4)(mixed,mixed,mixed,mixed) r0.x, vThreadID.x, l(0), t0.xxxx
ftod r0.xy, r0.x
dmul r0.xy, r0.xyxy, r0.xyxy
ishl r0.z, vThreadID.x, l(1)
store_structured u0.x, r0.z, l(0), r0.x
imad r0.x, vThreadID.x, l(2), l(1)
store_structured u0.x, r0.x, l(0), r0.y
ret
