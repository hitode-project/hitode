use super::ValueType;
use crate::shader::dxbc::fields::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    Operation,
    Declaration,
    Flow,
    Custom,
    Reserved,
}

macro_rules! opcodes {
    ($($name:ident = $token:ident, $mnemonic:literal, $category:ident, [$($dst:ident),*], [$($src:ident),*];)*) => {
        const _: () = assert!([$(stringify!($name),)*].len() == D3D10_SB_NUM_OPCODES as usize);

        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Opcode {
            $($name,)*
            F32ToF16Rtz,
        }

        impl Opcode {
            pub(crate) fn decode(token: u32) -> Option<Self> {
                match token {
                    $($token => Some(Self::$name),)*
                    _ => None,
                }
            }

            pub fn mnemonic(self) -> &'static str {
                match self {
                    $(Self::$name => $mnemonic,)*
                    Self::F32ToF16Rtz => "f32tof16",
                }
            }

            pub(crate) fn category(self) -> Category {
                match self {
                    $(Self::$name => Category::$category,)*
                    Self::F32ToF16Rtz => Category::Operation,
                }
            }

            pub fn types(self) -> (&'static [ValueType], &'static [ValueType]) {
                use ValueType::*;
                match self {
                    $(Self::$name => (&[$($dst),*], &[$($src),*]),)*
                    Self::F32ToF16Rtz => (&[Uint], &[Float]),
                }
            }
        }
    };
}

opcodes! {
    Add = D3D10_SB_OPCODE_ADD, "add", Operation, [Float], [Float, Float];
    And = D3D10_SB_OPCODE_AND, "and", Operation, [Uint], [Uint, Uint];
    Break = D3D10_SB_OPCODE_BREAK, "break", Operation, [], [];
    Breakc = D3D10_SB_OPCODE_BREAKC, "breakc", Operation, [], [Uint];
    Call = D3D10_SB_OPCODE_CALL, "call", Operation, [], [Opaque];
    Callc = D3D10_SB_OPCODE_CALLC, "callc", Operation, [], [Opaque, Uint];
    Case = D3D10_SB_OPCODE_CASE, "case", Flow, [], [Uint];
    Continue = D3D10_SB_OPCODE_CONTINUE, "continue", Operation, [], [];
    Continuec = D3D10_SB_OPCODE_CONTINUEC, "continuec", Operation, [], [Uint];
    Cut = D3D10_SB_OPCODE_CUT, "cut", Operation, [], [];
    Default = D3D10_SB_OPCODE_DEFAULT, "default", Flow, [], [];
    DerivRtx = D3D10_SB_OPCODE_DERIV_RTX, "deriv_rtx", Operation, [Float], [Float];
    DerivRty = D3D10_SB_OPCODE_DERIV_RTY, "deriv_rty", Operation, [Float], [Float];
    Discard = D3D10_SB_OPCODE_DISCARD, "discard", Operation, [], [Uint];
    Div = D3D10_SB_OPCODE_DIV, "div", Operation, [Float], [Float, Float];
    Dp2 = D3D10_SB_OPCODE_DP2, "dp2", Operation, [Float], [Float, Float];
    Dp3 = D3D10_SB_OPCODE_DP3, "dp3", Operation, [Float], [Float, Float];
    Dp4 = D3D10_SB_OPCODE_DP4, "dp4", Operation, [Float], [Float, Float];
    Else = D3D10_SB_OPCODE_ELSE, "else", Flow, [], [];
    Emit = D3D10_SB_OPCODE_EMIT, "emit", Operation, [], [];
    Emitthencut = D3D10_SB_OPCODE_EMITTHENCUT, "emitthencut", Operation, [], [];
    Endif = D3D10_SB_OPCODE_ENDIF, "endif", Flow, [], [];
    Endloop = D3D10_SB_OPCODE_ENDLOOP, "endloop", Flow, [], [];
    Endswitch = D3D10_SB_OPCODE_ENDSWITCH, "endswitch", Flow, [], [];
    Eq = D3D10_SB_OPCODE_EQ, "eq", Operation, [Uint], [Float, Float];
    Exp = D3D10_SB_OPCODE_EXP, "exp", Operation, [Float], [Float];
    Frc = D3D10_SB_OPCODE_FRC, "frc", Operation, [Float], [Float];
    Ftoi = D3D10_SB_OPCODE_FTOI, "ftoi", Operation, [Int], [Float];
    Ftou = D3D10_SB_OPCODE_FTOU, "ftou", Operation, [Uint], [Float];
    Ge = D3D10_SB_OPCODE_GE, "ge", Operation, [Uint], [Float, Float];
    Iadd = D3D10_SB_OPCODE_IADD, "iadd", Operation, [Int], [Int, Int];
    If = D3D10_SB_OPCODE_IF, "if", Flow, [], [Uint];
    Ieq = D3D10_SB_OPCODE_IEQ, "ieq", Operation, [Uint], [Int, Int];
    Ige = D3D10_SB_OPCODE_IGE, "ige", Operation, [Uint], [Int, Int];
    Ilt = D3D10_SB_OPCODE_ILT, "ilt", Operation, [Uint], [Int, Int];
    Imad = D3D10_SB_OPCODE_IMAD, "imad", Operation, [Int], [Int, Int, Int];
    Imax = D3D10_SB_OPCODE_IMAX, "imax", Operation, [Int], [Int, Int];
    Imin = D3D10_SB_OPCODE_IMIN, "imin", Operation, [Int], [Int, Int];
    Imul = D3D10_SB_OPCODE_IMUL, "imul", Operation, [Int, Int], [Int, Int];
    Ine = D3D10_SB_OPCODE_INE, "ine", Operation, [Uint], [Int, Int];
    Ineg = D3D10_SB_OPCODE_INEG, "ineg", Operation, [Int], [Int];
    Ishl = D3D10_SB_OPCODE_ISHL, "ishl", Operation, [Int], [Int, Int];
    Ishr = D3D10_SB_OPCODE_ISHR, "ishr", Operation, [Int], [Int, Int];
    Itof = D3D10_SB_OPCODE_ITOF, "itof", Operation, [Float], [Int];
    Label = D3D10_SB_OPCODE_LABEL, "label", Operation, [], [Opaque];
    Ld = D3D10_SB_OPCODE_LD, "ld", Operation, [Uint], [Int, Binding];
    LdMs = D3D10_SB_OPCODE_LD_MS, "ld_ms", Operation, [Uint], [Int, Binding, Int];
    Log = D3D10_SB_OPCODE_LOG, "log", Operation, [Float], [Float];
    Loop = D3D10_SB_OPCODE_LOOP, "loop", Flow, [], [];
    Lt = D3D10_SB_OPCODE_LT, "lt", Operation, [Uint], [Float, Float];
    Mad = D3D10_SB_OPCODE_MAD, "mad", Operation, [Float], [Float, Float, Float];
    Min = D3D10_SB_OPCODE_MIN, "min", Operation, [Float], [Float, Float];
    Max = D3D10_SB_OPCODE_MAX, "max", Operation, [Float], [Float, Float];
    Customdata = D3D10_SB_OPCODE_CUSTOMDATA, "customdata", Custom, [], [];
    Mov = D3D10_SB_OPCODE_MOV, "mov", Operation, [Bits], [Bits];
    Movc = D3D10_SB_OPCODE_MOVC, "movc", Operation, [Bits], [Uint, Bits, Bits];
    Mul = D3D10_SB_OPCODE_MUL, "mul", Operation, [Float], [Float, Float];
    Ne = D3D10_SB_OPCODE_NE, "ne", Operation, [Uint], [Float, Float];
    Nop = D3D10_SB_OPCODE_NOP, "nop", Operation, [], [];
    Not = D3D10_SB_OPCODE_NOT, "not", Operation, [Uint], [Uint];
    Or = D3D10_SB_OPCODE_OR, "or", Operation, [Uint], [Uint, Uint];
    Resinfo = D3D10_SB_OPCODE_RESINFO, "resinfo", Operation, [Float], [Int, Binding];
    Ret = D3D10_SB_OPCODE_RET, "ret", Operation, [], [];
    Retc = D3D10_SB_OPCODE_RETC, "retc", Operation, [], [Uint];
    RoundNe = D3D10_SB_OPCODE_ROUND_NE, "round_ne", Operation, [Float], [Float];
    RoundNi = D3D10_SB_OPCODE_ROUND_NI, "round_ni", Operation, [Float], [Float];
    RoundPi = D3D10_SB_OPCODE_ROUND_PI, "round_pi", Operation, [Float], [Float];
    RoundZ = D3D10_SB_OPCODE_ROUND_Z, "round_z", Operation, [Float], [Float];
    Rsq = D3D10_SB_OPCODE_RSQ, "rsq", Operation, [Float], [Float];
    Sample = D3D10_SB_OPCODE_SAMPLE, "sample", Operation, [Uint], [Float, Binding, Binding];
    SampleC = D3D10_SB_OPCODE_SAMPLE_C, "sample_c", Operation, [Float], [Float, Binding, Binding, Float];
    SampleCLz = D3D10_SB_OPCODE_SAMPLE_C_LZ, "sample_c_lz", Operation, [Float], [Float, Binding, Binding, Float];
    SampleL = D3D10_SB_OPCODE_SAMPLE_L, "sample_l", Operation, [Uint], [Float, Binding, Binding, Float];
    SampleD = D3D10_SB_OPCODE_SAMPLE_D, "sample_d", Operation, [Uint], [Float, Binding, Binding, Float, Float];
    SampleB = D3D10_SB_OPCODE_SAMPLE_B, "sample_b", Operation, [Uint], [Float, Binding, Binding, Float];
    Sqrt = D3D10_SB_OPCODE_SQRT, "sqrt", Operation, [Float], [Float];
    Switch = D3D10_SB_OPCODE_SWITCH, "switch", Flow, [], [Int];
    Sincos = D3D10_SB_OPCODE_SINCOS, "sincos", Operation, [Float, Float], [Float];
    Udiv = D3D10_SB_OPCODE_UDIV, "udiv", Operation, [Uint, Uint], [Uint, Uint];
    Ult = D3D10_SB_OPCODE_ULT, "ult", Operation, [Uint], [Uint, Uint];
    Uge = D3D10_SB_OPCODE_UGE, "uge", Operation, [Uint], [Uint, Uint];
    Umul = D3D10_SB_OPCODE_UMUL, "umul", Operation, [Uint, Uint], [Uint, Uint];
    Umad = D3D10_SB_OPCODE_UMAD, "umad", Operation, [Uint], [Uint, Uint, Uint];
    Umax = D3D10_SB_OPCODE_UMAX, "umax", Operation, [Uint], [Uint, Uint];
    Umin = D3D10_SB_OPCODE_UMIN, "umin", Operation, [Uint], [Uint, Uint];
    Ushr = D3D10_SB_OPCODE_USHR, "ushr", Operation, [Uint], [Uint, Uint];
    Utof = D3D10_SB_OPCODE_UTOF, "utof", Operation, [Float], [Uint];
    Xor = D3D10_SB_OPCODE_XOR, "xor", Operation, [Uint], [Uint, Uint];
    DclResource = D3D10_SB_OPCODE_DCL_RESOURCE, "dcl_resource", Declaration, [], [];
    DclConstantBuffer = D3D10_SB_OPCODE_DCL_CONSTANT_BUFFER, "dcl_constant_buffer", Declaration, [], [];
    DclSampler = D3D10_SB_OPCODE_DCL_SAMPLER, "dcl_sampler", Declaration, [], [];
    DclIndexRange = D3D10_SB_OPCODE_DCL_INDEX_RANGE, "dcl_index_range", Declaration, [], [];
    DclGsOutputPrimitiveTopology = D3D10_SB_OPCODE_DCL_GS_OUTPUT_PRIMITIVE_TOPOLOGY, "dcl_gs_output_primitive_topology", Declaration, [], [];
    DclGsInputPrimitive = D3D10_SB_OPCODE_DCL_GS_INPUT_PRIMITIVE, "dcl_gs_input_primitive", Declaration, [], [];
    DclMaxOutputVertexCount = D3D10_SB_OPCODE_DCL_MAX_OUTPUT_VERTEX_COUNT, "dcl_max_output_vertex_count", Declaration, [], [];
    DclInput = D3D10_SB_OPCODE_DCL_INPUT, "dcl_input", Declaration, [], [];
    DclInputSgv = D3D10_SB_OPCODE_DCL_INPUT_SGV, "dcl_input_sgv", Declaration, [], [];
    DclInputSiv = D3D10_SB_OPCODE_DCL_INPUT_SIV, "dcl_input_siv", Declaration, [], [];
    DclInputPs = D3D10_SB_OPCODE_DCL_INPUT_PS, "dcl_input_ps", Declaration, [], [];
    DclInputPsSgv = D3D10_SB_OPCODE_DCL_INPUT_PS_SGV, "dcl_input_ps_sgv", Declaration, [], [];
    DclInputPsSiv = D3D10_SB_OPCODE_DCL_INPUT_PS_SIV, "dcl_input_ps_siv", Declaration, [], [];
    DclOutput = D3D10_SB_OPCODE_DCL_OUTPUT, "dcl_output", Declaration, [], [];
    DclOutputSgv = D3D10_SB_OPCODE_DCL_OUTPUT_SGV, "dcl_output_sgv", Declaration, [], [];
    DclOutputSiv = D3D10_SB_OPCODE_DCL_OUTPUT_SIV, "dcl_output_siv", Declaration, [], [];
    DclTemps = D3D10_SB_OPCODE_DCL_TEMPS, "dcl_temps", Declaration, [], [];
    DclIndexableTemp = D3D10_SB_OPCODE_DCL_INDEXABLE_TEMP, "dcl_indexable_temp", Declaration, [], [];
    DclGlobalFlags = D3D10_SB_OPCODE_DCL_GLOBAL_FLAGS, "dcl_global_flags", Declaration, [], [];
    Reserved107 = D3D10_SB_OPCODE_RESERVED0, "reserved0", Reserved, [], [];
    Lod = D3D10_1_SB_OPCODE_LOD, "lod", Operation, [Float], [Float, Binding, Binding];
    Gather4 = D3D10_1_SB_OPCODE_GATHER4, "gather4", Operation, [Uint], [Float, Binding, Binding];
    SamplePos = D3D10_1_SB_OPCODE_SAMPLE_POS, "sample_pos", Operation, [Float], [Binding, Uint];
    SampleInfo = D3D10_1_SB_OPCODE_SAMPLE_INFO, "sample_info", Operation, [Float], [Binding];
    Reserved112 = D3D10_1_SB_OPCODE_RESERVED1, "reserved1", Reserved, [], [];
    HsDecls = D3D11_SB_OPCODE_HS_DECLS, "hs_decls", Operation, [], [];
    HsControlPointPhase = D3D11_SB_OPCODE_HS_CONTROL_POINT_PHASE, "hs_control_point_phase", Operation, [], [];
    HsForkPhase = D3D11_SB_OPCODE_HS_FORK_PHASE, "hs_fork_phase", Operation, [], [];
    HsJoinPhase = D3D11_SB_OPCODE_HS_JOIN_PHASE, "hs_join_phase", Operation, [], [];
    EmitStream = D3D11_SB_OPCODE_EMIT_STREAM, "emit_stream", Operation, [], [Binding];
    CutStream = D3D11_SB_OPCODE_CUT_STREAM, "cut_stream", Operation, [], [Binding];
    EmitthencutStream = D3D11_SB_OPCODE_EMITTHENCUT_STREAM, "emitthencut_stream", Operation, [], [Binding];
    InterfaceCall = D3D11_SB_OPCODE_INTERFACE_CALL, "interface_call", Operation, [], [Opaque];
    Bufinfo = D3D11_SB_OPCODE_BUFINFO, "bufinfo", Operation, [Int], [Binding];
    DerivRtxCoarse = D3D11_SB_OPCODE_DERIV_RTX_COARSE, "deriv_rtx_coarse", Operation, [Float], [Float];
    DerivRtxFine = D3D11_SB_OPCODE_DERIV_RTX_FINE, "deriv_rtx_fine", Operation, [Float], [Float];
    DerivRtyCoarse = D3D11_SB_OPCODE_DERIV_RTY_COARSE, "deriv_rty_coarse", Operation, [Float], [Float];
    DerivRtyFine = D3D11_SB_OPCODE_DERIV_RTY_FINE, "deriv_rty_fine", Operation, [Float], [Float];
    Gather4C = D3D11_SB_OPCODE_GATHER4_C, "gather4_c", Operation, [Float], [Float, Binding, Binding, Float];
    Gather4Po = D3D11_SB_OPCODE_GATHER4_PO, "gather4_po", Operation, [Float], [Float, Int, Binding, Binding];
    Gather4PoC = D3D11_SB_OPCODE_GATHER4_PO_C, "gather4_po_c", Operation, [Float], [Float, Int, Binding, Binding, Float];
    Rcp = D3D11_SB_OPCODE_RCP, "rcp", Operation, [Float], [Float];
    F32ToF16 = D3D11_SB_OPCODE_F32TOF16, "f32tof16", Operation, [Uint], [Float];
    F16ToF32 = D3D11_SB_OPCODE_F16TOF32, "f16tof32", Operation, [Float], [Uint];
    Uaddc = D3D11_SB_OPCODE_UADDC, "uaddc", Operation, [Uint, Uint], [Uint, Uint];
    Usubb = D3D11_SB_OPCODE_USUBB, "usubb", Operation, [Uint, Uint], [Uint, Uint];
    Countbits = D3D11_SB_OPCODE_COUNTBITS, "countbits", Operation, [Uint], [Uint];
    FirstbitHi = D3D11_SB_OPCODE_FIRSTBIT_HI, "firstbit_hi", Operation, [Uint], [Uint];
    FirstbitLo = D3D11_SB_OPCODE_FIRSTBIT_LO, "firstbit_lo", Operation, [Uint], [Uint];
    FirstbitShi = D3D11_SB_OPCODE_FIRSTBIT_SHI, "firstbit_shi", Operation, [Uint], [Int];
    Ubfe = D3D11_SB_OPCODE_UBFE, "ubfe", Operation, [Uint], [Int, Int, Uint];
    Ibfe = D3D11_SB_OPCODE_IBFE, "ibfe", Operation, [Int], [Int, Int, Int];
    Bfi = D3D11_SB_OPCODE_BFI, "bfi", Operation, [Uint], [Int, Int, Uint, Uint];
    Bfrev = D3D11_SB_OPCODE_BFREV, "bfrev", Operation, [Uint], [Uint];
    Swapc = D3D11_SB_OPCODE_SWAPC, "swapc", Operation, [Bits, Bits], [Uint, Bits, Bits];
    DclStream = D3D11_SB_OPCODE_DCL_STREAM, "dcl_stream", Declaration, [], [Binding];
    DclFunctionBody = D3D11_SB_OPCODE_DCL_FUNCTION_BODY, "dcl_function_body", Declaration, [], [];
    DclFunctionTable = D3D11_SB_OPCODE_DCL_FUNCTION_TABLE, "dcl_function_table", Declaration, [], [];
    DclInterface = D3D11_SB_OPCODE_DCL_INTERFACE, "dcl_interface", Declaration, [], [];
    DclInputControlPointCount = D3D11_SB_OPCODE_DCL_INPUT_CONTROL_POINT_COUNT, "dcl_input_control_point_count", Declaration, [], [];
    DclOutputControlPointCount = D3D11_SB_OPCODE_DCL_OUTPUT_CONTROL_POINT_COUNT, "dcl_output_control_point_count", Declaration, [], [];
    DclTessDomain = D3D11_SB_OPCODE_DCL_TESS_DOMAIN, "dcl_tess_domain", Declaration, [], [];
    DclTessPartitioning = D3D11_SB_OPCODE_DCL_TESS_PARTITIONING, "dcl_tess_partitioning", Declaration, [], [];
    DclTessOutputPrimitive = D3D11_SB_OPCODE_DCL_TESS_OUTPUT_PRIMITIVE, "dcl_tess_output_primitive", Declaration, [], [];
    DclHsMaxTessfactor = D3D11_SB_OPCODE_DCL_HS_MAX_TESSFACTOR, "dcl_hs_max_tessfactor", Declaration, [], [];
    DclHsForkPhaseInstanceCount = D3D11_SB_OPCODE_DCL_HS_FORK_PHASE_INSTANCE_COUNT, "dcl_hs_fork_phase_instance_count", Declaration, [], [];
    DclHsJoinPhaseInstanceCount = D3D11_SB_OPCODE_DCL_HS_JOIN_PHASE_INSTANCE_COUNT, "dcl_hs_join_phase_instance_count", Declaration, [], [];
    DclThreadGroup = D3D11_SB_OPCODE_DCL_THREAD_GROUP, "dcl_thread_group", Declaration, [], [];
    DclUnorderedAccessViewTyped = D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_TYPED, "dcl_unordered_access_view_typed", Declaration, [], [];
    DclUnorderedAccessViewRaw = D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_RAW, "dcl_unordered_access_view_raw", Declaration, [], [];
    DclUnorderedAccessViewStructured = D3D11_SB_OPCODE_DCL_UNORDERED_ACCESS_VIEW_STRUCTURED, "dcl_unordered_access_view_structured", Declaration, [], [];
    DclThreadGroupSharedMemoryRaw = D3D11_SB_OPCODE_DCL_THREAD_GROUP_SHARED_MEMORY_RAW, "dcl_thread_group_shared_memory_raw", Declaration, [], [];
    DclThreadGroupSharedMemoryStructured = D3D11_SB_OPCODE_DCL_THREAD_GROUP_SHARED_MEMORY_STRUCTURED, "dcl_thread_group_shared_memory_structured", Declaration, [], [];
    DclResourceRaw = D3D11_SB_OPCODE_DCL_RESOURCE_RAW, "dcl_resource_raw", Declaration, [], [];
    DclResourceStructured = D3D11_SB_OPCODE_DCL_RESOURCE_STRUCTURED, "dcl_resource_structured", Declaration, [], [];
    LdUavTyped = D3D11_SB_OPCODE_LD_UAV_TYPED, "ld_uav_typed", Operation, [Uint], [Int, Binding];
    StoreUavTyped = D3D11_SB_OPCODE_STORE_UAV_TYPED, "store_uav_typed", Operation, [Binding], [Int, Uint];
    LdRaw = D3D11_SB_OPCODE_LD_RAW, "ld_raw", Operation, [Uint], [Int, Binding];
    StoreRaw = D3D11_SB_OPCODE_STORE_RAW, "store_raw", Operation, [Binding], [Uint, Uint];
    LdStructured = D3D11_SB_OPCODE_LD_STRUCTURED, "ld_structured", Operation, [Uint], [Int, Int, Binding];
    StoreStructured = D3D11_SB_OPCODE_STORE_STRUCTURED, "store_structured", Operation, [Binding], [Int, Int, Uint];
    AtomicAnd = D3D11_SB_OPCODE_ATOMIC_AND, "atomic_and", Operation, [Binding], [Int, Uint];
    AtomicOr = D3D11_SB_OPCODE_ATOMIC_OR, "atomic_or", Operation, [Binding], [Int, Uint];
    AtomicXor = D3D11_SB_OPCODE_ATOMIC_XOR, "atomic_xor", Operation, [Binding], [Int, Uint];
    AtomicCmpStore = D3D11_SB_OPCODE_ATOMIC_CMP_STORE, "atomic_cmp_store", Operation, [Binding], [Int, Uint, Uint];
    AtomicIadd = D3D11_SB_OPCODE_ATOMIC_IADD, "atomic_iadd", Operation, [Binding], [Int, Int];
    AtomicImax = D3D11_SB_OPCODE_ATOMIC_IMAX, "atomic_imax", Operation, [Binding], [Int, Int];
    AtomicImin = D3D11_SB_OPCODE_ATOMIC_IMIN, "atomic_imin", Operation, [Binding], [Int, Int];
    AtomicUmax = D3D11_SB_OPCODE_ATOMIC_UMAX, "atomic_umax", Operation, [Binding], [Int, Uint];
    AtomicUmin = D3D11_SB_OPCODE_ATOMIC_UMIN, "atomic_umin", Operation, [Binding], [Int, Uint];
    ImmAtomicAlloc = D3D11_SB_OPCODE_IMM_ATOMIC_ALLOC, "imm_atomic_alloc", Operation, [Uint], [Binding];
    ImmAtomicConsume = D3D11_SB_OPCODE_IMM_ATOMIC_CONSUME, "imm_atomic_consume", Operation, [Uint], [Binding];
    ImmAtomicIadd = D3D11_SB_OPCODE_IMM_ATOMIC_IADD, "imm_atomic_iadd", Operation, [Uint, Binding], [Int, Int];
    ImmAtomicAnd = D3D11_SB_OPCODE_IMM_ATOMIC_AND, "imm_atomic_and", Operation, [Uint, Binding], [Int, Uint];
    ImmAtomicOr = D3D11_SB_OPCODE_IMM_ATOMIC_OR, "imm_atomic_or", Operation, [Uint, Binding], [Int, Uint];
    ImmAtomicXor = D3D11_SB_OPCODE_IMM_ATOMIC_XOR, "imm_atomic_xor", Operation, [Uint, Binding], [Int, Uint];
    ImmAtomicExch = D3D11_SB_OPCODE_IMM_ATOMIC_EXCH, "imm_atomic_exch", Operation, [Uint, Binding], [Int, Uint];
    ImmAtomicCmpExch = D3D11_SB_OPCODE_IMM_ATOMIC_CMP_EXCH, "imm_atomic_cmp_exch", Operation, [Uint, Binding], [Int, Uint, Uint];
    ImmAtomicImax = D3D11_SB_OPCODE_IMM_ATOMIC_IMAX, "imm_atomic_imax", Operation, [Int, Binding], [Int, Int];
    ImmAtomicImin = D3D11_SB_OPCODE_IMM_ATOMIC_IMIN, "imm_atomic_imin", Operation, [Int, Binding], [Int, Int];
    ImmAtomicUmax = D3D11_SB_OPCODE_IMM_ATOMIC_UMAX, "imm_atomic_umax", Operation, [Uint, Binding], [Int, Uint];
    ImmAtomicUmin = D3D11_SB_OPCODE_IMM_ATOMIC_UMIN, "imm_atomic_umin", Operation, [Uint, Binding], [Int, Uint];
    Sync = D3D11_SB_OPCODE_SYNC, "sync", Operation, [], [];
    Dadd = D3D11_SB_OPCODE_DADD, "dadd", Operation, [Double], [Double, Double];
    Dmax = D3D11_SB_OPCODE_DMAX, "dmax", Operation, [Double], [Double, Double];
    Dmin = D3D11_SB_OPCODE_DMIN, "dmin", Operation, [Double], [Double, Double];
    Dmul = D3D11_SB_OPCODE_DMUL, "dmul", Operation, [Double], [Double, Double];
    Deq = D3D11_SB_OPCODE_DEQ, "deq", Operation, [Uint], [Double, Double];
    Dge = D3D11_SB_OPCODE_DGE, "dge", Operation, [Uint], [Double, Double];
    Dlt = D3D11_SB_OPCODE_DLT, "dlt", Operation, [Uint], [Double, Double];
    Dne = D3D11_SB_OPCODE_DNE, "dne", Operation, [Uint], [Double, Double];
    Dmov = D3D11_SB_OPCODE_DMOV, "dmov", Operation, [Double], [Double];
    Dmovc = D3D11_SB_OPCODE_DMOVC, "dmovc", Operation, [Double], [Uint, Double, Double];
    Dtof = D3D11_SB_OPCODE_DTOF, "dtof", Operation, [Float], [Double];
    Ftod = D3D11_SB_OPCODE_FTOD, "ftod", Operation, [Double], [Float];
    EvalSnapped = D3D11_SB_OPCODE_EVAL_SNAPPED, "eval_snapped", Operation, [Float], [Float, Int];
    EvalSampleIndex = D3D11_SB_OPCODE_EVAL_SAMPLE_INDEX, "eval_sample_index", Operation, [Float], [Float, Int];
    EvalCentroid = D3D11_SB_OPCODE_EVAL_CENTROID, "eval_centroid", Operation, [Float], [Float];
    DclGsInstanceCount = D3D11_SB_OPCODE_DCL_GS_INSTANCE_COUNT, "dcl_gs_instance_count", Declaration, [], [];
    Abort = D3D11_SB_OPCODE_ABORT, "abort", Operation, [], [];
    DebugBreak = D3D11_SB_OPCODE_DEBUG_BREAK, "debug_break", Operation, [], [];
    Reserved209 = D3D11_SB_OPCODE_RESERVED0, "reserved0", Reserved, [], [];
    Ddiv = D3D11_1_SB_OPCODE_DDIV, "ddiv", Operation, [Double], [Double, Double];
    Dfma = D3D11_1_SB_OPCODE_DFMA, "dfma", Operation, [Double], [Double, Double, Double];
    Drcp = D3D11_1_SB_OPCODE_DRCP, "drcp", Operation, [Double], [Double];
    Msad = D3D11_1_SB_OPCODE_MSAD, "msad", Operation, [Uint], [Uint, Uint, Uint];
    Dtoi = D3D11_1_SB_OPCODE_DTOI, "dtoi", Operation, [Int], [Double];
    Dtou = D3D11_1_SB_OPCODE_DTOU, "dtou", Operation, [Uint], [Double];
    Itod = D3D11_1_SB_OPCODE_ITOD, "itod", Operation, [Double], [Int];
    Utod = D3D11_1_SB_OPCODE_UTOD, "utod", Operation, [Double], [Uint];
    Reserved218 = D3D11_1_SB_OPCODE_RESERVED0, "reserved0", Reserved, [], [];
    Gather4Feedback = D3DWDDM1_3_SB_OPCODE_GATHER4_FEEDBACK, "gather4_feedback", Operation, [Uint, Uint], [Float, Binding, Binding];
    Gather4CFeedback = D3DWDDM1_3_SB_OPCODE_GATHER4_C_FEEDBACK, "gather4_c_feedback", Operation, [Float, Uint], [Float, Binding, Binding, Float];
    Gather4PoFeedback = D3DWDDM1_3_SB_OPCODE_GATHER4_PO_FEEDBACK, "gather4_po_feedback", Operation, [Float, Uint], [Float, Int, Binding, Binding];
    Gather4PoCFeedback = D3DWDDM1_3_SB_OPCODE_GATHER4_PO_C_FEEDBACK, "gather4_po_c_feedback", Operation, [Float, Uint], [Float, Int, Binding, Binding, Float];
    LdFeedback = D3DWDDM1_3_SB_OPCODE_LD_FEEDBACK, "ld_feedback", Operation, [Uint, Uint], [Int, Binding];
    LdMsFeedback = D3DWDDM1_3_SB_OPCODE_LD_MS_FEEDBACK, "ld_ms_feedback", Operation, [Uint, Uint], [Int, Binding, Int];
    LdUavTypedFeedback = D3DWDDM1_3_SB_OPCODE_LD_UAV_TYPED_FEEDBACK, "ld_uav_typed_feedback", Operation, [Uint, Uint], [Int, Binding];
    LdRawFeedback = D3DWDDM1_3_SB_OPCODE_LD_RAW_FEEDBACK, "ld_raw_feedback", Operation, [Uint, Uint], [Int, Binding];
    LdStructuredFeedback = D3DWDDM1_3_SB_OPCODE_LD_STRUCTURED_FEEDBACK, "ld_structured_feedback", Operation, [Uint, Uint], [Int, Int, Binding];
    SampleLFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_L_FEEDBACK, "sample_l_feedback", Operation, [Uint, Uint], [Float, Binding, Binding, Float];
    SampleCLzFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_C_LZ_FEEDBACK, "sample_c_lz_feedback", Operation, [Float, Uint], [Float, Binding, Binding, Float];
    SampleClampFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_CLAMP_FEEDBACK, "sample_clamp_feedback", Operation, [Uint, Uint], [Float, Binding, Binding, Float];
    SampleBClampFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_B_CLAMP_FEEDBACK, "sample_b_clamp_feedback", Operation, [Uint, Uint], [Float, Binding, Binding, Float, Float];
    SampleDClampFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_D_CLAMP_FEEDBACK, "sample_d_clamp_feedback", Operation, [Uint, Uint], [Float, Binding, Binding, Float, Float, Float];
    SampleCClampFeedback = D3DWDDM1_3_SB_OPCODE_SAMPLE_C_CLAMP_FEEDBACK, "sample_c_clamp_feedback", Operation, [Float, Uint], [Float, Binding, Binding, Float, Float];
    CheckAccessFullyMapped = D3DWDDM1_3_SB_OPCODE_CHECK_ACCESS_FULLY_MAPPED, "check_access_fully_mapped", Operation, [Uint], [Uint];
    Reserved235 = D3DWDDM1_3_SB_OPCODE_RESERVED0, "reserved0", Reserved, [], [];
}

impl Opcode {
    pub(crate) fn is_atomic(self) -> bool {
        use Opcode::*;
        matches!(
            self,
            AtomicAnd
                | AtomicOr
                | AtomicXor
                | AtomicCmpStore
                | AtomicIadd
                | AtomicImax
                | AtomicImin
                | AtomicUmax
                | AtomicUmin
                | ImmAtomicIadd
                | ImmAtomicAnd
                | ImmAtomicOr
                | ImmAtomicXor
                | ImmAtomicExch
                | ImmAtomicCmpExch
                | ImmAtomicImax
                | ImmAtomicImin
                | ImmAtomicUmax
                | ImmAtomicUmin
        )
    }
}
