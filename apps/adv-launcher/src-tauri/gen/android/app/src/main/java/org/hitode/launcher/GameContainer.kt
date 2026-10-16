package org.hitode.launcher

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.os.Build
import android.view.MotionEvent
import android.view.RoundedCorner
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowInsets
import android.webkit.WebView
import android.widget.FrameLayout
import kotlin.math.roundToInt

/** Native game surface below the transparent overlay, with one target per gesture. */
class GameContainer(private val activity: MainActivity, val webView: WebView) :
    FrameLayout(activity), SurfaceHolder.Callback {
    private val surfaceView = SurfaceView(activity)
    private val cursor = Cursor(activity)
    private var gameGesture = false

    init {
        setBackgroundColor(Color.BLACK)
        surfaceView.holder.addCallback(this)
        addView(surfaceView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(webView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
        addView(cursor, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT))
    }

    override fun onApplyWindowInsets(insets: WindowInsets): WindowInsets {
        placeGame(insets)
        return super.onApplyWindowInsets(insets)
    }

    override fun onSizeChanged(width: Int, height: Int, oldWidth: Int, oldHeight: Int) {
        super.onSizeChanged(width, height, oldWidth, oldHeight)
        post { rootWindowInsets?.let(::placeGame) }
    }

    // AND-APP-16: report what hides the screen and keep the surface clear of cutouts.
    private fun placeGame(insets: WindowInsets) {
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
        val gestures = if (Build.VERSION.SDK_INT >= 30) {
            insets.getInsets(WindowInsets.Type.mandatorySystemGestures())
        } else {
            @Suppress("DEPRECATION") insets.mandatorySystemGestureInsets
        }
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
                event.getPointerId(index), pointerAction,
                event.getX(index) - surfaceView.left, event.getY(index) - surfaceView.top,
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
