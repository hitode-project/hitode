# Android player

ID prefix: AND

This spec covers the Android player: its package, its lifecycle and input, and the native game view. `adv-shell` implements the view and session, and `apps/adv-launcher` hosts them with a transparent webview above a SurfaceView. Rules shared with the other hosts are in [host.md](host.md).

### AND-APP-1 Package identity

**Rule**: The Android launcher package MUST be `org.hitode.launcher` and MUST declare `MainActivity`. It MUST target API 36, require API 29 or later, and package only `arm64-v8a` native libraries. Game mode MUST hold landscape orientation, immersive system bars and screen wakefulness, restoring launcher mode when the game ends.
**Applies to**: Android
**Verification**: Check — launcher Gradle configuration and merged APK manifest; device review for game mode.

### AND-APP-2 16 KiB alignment

**Rule**: Every ELF LOAD segment in the APK MUST be aligned to at least 16 KiB. Shared libraries MAY be compressed, so the installer extracts them; a stored library's ZIP data MUST start on a 16 KiB boundary.
**Applies to**: Android
**Verification**: Test — `tools/android/check-apk.py` verifies ELF and ZIP alignment, with authored fixtures in `test_check_apk.py`.

### AND-APP-3 Background

**Rule**: Moving to the background MUST pause execution and stop audio, and returning MUST resume them. Input MUST require both the lifecycle and the audio focus handshakes. Inputs from before a pause MUST be invalidated by generation.
**Applies to**: Android
**Verification**: Test

### AND-APP-4 Input queue

**Rule**: The input queue MUST be bounded. A Ctrl release MUST take effect at once and supersede a queued press.
**Applies to**: Android
**Verification**: Test

### AND-APP-8 Thread priority

**Rule**: The player MUST NOT pin threads to cores or use real-time priority. The report MUST record the actual nice value of the execution thread and any failure to set it.
**Applies to**: Android
**Verification**: Review — thread priority depends on the device.

### AND-APP-9 Game import

**Rule**: All-files access MUST use primary-storage real paths, while other folders MUST copy through SAF into private storage before library discovery. Copies MUST report byte progress, publish complete by same-parent rename, and remove temporary directories on failure or cancellation without modifying sources. Shared Rust library, settings and playtime storage MUST live under `app_data_dir`, and `reveal_path` MUST open only shared-storage folders, in DocumentsUI.
**Applies to**: Android
**Verification**: Test — permission screens and SAF providers also require device review.

### AND-APP-10 Native view

**Rule**: Android views MUST acquire host `ANativeWindow` handles, present through Vulkan without readback, and stop rendering when no surface exists. Replacement MUST retain the instance and adapter, discard queued rendering, drain active jobs before releasing the window, and reconfigure for full redraw. Choreographer MUST follow pending content and reported periods, while views MUST request 60 Hz with optional `ANativeWindow_setFrameRate` and default compatibility.
**Applies to**: Android
**Verification**: Test

### AND-APP-11 Touch input

**Rule**: Touch coordinates MUST match the scaled viewport and clamp to canvas edges. Touches MUST reach the guest only as the gestures of AND-APP-14.
**Applies to**: Android
**Verification**: Test

### AND-APP-12 Touch routing

**Rule**: Each Android gesture MUST retain the overlay Router target selected at the first press until ACTION_UP or ACTION_CANCEL. Hit testing MUST divide view pixels by display density, with the overlay owning all input when no game runs. Game gestures MUST forward each pointer's ID, action and view coordinates through JNI, while overlay gestures MUST use normal view dispatch.
**Applies to**: Android
**Verification**: Review — MotionEvent dispatch runs on a device; the overlay Router has unit tests.

### AND-APP-13 Game sessions

**Rule**: The launcher MUST create and drive game sessions on the activity's UI thread, at most one at a time, using the SurfaceView's current window. A session MUST be paused while the activity is paused, audio focus is lost or the overlay holds a pause. Game mode MUST begin when a session starts and end when it finishes.
**Applies to**: Android
**Verification**: Review — sessions need the activity's looper and SurfaceView, which only a device provides.

### AND-APP-14 Shared mobile gestures

**Rule**: A one-finger tap MUST click left and one-finger vertical movement MUST turn the wheel. A finger held still for 650 ms MUST press the left button and drag until it lifts. A two-finger tap MUST click right and a three-finger tap MUST ask the overlay for the keyboard.
**Verification**: Test

Direct mode acts where the finger lands. Trackpad mode moves a cursor drawn over the game with one finger, and taps and drags act at that cursor; two moving fingers turn the wheel there. The timing follows Moonlight's touch handlers. Distances are in points (dp on Android), not pixels. A touch stays a tap or a long press within 10 points. The long press fires from a timer, so the finger need not move to start a drag. One wheel notch takes 16 points and is sent only whole, because games count WHEEL_DELTA units; fingers moving down scroll toward earlier content, as on a touch screen. A click moves the mouse to its point, then holds its button for 100 ms so games that poll the button state see it. A tap within 250 ms and 20 points of the previous one clicks the same point, which steadies double clicks. Extra fingers leave a drag or scroll alone, and cancellation releases a held button. Changing the mode or pausing ends any gesture and releases the overlay's mouse buttons.

### AND-APP-16 Game screen layout

**Rule**: The game view MUST occupy the largest centred area no camera cutout reaches. It MUST report the presented frame, the cutouts, the corner radius and the system gesture edges to the overlay whenever they change. Overlay controls MAY use the letterbox and safe-area margins but MUST NOT overlap a cutout, a rounded corner or a gesture edge.
**Applies to**: Android, iOS
**Verification**: Test

iOS publishes no housing geometry. An iPhone's is read from its safe-area depth. Under 30 points is a status bar, under 55 a notch of at most 210 points, otherwise a Dynamic Island of at most 126 points. Each takes the largest of its generation, so newer, smaller islands stay covered. The screen reports its own corner radius.

### AND-APP-15 Shared mobile lifecycle

**Rule**: Background and suspended states MUST pause guest execution and audio. Foreground execution MUST require audio focus and no overlay pause. Ending an interruption MUST NOT override background, suspension or overlay pauses.
**Verification**: Test

Suspension retains the lifecycle state to restore, including foreground/background changes received while suspended. Android continues using its existing lifecycle and audio-focus callbacks and pause event path, which invalidates input generations under AND-APP-3.

Withdrawn: AND-APP-5, AND-APP-6, AND-APP-7
