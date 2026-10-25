# iOS player

ID prefix: IOS

This spec covers the iOS player. iOS 26 and later with TXM set the design baseline. There an app cannot create executable memory, so the JIT needs a debugger such as StikDebug. Rules shared with the other hosts are in [host.md](host.md), and the a64 engine rules in [a64.md](a64.md).

### IOS-JIT-1 Debugger-prepared code pool

**Rule**: A debugger still attached 500 ms after CS_DEBUGGED MUST prepare the pool as one read-execute region through StikDebug's universal calls, then detach. A debugger already gone MUST leave the pool self-mapped before iOS 26 and Tier 0 on iOS 26 and later. The pool MUST retry later if no debugger was found, but MUST stop breakpoint retries once a debugger detaches.
**Applies to**: iOS
**Verification**: Test — gdb-remote transcripts cover allocation, page writes and detach; preparing a real region also needs device review.

iOS 26 with TXM is the baseline: there the app cannot create executable memory itself, so the universal script prepares every page before detaching. A vm_remap alias of that region is the write view. The debugger marks each 16 KiB page, so the iOS pool defaults to 128 MiB. A debugger without that script only attaches and detaches, which leaves CS_DEBUGGED set; a breakpoint then would kill the process. Before iOS 26 that flag is enough, as StikJIT's integration guide says for devices without TXM. The app then maps its pages read-write and adds a read-execute alias. On iOS 26 TXM cannot be ruled out from the app, so a scriptless grant stays Tier 0.

The external StikDebug action in settings opens `stikdebug://enable-jit` with the running bundle ID, which SideStore suffixes, and the process ID. It always names `universal.js`, which also works without TXM. Inside LiveContainer, StikDebug needs LiveContainer's own bundle ID. An attached debugger slows the whole startup several times over, so preparing first keeps that window short. `tools/ios/jit_launch.py` serves the same calls from a Mac with StikDebug's gdb-remote packets, writing pages only on iOS 26 and later, where TXM needs them.

### IOS-JIT-2 Embedded debugger extension

**Rule**: When debugging is permitted, the launcher MUST launch a separate debugger extension for the player's PID using local RPPairing credentials and LocalDevVPN. The helper MUST mount the appropriate DDI, serve IOS-JIT-1 and detach, including on protocol failure. The launcher MUST report progress and failures, with readiness established only by the player's prepared pool.
**Applies to**: iOS
**Verification**: Test — debugger session transcripts; device pairing, mounting and extension activation also require review on a sideloaded device.
**Basis**: IOS-JIT-1

`apps/adv-jit-helper` is a Rust static library linked into `HitodeJIT.appex`, registered at `com.apple.ar.viewer` and launched through Foundation's private NSExtension API. The player sends credentials from Documents/pairingFile.plist over a loopback connection authenticated with a fresh random token delivered in the extension request. Settings imports that file through the document picker and rejects lockdown pairing records, which cannot open the tunnel. Enable Developer Mode and LocalDevVPN before activation; connections to it time out after 10 seconds. DDI files are cached in the extension's Application Support, downloaded from one pinned DeveloperDiskImage revision with jsDelivr as a fallback. Settings shows whether they are present and offers the download separately, because LocalDevVPN takes the only VPN slot from the user's usual network. The helper uses a personalized DDI before iOS 26.4 and a cryptex DDI from iOS 26.4. Before iOS 26 it attaches and detaches; from iOS 26 it serves the universal protocol and writes every code page. The system developer services perform pairing verification and image personalization. LiveContainer uses the external StikDebug action because it does not host this embedded extension.

A game started before the pool is ready stays on Tier 0 for its whole run. So by default, a launcher with debugging permission and an imported pairing file requests JIT when it opens. It asks again on returning to the foreground after a failure. For supported signatures, the library banner strongly recommends JIT until it is on. The launch screen waits for a request in progress and otherwise recommends JIT once per session.

### IOS-JIT-3 Signing capability

**Rule**: Before starting a JIT request or helper operation, the launcher MUST check the running host's signed `get-task-allow` entitlement. Unsupported signatures MUST receive an explicit development-signing instruction in settings, the library banner and game-launch advice. Unsupported installations MUST NOT automatically request JIT or start DDI checks or downloads.
**Applies to**: iOS
**Verification**: Review — signed entitlement queries require a sideloaded device, including the host process inside LiveContainer.

The check uses Security's `SecTaskCopyValueForEntitlement` for the current process rather than inferring capability from certificate names or embedded profiles. Pairing files remain ordinary app data; possessing one does not grant debugging permission. Settings lists no Developer Mode step: iOS launches development-signed apps only with Developer Mode on, so a host that passes this check already has it.

### IOS-MEM-1 Compact window

**Rule**: iOS, and other hosts with `ADV_A64_COMPACT_WINDOW=1`, MUST give the guest only `[0, 2 GiB)` of a 4 GiB-aligned window and map its `Memory::ptr` alias at `x27 + 2 GiB`. The lower guard MAY be an unmapped range the kernel will not allocate. A window search MUST try aligned bases upward from where the kernel places one, never reserving more than one window at a time.
**Applies to**: AArch64
**Verification**: Test

2 GiB is the user space of 32-bit Windows. Without extended virtual addressing an iOS app has about 7 GiB of address space, so a window and a separate alias do not fit. The lower guard is then the end of the dyld shared region.

### IOS-MEM-2 Shared compact window

**Rule**: Memories made on one thread MUST share one compact window that holds one memory's views at a time. A memory MUST map its views back, restoring the window view's protection, before `range` accesses, mapping changes, execution and its drop. Pointers into the window MUST NOT be kept across another process's scheduling turn.
**Applies to**: AArch64
**Verification**: Test

The family runs one process at a time. Every window starts at guest address 0, so `x27`, the alias and translated code stay valid across turns. A launcher that starts a game and waits switches only during their startup requests; mapping back takes about 40 µs.

### IOS-MEM-3 Window reserved at launch

**Rule**: The iOS app MUST reserve the compact window before anything else at launch and keep it until exit. A family MUST take it while no other family holds it, and its last memory MUST hand the range back still reserved.
**Applies to**: iOS
**Verification**: Test

The app's address space ends a few GiB past the dyld shared region. WebKit reserves its 2 GiB JavaScript heap at the start of that range once a webview loads, after which no 4 GiB-aligned window fits. Reserved first, the window leaves WebKit to run without that heap.

### IOS-APP-1 Game folder access

**Rule**: Game imports MUST read folders selected through Files at their original locations by default. External directory permissions MUST persist as bookmarks and be restored before library access. Coordinated reads MUST make cloud files locally readable before discovery and game launch.
**Applies to**: iOS
**Verification**: Review — the Files picker and iCloud downloads run only on a device.

The launcher holds selected directories' security scopes while it runs. Game folders remain at their selected locations; saves and registry use the game's independent private data directory. Finder file sharing also places games in Documents, where the launcher reads them in place.

### IOS-APP-2 Native view

**Rule**: The game view MUST present through a Metal layer above a black base and leave scaling to Core Animation under HOST-VIEW-9. It MUST report its pixel size from layout. A display link MUST publish the vsync grid for guest refresh pacing and remain paused without pending frames under HOST-SES-10.
**Applies to**: iOS
**Verification**: Review — UIKit layout and the display link run only inside an application.

### IOS-APP-3 Touch routing

**Rule**: The game view and the launcher's webview MUST share one container whose hit test asks the overlay Router for each new touch. UIKit then keeps that target for the touch's lifetime. The overlay MUST own all touches while no game runs, and the webview MUST be hidden while a game runs and the overlay draws nothing.
**Applies to**: iOS
**Verification**: Review — UIKit hit testing runs on a device; the overlay Router has unit tests.

### IOS-APP-4 Game sessions

**Rule**: The launcher MUST create and drive at most one session on the main thread, pausing on inactivity, audio interruption or overlay pause under AND-APP-15. During play, the app MUST disable idle sleep, hold landscape and hide the status bar. System edge gestures MUST require a confirming swipe, with home-indicator auto-hiding disabled so touches do not reveal it.
**Applies to**: iOS
**Verification**: Review — sessions need UIKit's main run loop, which only an application provides.

### IOS-APP-5 Audio session

**Rule**: The app MUST use the playback audio category, which the silent switch does not mute. It MUST activate the session at launch, on becoming active and when an interruption ends. Audio focus MUST be held exactly while that activation succeeds.
**Applies to**: iOS
**Verification**: Review — audio sessions and interruptions exist only on a device.

Activating on becoming active also recovers from interruptions the system never reports ending, such as another app's playback.

Withdrawn: none
