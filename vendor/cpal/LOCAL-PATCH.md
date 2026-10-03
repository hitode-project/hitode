# WASAPI output clock patch

This is CPAL 0.16.0, Apache-2.0, copied from the crates.io release whose source
revision is `36b9fed079e3d3066acaa58b1ae935caf5c2bfb1` in
[RustAudio/cpal](https://github.com/RustAudio/cpal/tree/36b9fed079e3d3066acaa58b1ae935caf5c2bfb1).
The crate version and upstream license are unchanged. Both workspace manifests
patch that single dependency; the user's registry source is unchanged.

Local differences:

* `OutputCallbackInfo` exposes an optional `OutputDeviceClock`. Other hosts use
  the existing `new(timestamp)` constructor and retain `None`/timestamp fallback.
* WASAPI obtains the clock frequency once, outside the data callback. Its native
  position is converted with `position * sample_rate / clock_frequency` using
  checked 128-bit arithmetic. Each new CPAL stream has a separate clock epoch.
* Immediately before callback submission, WASAPI queries `GetCurrentPadding`
  and the correlated `IAudioClock::GetPosition`/QPC pair. Padding means queued
  unread frames, and excludes the buffer currently being filled. Playback delay
  uses padding; upstream 0.16 incorrectly uses free/writable frames instead.
* Device position, frequency, consumed frames and padding are exposed together.
  The OS observations are sequential, not claimed to be an atomic OS snapshot;
  consumer diagnostics should tolerate timestamp/query rounding.

The clock field/conversion is allocation free. Platform setup and ownership stay
in the same CPAL backend. This does not add a second audio backend or resampler.

Definitions follow Microsoft documentation for
[GetCurrentPadding](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getcurrentpadding)
and [GetPosition](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclock-getposition).
The conversion and legacy constructor have deterministic unit tests.
