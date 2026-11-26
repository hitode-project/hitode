Texture2D t : register(t0);
cbuffer Coordinates : register(b0) { float4 parameters; };
float4 main(float4 p : SV_Position) : SV_Target
{
    int x = int(p.x) * int(parameters.x) + int(parameters.y);
    return t.Load(int3(x, int(parameters.z), int(parameters.w)));
}
