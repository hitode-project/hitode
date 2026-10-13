float4 settings : register(c0);
struct Input {float3 position:POSITION0;float4 color:COLOR0;};
struct Output {float4 position:POSITION0;float4 color:COLOR0;float fog:FOG;float size:PSIZE;};
Output vertex_main(Input v){Output o;o.position=float4(v.position,1);o.color=v.color;o.fog=settings.x;o.size=settings.y;return o;}

// Native software VP writes a driver-dependent colour layout when FOG and PSIZE
// are both output under clipping, so the clip case uses this position/colour
// entry and vertex_main checks fog and point size separately.
struct Basic {float4 position:POSITION0;float4 color:COLOR0;};
Basic vertex_clip(Input v){Basic o;o.position=float4(v.position,1);o.color=v.color;return o;}
