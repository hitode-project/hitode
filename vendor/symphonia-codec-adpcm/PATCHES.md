# Local compatibility changes

This is the published `symphonia-codec-adpcm` 0.5.5 source at upstream revision
`6d533f26150953a882a6a111ebd13f0abf7129d5`. The original copyright notices,
package version, source files and MPL-2.0 license are retained. The production
audio stack continues to use Symphonia's decoder registry.

Three integer arithmetic changes match native Windows ACM:

* IMA expansion keeps the shifted step terms separate, retaining the integer
  truncations of the conventional IMA recurrence. Combining them before a
  shift changes decoded samples and accumulates predictor drift.
* Microsoft ADPCM delta adaptation explicitly wraps its 32-bit multiplication.
  This preserves the release behavior and makes debug builds behave identically
  on large residual sequences rather than panicking on an integer overflow.
* Microsoft ADPCM predictor coefficients use an arithmetic right shift by
  eight. Signed division rounds negative fractional predictors toward zero;
  the native decoder rounds them down. The difference affects predictors 3–6.

Windows ACM is a test oracle only and is never linked into the production decoder.
