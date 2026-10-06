//! SDK-typed adapters for all ID3DXEffect x86 vtable slots.
use super::*;
pub(super) fn register(registry: &mut Registry) {
    for api in [
        method_0_ABI,
        method_1_ABI,
        method_2_ABI,
        method_3_ABI,
        method_4_ABI,
        method_5_ABI,
        method_6_ABI,
        method_7_ABI,
        method_8_ABI,
        method_9_ABI,
        method_10_ABI,
        method_11_ABI,
        method_12_ABI,
        method_13_ABI,
        method_14_ABI,
        method_15_ABI,
        method_16_ABI,
        method_17_ABI,
        method_18_ABI,
        method_19_ABI,
        method_20_ABI,
        method_21_ABI,
        method_22_ABI,
        method_23_ABI,
        method_24_ABI,
        method_25_ABI,
        method_26_ABI,
        method_27_ABI,
        method_28_ABI,
        method_29_ABI,
        method_30_ABI,
        method_31_ABI,
        method_32_ABI,
        method_33_ABI,
        method_34_ABI,
        method_35_ABI,
        method_36_ABI,
        method_37_ABI,
        method_38_ABI,
        method_39_ABI,
        method_40_ABI,
        method_41_ABI,
        method_42_ABI,
        method_43_ABI,
        method_44_ABI,
        method_45_ABI,
        method_46_ABI,
        method_47_ABI,
        method_48_ABI,
        method_49_ABI,
        method_50_ABI,
        method_51_ABI,
        method_52_ABI,
        method_53_ABI,
        method_54_ABI,
        method_55_ABI,
        method_56_ABI,
        method_57_ABI,
        method_58_ABI,
        method_59_ABI,
        method_60_ABI,
        method_61_ABI,
        method_62_ABI,
        method_63_ABI,
        method_64_ABI,
        method_65_ABI,
        method_66_ABI,
        method_67_ABI,
        method_68_ABI,
        method_69_ABI,
        method_70_ABI,
        method_71_ABI,
        method_72_ABI,
        method_73_ABI,
        method_74_ABI,
        method_75_ABI,
        method_76_ABI,
        method_77_ABI,
        method_78_ABI,
    ] {
        registry.register(api);
    }
}
#[win32(d3dx9_43, name = "ID3DXEffect::QueryInterface", level = implement)]
fn method_0(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 0, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::AddRef", level = implement)]
fn method_1(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 1, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::Release", level = implement)]
fn method_2(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 2, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetDesc", level = implement)]
fn method_3(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 3, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetParameterDesc", level = implement)]
fn method_4(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 4, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetTechniqueDesc", level = implement)]
fn method_5(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 5, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetPassDesc", level = implement)]
fn method_6(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 6, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetFunctionDesc", level = implement)]
fn method_7(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 7, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetParameter", level = implement)]
fn method_8(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: u32) -> Outcome {
    dispatch(env, this.addr(), 8, &[arg0.addr(), arg1])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetParameterByName", level = implement)]
fn method_9(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: adv_abi::AnsiStr,
) -> Outcome {
    dispatch(env, this.addr(), 9, &[arg0.addr(), arg1.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetParameterBySemantic", level = implement)]
fn method_10(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: adv_abi::AnsiStr,
) -> Outcome {
    dispatch(env, this.addr(), 10, &[arg0.addr(), arg1.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetParameterElement", level = implement)]
fn method_11(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: u32) -> Outcome {
    dispatch(env, this.addr(), 11, &[arg0.addr(), arg1])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetTechnique", level = implement)]
fn method_12(env: &mut Env<'_>, this: GuestPtr<()>, arg0: u32) -> Outcome {
    dispatch(env, this.addr(), 12, &[arg0])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetTechniqueByName", level = implement)]
fn method_13(env: &mut Env<'_>, this: GuestPtr<()>, arg0: adv_abi::AnsiStr) -> Outcome {
    dispatch(env, this.addr(), 13, &[arg0.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetPass", level = implement)]
fn method_14(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: u32) -> Outcome {
    dispatch(env, this.addr(), 14, &[arg0.addr(), arg1])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetPassByName", level = implement)]
fn method_15(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: adv_abi::AnsiStr,
) -> Outcome {
    dispatch(env, this.addr(), 15, &[arg0.addr(), arg1.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetFunction", level = implement)]
fn method_16(env: &mut Env<'_>, this: GuestPtr<()>, arg0: u32) -> Outcome {
    dispatch(env, this.addr(), 16, &[arg0])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetFunctionByName", level = implement)]
fn method_17(env: &mut Env<'_>, this: GuestPtr<()>, arg0: adv_abi::AnsiStr) -> Outcome {
    dispatch(env, this.addr(), 17, &[arg0.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetAnnotation", level = implement)]
fn method_18(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: u32) -> Outcome {
    dispatch(env, this.addr(), 18, &[arg0.addr(), arg1])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetAnnotationByName", level = implement)]
fn method_19(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: adv_abi::AnsiStr,
) -> Outcome {
    dispatch(env, this.addr(), 19, &[arg0.addr(), arg1.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetValue", level = implement)]
fn method_20(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 20, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetValue", level = implement)]
fn method_21(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 21, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetBool", level = implement)]
fn method_22(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: i32) -> Outcome {
    dispatch(env, this.addr(), 22, &[arg0.addr(), arg1 as u32])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetBool", level = implement)]
fn method_23(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 23, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetBoolArray", level = implement)]
fn method_24(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 24, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetBoolArray", level = implement)]
fn method_25(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 25, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetInt", level = implement)]
fn method_26(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: i32) -> Outcome {
    dispatch(env, this.addr(), 26, &[arg0.addr(), arg1 as u32])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetInt", level = implement)]
fn method_27(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 27, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetIntArray", level = implement)]
fn method_28(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 28, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetIntArray", level = implement)]
fn method_29(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 29, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetFloat", level = implement)]
fn method_30(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: f32) -> Outcome {
    dispatch(env, this.addr(), 30, &[arg0.addr(), arg1.to_bits()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetFloat", level = implement)]
fn method_31(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 31, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetFloatArray", level = implement)]
fn method_32(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 32, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetFloatArray", level = implement)]
fn method_33(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 33, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetVector", level = implement)]
fn method_34(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 34, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetVector", level = implement)]
fn method_35(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 35, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetVectorArray", level = implement)]
fn method_36(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 36, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetVectorArray", level = implement)]
fn method_37(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 37, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrix", level = implement)]
fn method_38(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 38, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrix", level = implement)]
fn method_39(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 39, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrixArray", level = implement)]
fn method_40(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 40, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrixArray", level = implement)]
fn method_41(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 41, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrixPointerArray", level = implement)]
fn method_42(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 42, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrixPointerArray", level = implement)]
fn method_43(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 43, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrixTranspose", level = implement)]
fn method_44(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 44, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrixTranspose", level = implement)]
fn method_45(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 45, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrixTransposeArray", level = implement)]
fn method_46(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 46, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrixTransposeArray", level = implement)]
fn method_47(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 47, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetMatrixTransposePointerArray", level = implement)]
fn method_48(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 48, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetMatrixTransposePointerArray", level = implement)]
fn method_49(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 49, &[arg0.addr(), arg1.addr(), arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetString", level = implement)]
fn method_50(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: adv_abi::AnsiStr,
) -> Outcome {
    dispatch(env, this.addr(), 50, &[arg0.addr(), arg1.0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetString", level = implement)]
fn method_51(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 51, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetTexture", level = implement)]
fn method_52(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 52, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetTexture", level = implement)]
fn method_53(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 53, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetPixelShader", level = implement)]
fn method_54(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 54, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetVertexShader", level = implement)]
fn method_55(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 55, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetArrayRange", level = implement)]
fn method_56(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: u32,
    arg2: u32,
) -> Outcome {
    dispatch(env, this.addr(), 56, &[arg0.addr(), arg1, arg2])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetPool", level = implement)]
fn method_57(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 57, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetTechnique", level = implement)]
fn method_58(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 58, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetCurrentTechnique", level = implement)]
fn method_59(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 59, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::ValidateTechnique", level = implement)]
fn method_60(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 60, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::FindNextValidTechnique", level = implement)]
fn method_61(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 61, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::IsParameterUsed", level = implement)]
fn method_62(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 62, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::Begin", level = implement)]
fn method_63(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>, arg1: u32) -> Outcome {
    dispatch(env, this.addr(), 63, &[arg0.addr(), arg1])
}
#[win32(d3dx9_43, name = "ID3DXEffect::BeginPass", level = implement)]
fn method_64(env: &mut Env<'_>, this: GuestPtr<()>, arg0: u32) -> Outcome {
    dispatch(env, this.addr(), 64, &[arg0])
}
#[win32(d3dx9_43, name = "ID3DXEffect::CommitChanges", level = implement)]
fn method_65(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 65, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::EndPass", level = implement)]
fn method_66(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 66, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::End", level = implement)]
fn method_67(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 67, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetDevice", level = implement)]
fn method_68(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 68, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::OnLostDevice", level = implement)]
fn method_69(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 69, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::OnResetDevice", level = implement)]
fn method_70(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 70, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetStateManager", level = implement)]
fn method_71(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 71, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::GetStateManager", level = implement)]
fn method_72(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 72, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::BeginParameterBlock", level = implement)]
fn method_73(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 73, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::EndParameterBlock", level = implement)]
fn method_74(env: &mut Env<'_>, this: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 74, &[])
}
#[win32(d3dx9_43, name = "ID3DXEffect::ApplyParameterBlock", level = implement)]
fn method_75(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 75, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::DeleteParameterBlock", level = implement)]
fn method_76(env: &mut Env<'_>, this: GuestPtr<()>, arg0: GuestPtr<()>) -> Outcome {
    dispatch(env, this.addr(), 76, &[arg0.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::CloneEffect", level = implement)]
fn method_77(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
) -> Outcome {
    dispatch(env, this.addr(), 77, &[arg0.addr(), arg1.addr()])
}
#[win32(d3dx9_43, name = "ID3DXEffect::SetRawValue", level = implement)]
fn method_78(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    arg0: GuestPtr<()>,
    arg1: GuestPtr<()>,
    arg2: u32,
    arg3: u32,
) -> Outcome {
    dispatch(
        env,
        this.addr(),
        78,
        &[arg0.addr(), arg1.addr(), arg2, arg3],
    )
}
