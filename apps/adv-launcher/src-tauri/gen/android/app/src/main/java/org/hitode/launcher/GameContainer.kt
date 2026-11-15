package org.hitode.launcher

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.os.Build
import android.util.SparseIntArray
import android.view.MotionEvent
import android.view.RoundedCorner
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.core.graphics.Insets
import androidx.core.view.WindowInsetsCompat
import kotlin.math.roundToInt

private const val TARGET_OVERLAY = 0
private const val TARGET_GAME = 1
private const val TARGET_EDGE = 2

/** Native game surface below the transparent overlay, with one target per pointer. */
class GameContainer(private val activity: MainActivity, val webView: WebView) :
    FrameLayout(activity), SurfaceHolder.Callback {
    private val surfaceView = SurfaceView(activity)
    private val cursor = Cursor(activity)
    private val pointerTargets = SparseIntArray()
    private var overlayDownTime = 0L
    private var gestureInsets = Insets.NONE
    private var imeVisible = false

    init {
        setBackgroundColor(Color.BLACK)
        surfaceView.holder.addCallback(this)
        addView(surfaceView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(webView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(cursor, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
    }

    override fun onApplyWindowInsets(insets: WindowInsets): WindowInsets {
        val compat = WindowInsetsCompat.toWindowInsetsCompat(insets, this)
        val imeNow = compat.isVisible(WindowInsetsCompat.Type.ime())
        if (imeVisible && !imeNow) {
            post { activity.reapplyGameBars() }
        }
        imeVisible = imeNow
        placeGame(insets, compat)
        return super.onApplyWindowInsets(insets)
    }

    override fun onSizeChanged(width: Int, height: Int, oldWidth: Int, oldHeight: Int) {
        super.onSizeChanged(width, height, oldWidth, oldHeight)
        post {
            rootWindowInsets?.let { insets ->
                placeGame(insets, WindowInsetsCompat.toWindowInsetsCompat(insets, this))
            }
        }
    }

    // AND-APP-16: report what hides the screen and keep the surface clear of cutouts.
    private fun placeGame(insets: WindowInsets, compat: WindowInsetsCompat) {
        if (width == 0 || height == 0) return
        val density = resources.displayMetrics.density
        val cutouts = insets.displayCutout?.boundingRects.orEmpty()
            .flatMap { listOf(it.left, it.top, it.width(), it.height()) }
            .map { it / density }
            .toFloatArray()
        val radius = if (Build.VERSION.SDK_INT >= 31) {
            listOf(
                RoundedCorner.POSITION_TOP_LEFT, RoundedCorner.POSITION_TOP_RIGHT,
                RoundedCorner.POSITION_BOTTOM_RIGHT, RoundedCorner.POSITION_BOTTOM_LEFT,
            ).maxOf { insets.getRoundedCorner(it)?.radius ?: 0 } / density
        } else 0f
        // Guard system gesture strips, any visible navigation bar, and the 20 dp
        // transient-swipe edge of a hidden navigation bar.
        val transientEdge = (20 * density).roundToInt()
        val hiddenNav = Insets.min(
            compat.getInsetsIgnoringVisibility(WindowInsetsCompat.Type.navigationBars()),
            Insets.of(transientEdge, transientEdge, transientEdge, transientEdge),
        )
        val types = WindowInsetsCompat.Type.systemGestures() or WindowInsetsCompat.Type.navigationBars()
        val gestures = Insets.max(compat.getInsets(types), hiddenNav)
        gestureInsets = gestures
        val area = activity.nativeScreen(
            width / density, height / density, cutouts, radius,
            listOf(gestures.top, gestures.right, gestures.bottom, gestures.left)
                .map { it / density }.toFloatArray(),
            density,
        ) ?: return
        // Symmetric margins keep the surface centred to the pixel.
        val left = (area[0] * density).roundToInt()
        val top = (area[1] * density).roundToInt()
        val params = surfaceView.layoutParams as LayoutParams
        if (params.leftMargin != left || params.topMargin != top) {
            params.setMargins(left, top, left, top)
            surfaceView.layoutParams = params
        }
    }

    /** Shows the trackpad cursor at a point of the game surface, in pixels. */
    fun setCursor(shown: Boolean, x: Float, y: Float) {
        cursor.visibility = if (shown) View.VISIBLE else View.GONE
        cursor.translationX = surfaceView.left + x - cursor.tip
        cursor.translationY = surfaceView.top + y - cursor.tip
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

    private fun routePointer(x: Float, y: Float): Int {
        val density = resources.displayMetrics.density
        if (!activity.nativeRouteTouch(x / density, y / density)) return TARGET_OVERLAY
        // Let a system gesture or transient bar swipe start without sending motion to Wine.
        val edge = x < gestureInsets.left || x >= width - gestureInsets.right ||
            y < gestureInsets.top || y >= height - gestureInsets.bottom
        return if (edge) TARGET_EDGE else TARGET_GAME
    }

    // AND-APP-12, HOST-OVL-4: each pointer keeps the target selected at press until lift.
    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        val action = event.actionMasked
        if (action == MotionEvent.ACTION_DOWN) {
            pointerTargets.clear()
        }
        if (action == MotionEvent.ACTION_DOWN || action == MotionEvent.ACTION_POINTER_DOWN) {
            val idx = event.actionIndex
            val target = routePointer(event.getX(idx), event.getY(idx))
            pointerTargets.put(event.getPointerId(idx), target)
        }

        fun targetOf(index: Int): Int =
            pointerTargets.get(event.getPointerId(index), TARGET_OVERLAY)

        fun forwardGame(index: Int, pointerAction: Int) {
            activity.nativeTouch(
                event.getPointerId(index), pointerAction,
                event.getX(index) - surfaceView.left, event.getY(index) - surfaceView.top,
            )
        }

        when (action) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val idx = event.actionIndex
                if (targetOf(idx) == TARGET_GAME) forwardGame(idx, MotionEvent.ACTION_DOWN)
            }
            MotionEvent.ACTION_MOVE -> {
                for (index in 0 until event.pointerCount) {
                    if (targetOf(index) == TARGET_GAME) forwardGame(index, MotionEvent.ACTION_MOVE)
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val idx = event.actionIndex
                if (targetOf(idx) == TARGET_GAME) forwardGame(idx, MotionEvent.ACTION_UP)
            }
            MotionEvent.ACTION_CANCEL -> {
                for (index in 0 until event.pointerCount) {
                    if (targetOf(index) == TARGET_GAME) forwardGame(index, MotionEvent.ACTION_CANCEL)
                }
            }
        }

        var overlayCount = 0
        for (index in 0 until event.pointerCount) {
            if (targetOf(index) == TARGET_OVERLAY) overlayCount++
        }

        val handled = if (overlayCount == 0) {
            true
        } else if (action == MotionEvent.ACTION_DOWN && overlayCount == event.pointerCount) {
            overlayDownTime = event.downTime
            super.dispatchTouchEvent(event)
        } else if (overlayCount == event.pointerCount && overlayDownTime == event.downTime) {
            super.dispatchTouchEvent(event)
        } else {
            dispatchOverlaySubset(event, action, overlayCount)
        }

        if (action == MotionEvent.ACTION_POINTER_UP) {
            pointerTargets.delete(event.getPointerId(event.actionIndex))
        } else if (action == MotionEvent.ACTION_UP || action == MotionEvent.ACTION_CANCEL) {
            pointerTargets.clear()
        }
        return handled
    }

    private fun dispatchOverlaySubset(event: MotionEvent, action: Int, overlayCount: Int): Boolean {
        val actionIdx = event.actionIndex
        val actionIsOverlay =
            pointerTargets.get(event.getPointerId(actionIdx), TARGET_OVERLAY) == TARGET_OVERLAY
        if ((action == MotionEvent.ACTION_POINTER_DOWN || action == MotionEvent.ACTION_POINTER_UP) &&
            !actionIsOverlay
        ) {
            return true
        }

        val properties = Array(overlayCount) { MotionEvent.PointerProperties() }
        val coords = Array(overlayCount) { MotionEvent.PointerCoords() }
        var mappedActionIdx = 0
        var out = 0
        for (index in 0 until event.pointerCount) {
            if (pointerTargets.get(event.getPointerId(index), TARGET_OVERLAY) != TARGET_OVERLAY) {
                continue
            }
            if (index == actionIdx) mappedActionIdx = out
            event.getPointerProperties(index, properties[out])
            event.getPointerCoords(index, coords[out])
            out++
        }

        val filteredAction = when (action) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                if (overlayCount == 1) {
                    overlayDownTime = event.eventTime
                    MotionEvent.ACTION_DOWN
                } else {
                    MotionEvent.ACTION_POINTER_DOWN or
                        (mappedActionIdx shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                if (overlayCount == 1) {
                    MotionEvent.ACTION_UP
                } else {
                    MotionEvent.ACTION_POINTER_UP or
                        (mappedActionIdx shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
                }
            }
            MotionEvent.ACTION_MOVE -> MotionEvent.ACTION_MOVE
            MotionEvent.ACTION_CANCEL -> MotionEvent.ACTION_CANCEL
            else -> return true
        }

        val filtered = MotionEvent.obtain(
            overlayDownTime,
            event.eventTime,
            filteredAction,
            overlayCount,
            properties,
            coords,
            event.metaState,
            event.buttonState,
            event.xPrecision,
            event.yPrecision,
            event.deviceId,
            event.edgeFlags,
            event.source,
            event.flags,
        )
        return try {
            super.dispatchTouchEvent(filtered)
        } finally {
            filtered.recycle()
        }
    }
}

/** The trackpad cursor: a white arrow with a dark outline. */
private class Cursor(context: Context) : View(context) {
    private val scale = context.resources.displayMetrics.density
    /** The arrow's tip, inset on both axes so the outline is not clipped. */
    val tip = scale
    private val arrow = Path().apply {
        moveTo(0f, 0f)
        lineTo(0f, 17f)
        lineTo(4.5f, 12.5f)
        lineTo(7.5f, 19f)
        lineTo(10f, 18f)
        lineTo(7f, 11.5f)
        lineTo(13f, 11.5f)
        close()
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
    private val outline = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.BLACK
        style = Paint.Style.STROKE
        strokeWidth = 1.5f
        strokeJoin = Paint.Join.ROUND
    }

    init {
        visibility = GONE
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val size = (22 * scale).roundToInt()
        setMeasuredDimension(size, size)
    }

    override fun onDraw(canvas: Canvas) {
        canvas.translate(tip, tip)
        canvas.scale(scale, scale)
        canvas.drawPath(arrow, fill)
        canvas.drawPath(arrow, outline)
    }
}
