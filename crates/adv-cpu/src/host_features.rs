//! Process-wide OS-advertised ARM64 capabilities, independent of guest CPUID.
use std::fmt;
use std::sync::OnceLock;

/// Capabilities safe to use on every CPU allowed by the host OS.
/// This is availability data only; it does not select an execution policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostFeatures(u32);

const NAMES: [(&str, HostFeatures); 16] = [
    ("flagm", HostFeatures::FLAGM),
    ("flagm2", HostFeatures::FLAGM2),
    ("afp", HostFeatures::AFP),
    ("lse", HostFeatures::LSE),
    ("lse2", HostFeatures::LSE2),
    ("rcpc", HostFeatures::RCPC),
    ("fp16", HostFeatures::FP16),
    ("bf16", HostFeatures::BF16),
    ("dotprod", HostFeatures::DOTPROD),
    ("i8mm", HostFeatures::I8MM),
    ("sve", HostFeatures::SVE),
    ("sve2", HostFeatures::SVE2),
    ("sme", HostFeatures::SME),
    ("pauth", HostFeatures::PAUTH),
    ("pacg", HostFeatures::PACG),
    ("bti", HostFeatures::BTI),
];

struct Snapshot {
    detected: HostFeatures,
    enabled: HostFeatures,
}

fn snapshot() -> &'static Snapshot {
    static FEATURES: OnceLock<Snapshot> = OnceLock::new();
    FEATURES.get_or_init(|| {
        let detected = detect();
        let enabled =
            std::env::var("ADV_HOST_FEATURES").map_or(detected, |value| detected.disable(&value));
        Snapshot { detected, enabled }
    })
}

impl HostFeatures {
    pub const EMPTY: Self = Self(0);
    pub const FLAGM: Self = Self(1 << 0);
    pub const FLAGM2: Self = Self(1 << 1);
    pub const AFP: Self = Self(1 << 2);
    pub const LSE: Self = Self(1 << 3);
    /// Linux HWCAP_USCAT (FEAT_LSE2), not an alias for FEAT_LSE.
    pub const LSE2: Self = Self(1 << 4);
    pub const RCPC: Self = Self(1 << 5);
    /// Both scalar and Advanced SIMD half-precision arithmetic.
    pub const FP16: Self = Self(1 << 6);
    pub const BF16: Self = Self(1 << 7);
    pub const DOTPROD: Self = Self(1 << 8);
    pub const I8MM: Self = Self(1 << 9);
    pub const SVE: Self = Self(1 << 10);
    pub const SVE2: Self = Self(1 << 11);
    pub const SME: Self = Self(1 << 12);
    /// Address authentication (Linux PACA); generic PACG is separate.
    pub const PAUTH: Self = Self(1 << 13);
    pub const PACG: Self = Self(1 << 14);
    pub const BTI: Self = Self(1 << 15);

    /// Effective capabilities, sampled once including the disable-only override.
    pub fn get() -> Self {
        snapshot().enabled
    }

    /// OS capabilities before the diagnostic override, from the same snapshot.
    pub fn detected() -> Self {
        snapshot().detected
    }

    pub const fn contains(self, feature: Self) -> bool {
        self.0 & feature.0 == feature.0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[cfg(any(
        test,
        all(
            target_arch = "aarch64",
            any(target_os = "linux", target_os = "android", target_os = "macos")
        )
    ))]
    fn insert(&mut self, feature: Self, available: bool) {
        if available {
            self.0 |= feature.0;
        }
    }

    fn disable(mut self, value: &str) -> Self {
        for token in value.split(',') {
            let token = token.trim().to_ascii_lowercase();
            let Some(name) = token.strip_prefix('-') else {
                continue;
            };
            let name = match name {
                "uscat" => "lse2",
                "paca" => "pauth",
                other => other,
            };
            if let Some((_, feature)) = NAMES.iter().find(|(key, _)| *key == name) {
                self.0 &= !feature.0;
            }
        }
        // A stronger extension must not bypass a disabled prerequisite.
        if !self.contains(Self::FLAGM) {
            self.0 &= !Self::FLAGM2.0;
        }
        if !self.contains(Self::SVE) {
            self.0 &= !Self::SVE2.0;
        }
        self
    }
}

impl fmt::Display for HostFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for (name, feature) in NAMES {
            if self.contains(feature) {
                write!(f, "{separator}{name}")?;
                separator = ",";
            }
        }
        if self.is_empty() {
            f.write_str("none")?;
        }
        Ok(())
    }
}

// Bit positions come from Linux UAPI arch/arm64/include/uapi/asm/hwcap.h:
// https://github.com/torvalds/linux/blob/v6.12/arch/arm64/include/uapi/asm/hwcap.h
// FP16 requires FPHP and ASIMDHP, matching Rust std_detect and Cranelift native.
#[cfg(any(
    test,
    all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android")
    )
))]
fn from_auxv(hwcap: u64, hwcap2: u64) -> HostFeatures {
    let mut features = HostFeatures::EMPTY;
    for (feature, mask) in [
        (HostFeatures::LSE, 1 << 8),                // HWCAP_ATOMICS
        (HostFeatures::FP16, (1 << 9) | (1 << 10)), // FPHP | ASIMDHP
        (HostFeatures::RCPC, 1 << 15),              // HWCAP_LRCPC
        (HostFeatures::DOTPROD, 1 << 20),           // HWCAP_ASIMDDP
        (HostFeatures::SVE, 1 << 22),               // HWCAP_SVE
        (HostFeatures::LSE2, 1 << 25),              // HWCAP_USCAT
        (HostFeatures::FLAGM, 1 << 27),             // HWCAP_FLAGM
        (HostFeatures::PAUTH, 1 << 30),             // HWCAP_PACA
        (HostFeatures::PACG, 1 << 31),              // HWCAP_PACG
    ] {
        features.insert(feature, hwcap & mask == mask);
    }
    for (feature, bit) in [
        (HostFeatures::SVE2, 1),   // HWCAP2_SVE2
        (HostFeatures::FLAGM2, 7), // HWCAP2_FLAGM2
        (HostFeatures::I8MM, 13),  // HWCAP2_I8MM
        (HostFeatures::BF16, 14),  // HWCAP2_BF16
        (HostFeatures::BTI, 17),   // HWCAP2_BTI
        (HostFeatures::AFP, 20),   // HWCAP2_AFP
        (HostFeatures::SME, 23),   // HWCAP2_SME
    ] {
        features.insert(feature, hwcap2 & (1 << bit) != 0);
    }
    features
}

#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
fn detect() -> HostFeatures {
    // SAFETY: getauxval reads the process's immutable OS-provided auxiliary vector.
    unsafe {
        from_auxv(
            libc::getauxval(libc::AT_HWCAP),
            libc::getauxval(libc::AT_HWCAP2),
        )
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
fn detect() -> HostFeatures {
    // Exact, case-sensitive names exported by Apple XNU arm_features.inc and
    // bsd/kern/kern_mib.c (ARM_FEATURE_FLAG). Missing keys remain unavailable.
    // https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/arm/arm_features.inc
    let mut features = HostFeatures::EMPTY;
    for (feature, key) in [
        (HostFeatures::FLAGM, c"hw.optional.arm.FEAT_FlagM"),
        (HostFeatures::FLAGM2, c"hw.optional.arm.FEAT_FlagM2"),
        (HostFeatures::AFP, c"hw.optional.arm.FEAT_AFP"),
        (HostFeatures::LSE, c"hw.optional.arm.FEAT_LSE"),
        (HostFeatures::LSE2, c"hw.optional.arm.FEAT_LSE2"),
        (HostFeatures::RCPC, c"hw.optional.arm.FEAT_LRCPC"),
        (HostFeatures::FP16, c"hw.optional.arm.FEAT_FP16"),
        (HostFeatures::BF16, c"hw.optional.arm.FEAT_BF16"),
        (HostFeatures::DOTPROD, c"hw.optional.arm.FEAT_DotProd"),
        (HostFeatures::I8MM, c"hw.optional.arm.FEAT_I8MM"),
        (HostFeatures::SME, c"hw.optional.arm.FEAT_SME"),
        (HostFeatures::PAUTH, c"hw.optional.arm.FEAT_PAuth"),
        (HostFeatures::BTI, c"hw.optional.arm.FEAT_BTI"),
    ] {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&value);
        // SAFETY: NUL-terminated static key and an appropriately sized int buffer.
        let result = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                (&mut value as *mut libc::c_int).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        features.insert(
            feature,
            result == 0 && len == std::mem::size_of_val(&value) && value != 0,
        );
    }
    // XNU does not export SVE/SVE2 or a separate generic PACG capability.
    features
}

#[cfg(not(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android", target_os = "macos")
)))]
fn detect() -> HostFeatures {
    HostFeatures::EMPTY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxv_mapping_and_fp16_pair() {
        assert_eq!(from_auxv(0, 0), HostFeatures::EMPTY);
        assert!(!from_auxv(1 << 9, 0).contains(HostFeatures::FP16));
        assert!(!from_auxv(1 << 10, 0).contains(HostFeatures::FP16));
        assert_eq!(from_auxv(u64::MAX, u64::MAX).bits(), 0xffff);
        for (feature, word, bit) in [
            (HostFeatures::FLAGM, 0, 27),
            (HostFeatures::LSE2, 0, 25),
            (HostFeatures::FLAGM2, 1, 7),
            (HostFeatures::AFP, 1, 20),
            (HostFeatures::SVE2, 1, 1),
            (HostFeatures::SME, 1, 23),
        ] {
            let set = from_auxv(
                if word == 0 { 1 << bit } else { 0 },
                if word == 1 { 1 << bit } else { 0 },
            );
            assert_eq!(set, feature);
        }
    }

    #[test]
    fn override_can_only_remove_features() {
        let all = from_auxv(u64::MAX, u64::MAX);
        let limited = all.disable("-AFP,-flagm,-sve,-uscat,-paca, +afp, fp16, -unknown");
        for feature in [
            HostFeatures::AFP,
            HostFeatures::FLAGM,
            HostFeatures::FLAGM2,
            HostFeatures::SVE,
            HostFeatures::SVE2,
            HostFeatures::LSE2,
            HostFeatures::PAUTH,
        ] {
            assert!(!limited.contains(feature));
        }
        assert!(limited.contains(HostFeatures::FP16));
        assert_eq!(
            HostFeatures::EMPTY.disable("+afp,flagm,all"),
            HostFeatures::EMPTY
        );
        assert_eq!(all.disable(""), all);
        assert!(HostFeatures::detected().contains(HostFeatures::get()));
    }

    #[test]
    fn stable_display() {
        assert_eq!(HostFeatures::EMPTY.to_string(), "none");
        assert_eq!(from_auxv(1 << 27, 1 << 20).to_string(), "flagm,afp");
    }

    #[cfg(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android", target_os = "macos")
    ))]
    #[test]
    fn agrees_with_rust_runtime_detection() {
        let features = HostFeatures::detected();
        macro_rules! compare {
            ($feature:ident, $name:tt) => {
                assert_eq!(
                    features.contains(HostFeatures::$feature),
                    std::arch::is_aarch64_feature_detected!($name),
                    $name
                );
            };
        }
        compare!(FLAGM, "flagm");
        compare!(LSE, "lse");
        compare!(LSE2, "lse2");
        compare!(RCPC, "rcpc");
        compare!(FP16, "fp16");
        compare!(BF16, "bf16");
        compare!(DOTPROD, "dotprod");
        compare!(I8MM, "i8mm");
        compare!(SVE, "sve");
        compare!(SVE2, "sve2");
        compare!(PAUTH, "paca");
        compare!(BTI, "bti");
        #[cfg(not(target_os = "macos"))]
        compare!(PACG, "pacg");
        // AFP has no Rust detection name; FlagM2 and SME detection macros are
        // still unstable. Their kernel bit mappings are tested independently.
        println!(
            "host_features detected={features} enabled={}",
            HostFeatures::get()
        );
    }
}
