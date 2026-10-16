package org.hitode.launcher

import android.content.pm.ActivityInfo
import android.graphics.Color
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Surface
import android.view.View
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.annotation.Keep
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat

@Keep
class MainActivity : TauriActivity() {
    private var container: GameContainer? = null
    private var gameMode = false
    private var resumed = false
    private var previousOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    private var previousKeepScreenOn = false
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
    }

    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        webView.setBackgroundColor(Color.TRANSPARENT)
        container = GameContainer(this, webView)
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
        if (active) {
            previousOrientation = requestedOrientation
            previousKeepScreenOn = window.attributes.flags and
                WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            if (resumed) requestAudioFocus()
        } else {
            abandonAudioFocus()
            requestedOrientation = previousOrientation
            if (!previousKeepScreenOn) {
                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            }
        }
        applyImmersiveMode()
    }

    private fun applyImmersiveMode() {
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
    }

    override fun onPause() {
        resumed = false
        nativePause()
        if (gameMode) abandonAudioFocus()
        super.onPause()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && gameMode) applyImmersiveMode()
    }

    override fun onDestroy() {
        if (gameMode) abandonAudioFocus()
        super.onDestroy()
    }

    external fun nativeSurfaceCreated(surface: Surface, width: Int, height: Int)
    external fun nativeSurfaceChanged(surface: Surface, width: Int, height: Int)
    external fun nativeSurfaceDestroyed(surface: Surface, width: Int, height: Int)
    /** Called by the native trackpad cursor (AND-APP-14). */
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
    external fun nativeResume()
    external fun nativeAudioFocus(active: Boolean)
}
