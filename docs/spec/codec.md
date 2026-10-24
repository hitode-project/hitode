# Static codec kernels

ID prefix: CODEC

This spec covers static Vorbis and Theora bindings on the Wine stack.
Device decoding follows [device.md](device.md).

### CODEC-MAP-1 Image binding

**Rule**: Non-distribution images MUST be scanned once per mapping, using executable ranges and the reviewed codec signatures. Distribution images and guest mode MUST NOT be scanned. Unmapping MUST retire bindings and their decoder state.
**Verification**: Test

### CODEC-CALL-1 Vorbis transforms

**Rule**: Native Vorbis transforms MUST retain the built-in stack's admission, ABI, table, buffer, and floating-environment checks. Declined calls MUST preserve guest memory and resume the original function through its trampoline. Verify mode MUST publish guest results and preserve guest floating state.
**Verification**: Test

### CODEC-CALL-2 Theora packets

**Rule**: Theora bindings MUST retain complete library-family admission and preserve pixels, state, guest allocation lifetime, return values, and callbacks. Calls without admitted decoder state MUST resume guest code before changing guest memory. Verify mode MUST compare native results while publishing guest results.
**Verification**: Test
