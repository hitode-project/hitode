# Metal depth-test placement

This is the crates.io release of naga 30.0.1 from wgpu commit
`40f4a34ebaf56f9a046231f54125ad046239d3f3`, licensed under MIT or Apache-2.0.

The MSL writer emits `[[early_fragment_tests]]` for forced early tests and
`[[depth(greater)]]` or `[[depth(less)]]` for conservative depth exports.
These modes are separate in naga IR. Ordinary depth exports and conservative
unchanged depth keep `[[depth(any)]]`.
Metal requires a forced early-test entry point to omit depth and stencil outputs.

Apply the diff from the root of the upstream wgpu checkout.

```diff
diff --git a/naga/src/back/msl/mod.rs b/naga/src/back/msl/mod.rs
--- a/naga/src/back/msl/mod.rs
+++ b/naga/src/back/msl/mod.rs
@@ -169,6 +169,7 @@

 enum ResolvedBinding {
     BuiltIn(crate::BuiltIn),
+    ConservativeDepth(crate::ConservativeDepth),
     Attribute(u32),
     Color {
         location: u32,
@@ -644,6 +645,14 @@
     fn try_fmt<W: Write>(&self, out: &mut W) -> Result<(), Error> {
         write!(out, " [[")?;
         match *self {
+            Self::ConservativeDepth(conservative) => {
+                let depth = match conservative {
+                    crate::ConservativeDepth::GreaterEqual => "greater",
+                    crate::ConservativeDepth::LessEqual => "less",
+                    crate::ConservativeDepth::Unchanged => "any",
+                };
+                write!(out, "depth({depth})")?;
+            }
             Self::BuiltIn(built_in) => {
                 use crate::BuiltIn as Bi;
                 let name = match built_in {
@@ -843,7 +852,7 @@
         // No CULL_DISTANCE
         | Caps::STORAGE_TEXTURE_16BIT_NORM_FORMATS
         | Caps::MULTIVIEW
-        // No EARLY_DEPTH_TEST
+        | Caps::EARLY_DEPTH_TEST
         | Caps::MULTISAMPLED_SHADING
         | Caps::RAY_QUERY
         | Caps::DUAL_SOURCE_BLENDING
diff --git a/naga/src/back/msl/writer.rs b/naga/src/back/msl/writer.rs
--- a/naga/src/back/msl/writer.rs
+++ b/naga/src/back/msl/writer.rs
@@ -7401,7 +7401,13 @@
                             } => Some(size),
                             _ => None,
                         };
-                        let resolved = options.resolve_local_binding(binding, out_mode)?;
+                        let resolved = match (binding, ep.early_depth_test) {
+                            (
+                                crate::Binding::BuiltIn(crate::BuiltIn::FragDepth),
+                                Some(crate::EarlyDepthTest::Allow { conservative }),
+                            ) => super::ResolvedBinding::ConservativeDepth(conservative),
+                            _ => options.resolve_local_binding(binding, out_mode)?,
+                        };
                         write!(self.out, "{}{} {}", back::INDENT, ty_name, name)?;
                         resolved.try_fmt(&mut self.out)?;
                         if let Some(array_len) = array_len {
@@ -7489,6 +7495,9 @@
             }

             // Write the entry point function's name, and begin its argument list.
+            if ep.early_depth_test == Some(crate::EarlyDepthTest::Force) {
+                write!(self.out, "[[early_fragment_tests]] ")?;
+            }
             if let Some(em_str) = em_str {
                 write!(self.out, "{em_str} ")?;
             }
```

## SPIR-V texture atomics

The SPIR-V writer passes the selected descriptor element pointer to
`OpImageTexelPointer` when an atomic targets a storage texture in a binding
array. Direct storage textures use their global variable pointer.

## SPIR-V sample masks

Vulkan requires an integer array for the SampleMask interface variable. The
SPIR-V writer represents the WGSL scalar as the first element of a one-word
array in both directions.
