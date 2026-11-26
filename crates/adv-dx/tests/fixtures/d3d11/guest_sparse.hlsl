struct Targets {
    float4 first : SV_Target0;
    float4 third : SV_Target2;
};
Targets main()
{
    Targets result;
    result.first = float4(1, 0, 0, 1);
    result.third = float4(0, 1, 0, 1);
    return result;
}
