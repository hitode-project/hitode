package org.hitode.launcher

import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.webkit.WebView
import android.widget.FrameLayout

/** Native game surface below the transparent overlay, with one target per gesture. */
class GameContainer(private val activity: MainActivity, val webView: WebView) :
    FrameLayout(activity), SurfaceHolder.Callback {
    private val surfaceView = SurfaceView(activity)
    private var gameGesture = false

    init {
        surfaceView.holder.addCallback(this)
        addView(surfaceView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(webView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        val frame = holder.surfaceFrame
        activity.nativeSurfaceCreated(holder.surface, frame.width(), frame.height())
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        activity.nativeSurfaceChanged(holder.surface, width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        val frame = holder.surfaceFrame
        activity.nativeSurfaceDestroyed(holder.surface, frame.width(), frame.height())
    }

    // AND-APP-12: region changes and additional fingers cannot split a gesture.
    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        val action = event.actionMasked
        if (action == MotionEvent.ACTION_DOWN) {
            val density = resources.displayMetrics.density
            gameGesture = activity.nativeRouteTouch(event.getX(0) / density, event.getY(0) / density)
        }
        if (!gameGesture) return super.dispatchTouchEvent(event)

        fun forward(index: Int, pointerAction: Int) {
            activity.nativeTouch(
                event.getPointerId(index), pointerAction, event.getX(index), event.getY(index)
            )
        }
        when (action) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN ->
                forward(event.actionIndex, MotionEvent.ACTION_DOWN)
            MotionEvent.ACTION_MOVE ->
                for (index in 0 until event.pointerCount) forward(index, MotionEvent.ACTION_MOVE)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP ->
                forward(event.actionIndex, MotionEvent.ACTION_UP)
            MotionEvent.ACTION_CANCEL ->
                for (index in 0 until event.pointerCount) forward(index, MotionEvent.ACTION_CANCEL)
        }
        if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            gameGesture = false
        }
        return true
    }
}
