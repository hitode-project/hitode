package org.hitode.launcher

import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.hardware.input.InputManager
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import java.io.File
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import android.view.View
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.BackEventCompat
import androidx.activity.enableEdgeToEdge
import androidx.activity.OnBackPressedCallback
import androidx.annotation.Keep
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

@Keep
class MainActivity : TauriActivity() {
    private var container: GameContainer? = null
    private val launcherBack = object : OnBackPressedCallback(false) {
        private var swipeEdge = -1

        override fun handleOnBackStarted(backEvent: BackEventCompat) {
            swipeEdge = backEvent.swipeEdge
        }

        override fun handleOnBackCancelled() {
            swipeEdge = -1
        }

        override fun handleOnBackPressed() {
            val edge = swipeEdge
            swipeEdge = -1
            nativeBack(edge)
        }
    }
    private var gameMode = false
    private var resumed = false
    private var previousOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    private var previousKeepScreenOn = false
    private val mainHandler by lazy { Handler(Looper.getMainLooper()) }
    private val inputManager by lazy { getSystemService(InputManager::class.java) }
    private val trackedGamepads = LinkedHashMap<Int, TrackedGamepad>()
    private var gamepadTicking = false
    private val gamepadTickRunnable = object : Runnable {
        override fun run() {
            if (!gamepadTicking) return
            if (nativeGamepadTick()) {
                mainHandler.postDelayed(this, 16L)
            } else {
                gamepadTicking = false
            }
        }
    }
    private val gamepadListener = object : InputManager.InputDeviceListener {
        override fun onInputDeviceAdded(deviceId: Int) = syncGamepadDevice(deviceId)
        override fun onInputDeviceChanged(deviceId: Int) = syncGamepadDevice(deviceId)
        override fun onInputDeviceRemoved(deviceId: Int) = removeGamepadDevice(deviceId)
    }

    private data class TrackedGamepad(
        val stableId: String,
        val name: String,
        var hatUp: Boolean = false,
        var hatDown: Boolean = false,
        var hatLeft: Boolean = false,
        var hatRight: Boolean = false,
        var trigL: Boolean = false,
        var trigR: Boolean = false,
    )
    private val audioManager by lazy { getSystemService(AudioManager::class.java) }
    private val audioRequest by lazy {
        AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_GAME)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setOnAudioFocusChangeListener(
                { change -> nativeAudioFocus(change == AudioManager.AUDIOFOCUS_GAIN) },
                Handler(Looper.getMainLooper())
            )
            .build()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        inputManager?.registerInputDeviceListener(gamepadListener, mainHandler)
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        webView.setBackgroundColor(Color.TRANSPARENT)
        container = GameContainer(this, webView)
        scanGamepads()
    }

    fun setLauncherBack(enabled: Boolean) {
        if (launcherBack.isEnabled == enabled) return
        launcherBack.isEnabled = enabled
        if (enabled) onBackPressedDispatcher.addCallback(this, launcherBack)
        else launcherBack.remove()
    }

    fun openIssueInBrowser(url: String): Boolean {
        // Resolve against generic HTTPS handlers, but deliver the original URL.
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
            addCategory(Intent.CATEGORY_BROWSABLE)
            selector = Intent(Intent.ACTION_VIEW, Uri.parse("https:"))
                .addCategory(Intent.CATEGORY_BROWSABLE)
        }
        return try {
            startActivity(intent)
            true
        } catch (_: ActivityNotFoundException) {
            false
        }
    }

    // Wry calls setContentView(webView) after onWebViewCreate. Install the
    // wrapper here so it never reparents a webview already in GameContainer.
    override fun setContentView(view: View?) {
        val host = container
        super.setContentView(if (host != null && view === host.webView) host else view)
    }

    fun setGameMode(active: Boolean) {
        if (gameMode == active) return
        gameMode = active
        DriveProvider.gameRunning = active
        if (active) {
            previousOrientation = requestedOrientation
            previousKeepScreenOn = window.attributes.flags and
                WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            if (resumed) requestAudioFocus()
        } else {
            container?.setCursor(false, 0f, 0f)
            abandonAudioFocus()
            requestedOrientation = previousOrientation
            if (!previousKeepScreenOn) {
                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            }
        }
        applyGameBars()
    }

    fun reapplyGameBars() {
        if (gameMode) applyGameBars()
    }

    private fun applyGameBars() {
        val bars = WindowCompat.getInsetsController(window, window.decorView)
        bars.systemBarsBehavior =
            WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        if (gameMode) bars.hide(WindowInsetsCompat.Type.systemBars())
        else bars.show(WindowInsetsCompat.Type.systemBars())
    }

    private fun requestAudioFocus() {
        nativeAudioFocus(
            audioManager.requestAudioFocus(audioRequest) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
        )
    }

    private fun abandonAudioFocus() {
        audioManager.abandonAudioFocusRequest(audioRequest)
        nativeAudioFocus(false)
    }

    override fun onResume() {
        super.onResume()
        resumed = true
        if (gameMode) requestAudioFocus()
        nativeResume()
        scanGamepads()
    }

    override fun onPause() {
        resumed = false
        stopGamepadTick()
        nativePause()
        if (gameMode) abandonAudioFocus()
        super.onPause()
    }

    /** Hidden UI or memory pressure: the display keeps one buffer per pool. */
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        nativeTrimMemory()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && gameMode) applyGameBars()
    }

    override fun onDestroy() {
        stopGamepadTick()
        inputManager?.unregisterInputDeviceListener(gamepadListener)
        if (gameMode) abandonAudioFocus()
        super.onDestroy()
    }

    private fun isRealGamepad(device: InputDevice): Boolean {
        if (device.isVirtual) return false
        val src = device.sources
        val hasGamepad = (src and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD
        if (hasGamepad) return true
        val hasJoystick = (src and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
        if (!hasJoystick) return false
        return device.hasKeys(
            KeyEvent.KEYCODE_BUTTON_A,
            KeyEvent.KEYCODE_BUTTON_B,
            KeyEvent.KEYCODE_BUTTON_X,
            KeyEvent.KEYCODE_BUTTON_Y,
        ).any { it }
    }

    private fun detectGamepadStyle(device: InputDevice): Int {
        val name = device.name.orEmpty().lowercase()
        return when {
            device.vendorId == 0x054c || name.contains("dualsense") || name.contains("dualshock") || name.contains("playstation") -> 1
            device.vendorId == 0x057e || name.contains("nintendo") || name.contains("switch") || name.contains("joy-con") || name.contains("pro controller") -> 2
            else -> 0
        }
    }

    private fun scanGamepads() {
        val ids = inputManager?.inputDeviceIds ?: return
        for (id in ids) syncGamepadDevice(id)
    }

    private fun syncGamepadDevice(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId)
        if (device == null || !isRealGamepad(device)) {
            removeGamepadDevice(deviceId)
            return
        }
        if (trackedGamepads.containsKey(deviceId)) return
        val name = device.name?.takeIf { it.isNotBlank() } ?: "Game Controller"
        val baseId = "android:%04x:%04x:%s".format(device.vendorId, device.productId, name.lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-'))
        val existingIds = trackedGamepads.values.map { it.stableId }.toSet()
        var stableId = baseId
        var idx = 2
        while (existingIds.contains(stableId)) {
            stableId = "$baseId#$idx"
            idx++
        }
        trackedGamepads[deviceId] = TrackedGamepad(stableId, name)
        nativeGamepadConnected(stableId, name, detectGamepadStyle(device))
    }

    private fun removeGamepadDevice(deviceId: Int) {
        val removed = trackedGamepads.remove(deviceId) ?: return
        nativeGamepadDisconnected(removed.stableId)
        if (trackedGamepads.isEmpty()) stopGamepadTick()
    }

    private fun updateGamepadTick(needed: Boolean) {
        if (needed && !gamepadTicking) {
            gamepadTicking = true
            mainHandler.postDelayed(gamepadTickRunnable, 16L)
        } else if (!needed && gamepadTicking) {
            stopGamepadTick()
        }
    }

    private fun stopGamepadTick() {
        gamepadTicking = false
        mainHandler.removeCallbacks(gamepadTickRunnable)
    }

    private fun mapGamepadKeyCode(keyCode: Int): Int = when (keyCode) {
        KeyEvent.KEYCODE_BUTTON_A -> 0
        KeyEvent.KEYCODE_BUTTON_B -> 1
        KeyEvent.KEYCODE_BUTTON_X -> 2
        KeyEvent.KEYCODE_BUTTON_Y -> 3
        KeyEvent.KEYCODE_BUTTON_L1 -> 4
        KeyEvent.KEYCODE_BUTTON_R1 -> 5
        KeyEvent.KEYCODE_BUTTON_L2 -> 6
        KeyEvent.KEYCODE_BUTTON_R2 -> 7
        KeyEvent.KEYCODE_BUTTON_THUMBL -> 8
        KeyEvent.KEYCODE_BUTTON_THUMBR -> 9
        KeyEvent.KEYCODE_DPAD_UP -> 10
        KeyEvent.KEYCODE_DPAD_DOWN -> 11
        KeyEvent.KEYCODE_DPAD_LEFT -> 12
        KeyEvent.KEYCODE_DPAD_RIGHT -> 13
        KeyEvent.KEYCODE_BUTTON_START -> 14
        KeyEvent.KEYCODE_BUTTON_SELECT -> 15
        else -> -1
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val tracked = trackedGamepads[event.deviceId]
        if (tracked != null) {
            val idx = mapGamepadKeyCode(event.keyCode)
            if (idx >= 0) {
                if (event.repeatCount == 0 && (event.action == KeyEvent.ACTION_DOWN || event.action == KeyEvent.ACTION_UP)) {
                    nativeGamepadButton(tracked.stableId, idx, event.action == KeyEvent.ACTION_DOWN)
                }
                return true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        val tracked = trackedGamepads[event.deviceId]
        if (tracked != null && event.action == MotionEvent.ACTION_MOVE) {
            val lx = event.getAxisValue(MotionEvent.AXIS_X)
            val ly = event.getAxisValue(MotionEvent.AXIS_Y)
            val rx = event.getAxisValue(MotionEvent.AXIS_Z)
            val ry = event.getAxisValue(MotionEvent.AXIS_RZ)
            val hatX = event.getAxisValue(MotionEvent.AXIS_HAT_X)
            val hatY = event.getAxisValue(MotionEvent.AXIS_HAT_Y)
            val lTrig = maxOf(event.getAxisValue(MotionEvent.AXIS_LTRIGGER), event.getAxisValue(MotionEvent.AXIS_BRAKE)) > 0.35f
            val rTrig = maxOf(event.getAxisValue(MotionEvent.AXIS_RTRIGGER), event.getAxisValue(MotionEvent.AXIS_GAS)) > 0.35f

            val up = hatY < -0.5f
            val down = hatY > 0.5f
            val left = hatX < -0.5f
            val right = hatX > 0.5f
            if (up != tracked.hatUp) { tracked.hatUp = up; nativeGamepadButton(tracked.stableId, 10, up) }
            if (down != tracked.hatDown) { tracked.hatDown = down; nativeGamepadButton(tracked.stableId, 11, down) }
            if (left != tracked.hatLeft) { tracked.hatLeft = left; nativeGamepadButton(tracked.stableId, 12, left) }
            if (right != tracked.hatRight) { tracked.hatRight = right; nativeGamepadButton(tracked.stableId, 13, right) }
            if (lTrig != tracked.trigL) { tracked.trigL = lTrig; nativeGamepadButton(tracked.stableId, 6, lTrig) }
            if (rTrig != tracked.trigR) { tracked.trigR = rTrig; nativeGamepadButton(tracked.stableId, 7, rTrig) }

            val needL = nativeGamepadStick(tracked.stableId, false, lx, ly)
            val needR = nativeGamepadStick(tracked.stableId, true, rx, ry)
            updateGamepadTick(needL || needR)
            return true
        }
        return super.dispatchGenericMotionEvent(event)
    }

    external fun nativeSurfaceCreated(surface: Surface, width: Int, height: Int)
    external fun nativeSurfaceChanged(surface: Surface, width: Int, height: Int)
    external fun nativeSurfaceDestroyed(surface: Surface, width: Int, height: Int)
    /** Called by the native trackpad cursor (AND-APP-14). */
    /**
     * Opens an external-storage volume folder in the system Files app
     * (DocumentsUI), asked for by name so file-handling apps do not offer
     * themselves. DocumentsUI stacks every ancestor folder for Back, so it
     * opens as its own task beside the launcher (split screen on large screens)
     * rather than on top of it.
     */
    fun revealFolder(path: String): Boolean {
        val documentId = externalDocumentId(File(path)) ?: return false
        val uri = DocumentsContract.buildDocumentUri(
            "com.android.externalstorage.documents",
            documentId
        )
        return revealDirectory(uri)
    }

    private fun externalDocumentId(target: File): String? {
        val normalized = target.toPath().normalize()
        if (Build.VERSION.SDK_INT >= 30) {
            val manager = getSystemService(StorageManager::class.java)
            for (volume in manager?.storageVolumes.orEmpty()) {
                val root = volume.directory?.toPath()?.normalize() ?: continue
                if (!normalized.startsWith(root)) continue
                val id = if (volume.isPrimary) "primary" else volume.uuid ?: continue
                val relative = root.relativize(normalized).toString().replace(File.separatorChar, '/')
                return "$id:$relative"
            }
        }
        val primary = Environment.getExternalStorageDirectory().toPath().normalize()
        if (!normalized.startsWith(primary)) return null
        val relative = primary.relativize(normalized).toString().replace(File.separatorChar, '/')
        return "primary:$relative"
    }

    fun revealImport(relative: String): Boolean {
        val uri = DocumentsContract.buildDocumentUri(DriveProvider.AUTHORITY, "${DriveProvider.GAMES}/$relative")
        return revealDirectory(uri)
    }

    private fun revealDirectory(uri: android.net.Uri): Boolean {
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_LAUNCH_ADJACENT)
        val files = packageManager.queryIntentActivities(intent, 0)
            .firstOrNull { it.activityInfo.packageName.endsWith(".documentsui") }
            ?: return false
        intent.setClassName(files.activityInfo.packageName, files.activityInfo.name)
        return try {
            startActivity(intent)
            true
        } catch (_: ActivityNotFoundException) {
            false
        }
    }

    /** Opens drive C: of the Wine prefix in the system Files app. */
    fun revealDrive(): Boolean {
        val uri = DocumentsContract.buildRootUri(DriveProvider.AUTHORITY, DriveProvider.ROOT)
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, DocumentsContract.Root.MIME_TYPE_ITEM)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_LAUNCH_ADJACENT)
        return try {
            startActivity(intent)
            true
        } catch (_: ActivityNotFoundException) {
            false
        }
    }

    fun setCursor(shown: Boolean, x: Float, y: Float) {
        container?.setCursor(shown, x, y)
    }

    external fun nativeScreen(
        width: Float,
        height: Float,
        cutouts: FloatArray,
        cornerRadius: Float,
        gestures: FloatArray,
        density: Float,
    ): FloatArray?
    external fun nativeRouteTouch(xCss: Float, yCss: Float): Boolean
    external fun nativeTouch(pointer: Int, action: Int, x: Float, y: Float)
    external fun nativePause()
    external fun nativeBack(edge: Int)
    external fun nativeResume()
    external fun nativeGamepadConnected(id: String, name: String, style: Int)
    external fun nativeGamepadDisconnected(id: String)
    external fun nativeGamepadButton(id: String, buttonIdx: Int, down: Boolean)
    external fun nativeGamepadStick(id: String, rightStick: Boolean, x: Float, y: Float): Boolean
    external fun nativeGamepadTick(): Boolean
    external fun nativeAudioFocus(active: Boolean)
    external fun nativeTrimMemory()
}
