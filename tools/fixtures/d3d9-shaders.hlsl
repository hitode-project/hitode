// Authored source for the D3D9 native/guest differential oracle.
// Explicit constant registers make the host-facing state contract unambiguous.
float4 scale : register(c0);
float4 bias : register(c1);
sampler2D image : register(s0);

struct Input { float3 position : POSITION0; float4 color : COLOR0; float2 uv : TEXCOORD0; };
struct Output { float4 position : POSITION0; float4 color : COLOR0; float2 uv : TEXCOORD0; };

Output vertex_main(Input input) {
    Output output;
    output.position = float4(input.position, 1.0);
    output.color = input.color;
    output.uv = input.uv;
    return output;
}

float4 pixel_color(Output input) : COLOR0 {
    return input.color * scale + bias;
}

float4 pixel_texture(Output input) : COLOR0 {
    return tex2D(image, input.uv) * scale + bias;
}

float4 pixel_flow(Output input) : COLOR0 {
    float4 result = input.color;
    [branch] if (input.uv.x > scale.x) {
        result.rgb = float3(1.0, 0.0, 0.0);
        [loop] for (int i = 0; i < 4; ++i) result.y += bias.y;
    }
    else result.rgb = float3(0.0, 0.0, 1.0);
    return result;
}
