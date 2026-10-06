// Authored binary Effect contract; the runtime consumes only fxc's FXO output.
float4 tone = float4(0.25, 0.5, 0.75, 1.0);
// Uniform arithmetic is emitted as a PRES program: add, reciprocal, multiply.
float4 direct_main() : COLOR0 { return (tone * 0.5) / (tone.x + 0.5); }
technique Primary {
    pass Color {
        VertexShader = NULL;
        PixelShader = compile ps_2_0 direct_main();
        ZEnable = FALSE;
        AlphaBlendEnable = FALSE;
        CullMode = NONE;
    }
}
