package app.oasis.mobile

import android.annotation.SuppressLint
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import android.net.Uri
import android.text.InputType
import android.util.TypedValue
import android.view.ActionMode
import android.view.GestureDetector
import android.view.HapticFeedbackConstants
import android.view.KeyEvent
import android.view.Menu
import android.view.MenuItem
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.Toast
import androidx.core.content.res.ResourcesCompat
import java.nio.ByteBuffer
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor

/**
 * Draws the server's surface for the open machine and turns touch and keyboard
 * input into pane input: a tap shows the keyboard, a drag scrolls, a pinch sizes
 * the font, a sideways fling switches agent, and a long press selects for copy.
 */
@SuppressLint("ViewConstructor")
class TerminalView(context: Context, private val core: Core) : View(context) {
    /** Called with +1 or -1 when the user flings sideways to the next or previous agent. */
    var onSwipe: (Int) -> Unit = {}

    private class Run(val col: Int, val cells: Int, val fg: Int, val bg: Int, val flags: Int, val text: String)

    private var frameCols = 0
    private var frameRows = 0
    private var cursorCol = -1
    private var cursorRow = -1
    private var cursorShape = 0
    private var rowRuns: Array<List<Run>> = emptyArray()

    private val regular = ResourcesCompat.getFont(context, R.font.jetbrains_mono_regular) ?: Typeface.MONOSPACE
    private val bold = ResourcesCompat.getFont(context, R.font.jetbrains_mono_bold) ?: Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG).apply { typeface = regular }
    private val fill = Paint()

    var cellW = 1f
        private set
    var cellH = 1f
        private set
    private var baseline = 0f
    var cols = 0
        private set
    var rows = 0
        private set

    private var composing = ""
    private var selection: Selection? = null
    private var selecting = false
    private var actionMode: ActionMode? = null
    private var scrollAccum = 0f
    private var scaling = false

    private data class Selection(val anchor: Pair<Int, Int>, val head: Pair<Int, Int>, val left: Int, val right: Int, val word: IntArray)

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        setFontSp(core.prefs.fontSp)
    }

    fun setFontSp(sp: Float) {
        paint.textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, sp, resources.displayMetrics)
        cellW = paint.measureText("M")
        val metrics = paint.fontMetrics
        cellH = ceil(metrics.descent - metrics.ascent)
        baseline = -metrics.ascent
        fitGrid()
        invalidate()
    }

    /** The geometry to start the core with before this view has been laid out. */
    fun estimate(widthPx: Int, heightPx: Int) = intArrayOf(
        (widthPx / cellW).toInt().coerceAtLeast(20),
        (heightPx / cellH).toInt().coerceAtLeast(10),
        cellW.toInt(),
        cellH.toInt(),
    )

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) = fitGrid()

    private fun fitGrid() {
        if (width == 0 || height == 0) return
        val c = (width / cellW).toInt().coerceAtLeast(1)
        val r = (height / cellH).toInt().coerceAtLeast(1)
        if (c == cols && r == rows) return
        cols = c
        rows = r
        if (core.handle != 0L) Native.resize(core.handle, c, r, cellW.toInt(), cellH.toInt())
    }

    /** After the core starts, so it lays the surface out for this view. */
    fun syncSize() {
        if (core.handle != 0L && cols > 0) Native.resize(core.handle, cols, rows, cellW.toInt(), cellH.toInt())
    }

    // Frame: see android/core/src/frame.rs for the layout.

    fun setFrame(bytes: ByteArray) {
        val b = ByteBuffer.wrap(bytes)
        frameCols = b.short.toInt() and 0xFFFF
        frameRows = b.short.toInt() and 0xFFFF
        cursorCol = b.short.toInt()
        cursorRow = b.short.toInt()
        cursorShape = b.get().toInt()
        rowRuns = Array(frameRows) {
            val count = b.short.toInt() and 0xFFFF
            List(count) {
                val col = b.short.toInt() and 0xFFFF
                val cells = b.short.toInt() and 0xFFFF
                val fg = b.int
                val bg = b.int
                val flags = b.short.toInt() and 0xFFFF
                val len = b.short.toInt() and 0xFFFF
                val text = String(bytes, b.position(), len, Charsets.UTF_8)
                b.position(b.position() + len)
                Run(col, cells, fg, bg, flags, text)
            }
        }
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        // Compose hosts this view without clipping its children.
        canvas.clipRect(0, 0, width, height)
        canvas.drawColor(BACKGROUND)
        for ((y, runs) in rowRuns.withIndex()) {
            val top = y * cellH
            for (run in runs) {
                val left = run.col * cellW
                if (run.bg != BACKGROUND) {
                    fill.color = run.bg
                    canvas.drawRect(left, top, left + run.cells * cellW, top + cellH, fill)
                }
                if (run.text.isBlank()) continue
                paint.color = run.fg
                paint.typeface = if (run.flags and BOLD != 0) bold else regular
                paint.textSkewX = if (run.flags and ITALIC != 0) -0.2f else 0f
                // One glyph drawn alone: squeeze it into its cells if the font draws it wider.
                val fits = run.text.length == run.cells || paint.measureText(run.text) <= run.cells * cellW + 0.5f
                paint.textScaleX = if (fits) 1f else run.cells * cellW / paint.measureText(run.text)
                canvas.drawText(run.text, left, top + baseline, paint)
                paint.textScaleX = 1f
                if (run.flags and UNDERLINE != 0) {
                    fill.color = run.fg
                    canvas.drawRect(left, top + cellH - 2f, left + run.cells * cellW, top + cellH - 0.5f, fill)
                }
                if (run.flags and STRIKE != 0) {
                    fill.color = run.fg
                    canvas.drawRect(left, top + cellH / 2, left + run.cells * cellW, top + cellH / 2 + 1.5f, fill)
                }
            }
        }
        paint.textSkewX = 0f
        paint.typeface = regular
        drawSelection(canvas)
        drawCursor(canvas)
    }

    private fun drawCursor(canvas: Canvas) {
        if (cursorCol < 0 || cursorRow < 0) return
        val left = cursorCol * cellW
        val top = cursorRow * cellH
        if (composing.isNotEmpty()) {
            // Text the keyboard is still composing, shown where it will land.
            val w = paint.measureText(composing)
            fill.color = SELECTION
            canvas.drawRect(left, top, left + w, top + cellH, fill)
            paint.color = Color.WHITE
            canvas.drawText(composing, left, top + baseline, paint)
            fill.color = ACCENT
            canvas.drawRect(left, top + cellH - 3f, left + w, top + cellH, fill)
            return
        }
        fill.color = CURSOR
        when (cursorShape) {
            3, 4 -> canvas.drawRect(left, top + cellH - 3f, left + cellW, top + cellH, fill)
            5, 6 -> canvas.drawRect(left, top, left + 3f, top + cellH, fill)
            else -> {
                fill.alpha = if (hasFocus()) 140 else 70
                canvas.drawRect(left, top, left + cellW, top + cellH, fill)
                fill.alpha = 255
            }
        }
    }

    // Keyboard.

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // Plain text without auto-capitalize: suggestions, swipe and voice typing still
        // work, and each committed word goes straight to the pane.
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_AUTO_CORRECT
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or
            EditorInfo.IME_ACTION_NONE or EditorInfo.IME_FLAG_NO_ENTER_ACTION
        return TerminalInput()
    }

    private inner class TerminalInput : BaseInputConnection(this@TerminalView, true) {
        override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
            setComposing("")
            super.commitText(text, newCursorPosition)
            send(text.toString())
            trimEditable()
            return true
        }

        override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
            super.setComposingText(text, newCursorPosition)
            setComposing(text.toString())
            return true
        }

        override fun finishComposingText(): Boolean {
            val text = composing
            setComposing("")
            super.finishComposingText()
            if (text.isNotEmpty()) send(text)
            return true
        }

        override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
            repeat(beforeLength.coerceAtMost(64)) { key(Keys.BACKSPACE) }
            repeat(afterLength.coerceAtMost(64)) { key(Keys.DELETE) }
            return super.deleteSurroundingText(beforeLength, afterLength)
        }

        override fun sendKeyEvent(event: KeyEvent): Boolean {
            if (event.action == KeyEvent.ACTION_DOWN) onKeyDown(event.keyCode, event)
            return true
        }

        override fun performEditorAction(actionCode: Int): Boolean {
            key(Keys.ENTER)
            return true
        }

        /** Keeps a little text for the keyboard's context, never an unbounded buffer. */
        private fun trimEditable() {
            val editable = editable ?: return
            if (editable.length > 200) editable.delete(0, editable.length - 100)
        }
    }

    private fun setComposing(text: String) {
        if (text != composing) {
            composing = text
            invalidate()
        }
    }

    /** Typed text, sent as a Ctrl/Alt key when one is armed on the extra-keys row. */
    private fun send(text: String) {
        if (text.isEmpty() || core.handle == 0L) return
        val mods = armedModifiers()
        if (text == "\n") {
            key(Keys.ENTER)
        } else if (mods != 0 && text.codePointCount(0, text.length) == 1) {
            key(Keys.char(text.lowercase().codePointAt(0)), mods)
        } else {
            Native.text(core.handle, text.replace('\n', '\r'))
        }
    }

    fun key(code: Int, modifiers: Int = 0) {
        if (core.handle == 0L) return
        Native.key(core.handle, code, modifiers or armedModifiers())
    }

    private fun armedModifiers(): Int {
        var mods = 0
        if (core.ctrl.value) mods = mods or Keys.CTRL
        if (core.alt.value) mods = mods or Keys.ALT
        core.ctrl.value = false
        core.alt.value = false
        return mods
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        var mods = 0
        if (event.isShiftPressed) mods = mods or Keys.SHIFT
        if (event.isCtrlPressed) mods = mods or Keys.CTRL
        if (event.isAltPressed) mods = mods or Keys.ALT
        val named = when (keyCode) {
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> Keys.ENTER
            KeyEvent.KEYCODE_DEL -> Keys.BACKSPACE
            KeyEvent.KEYCODE_FORWARD_DEL -> Keys.DELETE
            KeyEvent.KEYCODE_TAB -> if (event.isShiftPressed) Keys.BACK_TAB else Keys.TAB
            KeyEvent.KEYCODE_ESCAPE -> Keys.ESC
            KeyEvent.KEYCODE_DPAD_LEFT -> Keys.LEFT
            KeyEvent.KEYCODE_DPAD_RIGHT -> Keys.RIGHT
            KeyEvent.KEYCODE_DPAD_UP -> Keys.UP
            KeyEvent.KEYCODE_DPAD_DOWN -> Keys.DOWN
            KeyEvent.KEYCODE_MOVE_HOME -> Keys.HOME
            KeyEvent.KEYCODE_MOVE_END -> Keys.END
            KeyEvent.KEYCODE_PAGE_UP -> Keys.PAGE_UP
            KeyEvent.KEYCODE_PAGE_DOWN -> Keys.PAGE_DOWN
            KeyEvent.KEYCODE_INSERT -> Keys.INSERT
            in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> Keys.f(keyCode - KeyEvent.KEYCODE_F1 + 1)
            else -> null
        }
        if (named != null) {
            key(named, if (named == Keys.BACK_TAB) mods and Keys.SHIFT.inv() else mods)
            return true
        }
        val unicode = event.getUnicodeChar(event.metaState and (KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK).inv())
        if (unicode <= 0) return super.onKeyDown(keyCode, event)
        if (mods and (Keys.CTRL or Keys.ALT) != 0) {
            key(Keys.char(Character.toLowerCase(unicode)), mods and Keys.SHIFT.inv())
        } else {
            send(String(Character.toChars(unicode)))
        }
        return true
    }

    fun showKeyboard() {
        requestFocus()
        context.getSystemService(InputMethodManager::class.java).showSoftInput(this, 0)
    }

    /** Opens a link the bridge found under a tap; false when nothing can handle it. */
    /** Opens a tapped link in the app that handles it. A link nothing on the phone will
     *  open (Chrome refuses `chrome://` pages from other apps) is copied instead, to
     *  paste into the browser's address bar. */
    private fun openLink(link: String): Boolean {
        val uri = Uri.parse(link)
        val web = uri.scheme?.lowercase() in setOf("http", "https", "mailto")
        val intent = Intent(Intent.ACTION_VIEW, uri)
        if (web) intent.addCategory(Intent.CATEGORY_BROWSABLE)
        if (context !is android.app.Activity) intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
        try {
            if (uri.scheme?.lowercase() != "chrome") {
                context.startActivity(intent)
                return true
            }
        } catch (_: ActivityNotFoundException) {
        } catch (_: SecurityException) {
        }
        context.getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("link", link))
        Toast.makeText(context, "Copied $link: paste it in the browser's address bar", Toast.LENGTH_LONG).show()
        return true
    }

    fun toggleKeyboard() {
        val shown = rootWindowInsets?.isVisible(android.view.WindowInsets.Type.ime()) == true
        val imm = context.getSystemService(InputMethodManager::class.java)
        if (shown) imm.hideSoftInputFromWindow(windowToken, 0) else showKeyboard()
    }

    // Touch.

    private fun cellAt(x: Float, y: Float) = Pair(
        floor(x / cellW).toInt().coerceIn(0, (frameCols - 1).coerceAtLeast(0)),
        floor(y / cellH).toInt().coerceIn(0, (frameRows - 1).coerceAtLeast(0)),
    )

    private val gestures = GestureDetector(
        context,
        object : GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: MotionEvent) = true

            override fun onSingleTapUp(e: MotionEvent): Boolean {
                if (selection != null) {
                    clearSelection()
                    return true
                }
                val (col, row) = cellAt(e.x, e.y)
                if (core.handle == 0L) {
                    showKeyboard()
                    return true
                }
                // A link opens in the phone's browser; the tap is not passed on, so the
                // remote app doesn't also act on it.
                val link = Native.linkAt(core.handle, col, row)
                if (link != null && openLink(link)) return true
                Native.tap(core.handle, col, row)
                showKeyboard()
                return true
            }

            override fun onScroll(e1: MotionEvent?, e2: MotionEvent, dx: Float, dy: Float): Boolean {
                if (scaling || selecting || core.handle == 0L) return false
                scrollAccum += dy / cellH
                val lines = scrollAccum.toInt()
                if (lines != 0) {
                    scrollAccum -= lines
                    val (col, row) = cellAt(e2.x, e2.y)
                    Native.scroll(core.handle, lines, col, row)
                }
                return true
            }

            override fun onFling(e1: MotionEvent?, e2: MotionEvent, vx: Float, vy: Float): Boolean {
                val start = e1 ?: return false
                val dx = e2.x - start.x
                if (scaling || selecting || abs(vx) < 1500 || abs(dx) < width / 4 || abs(dx) < 2 * abs(e2.y - start.y)) return false
                onSwipe(if (dx < 0) 1 else -1)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (scaling || core.handle == 0L || frameCols == 0) return
                val (col, row) = cellAt(e.x, e.y)
                val word = Native.wordAt(core.handle, col, row)
                selection = Selection(Pair(word[0], word[1]), Pair(word[2], word[3]), word[4], word[5], word)
                selecting = true
                actionMode?.finish()
                performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                invalidate()
            }
        },
    )

    private val pinch = ScaleGestureDetector(
        context,
        object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
            private var size = 0f

            override fun onScaleBegin(detector: ScaleGestureDetector): Boolean {
                scaling = true
                size = core.prefs.fontSp
                return true
            }

            override fun onScale(detector: ScaleGestureDetector): Boolean {
                size = (size * detector.scaleFactor).coerceIn(Prefs.MIN_FONT_SP, Prefs.MAX_FONT_SP)
                setFontSp(Math.round(size * 2) / 2f)
                return true
            }

            override fun onScaleEnd(detector: ScaleGestureDetector) {
                core.prefs.fontSp = Math.round(size * 2) / 2f
            }
        },
    )

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean {
        pinch.onTouchEvent(event)
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
            scaling = false
            scrollAccum = 0f
        }
        if (selecting) {
            when (event.actionMasked) {
                MotionEvent.ACTION_MOVE -> extendSelection(event.x, event.y)
                MotionEvent.ACTION_UP -> {
                    selecting = false
                    showSelectionMenu()
                }
                MotionEvent.ACTION_CANCEL -> clearSelection()
            }
            return true
        }
        if (!pinch.isInProgress) gestures.onTouchEvent(event)
        return true
    }

    /** Grows the selection from the long-pressed word toward the finger, inside its pane. */
    private fun extendSelection(x: Float, y: Float) {
        val sel = selection ?: return
        val (col, row) = cellAt(x, y)
        val c = col.coerceIn(sel.left, sel.right)
        val w = sel.word
        val before = row < w[1] || (row == w[1] && c < w[0])
        val anchor = if (before) Pair(w[2], w[3]) else Pair(w[0], w[1])
        val head = if (before) Pair(c, row) else if (row == w[3] && c < w[2]) Pair(w[2], w[3]) else Pair(c, row)
        selection = sel.copy(anchor = anchor, head = head)
        invalidate()
    }

    private fun ordered(sel: Selection): Pair<Pair<Int, Int>, Pair<Int, Int>> {
        val a = sel.anchor
        val h = sel.head
        return if (a.second < h.second || (a.second == h.second && a.first <= h.first)) Pair(a, h) else Pair(h, a)
    }

    private fun drawSelection(canvas: Canvas) {
        val sel = selection ?: return
        val (start, end) = ordered(sel)
        fill.color = SELECTION
        fill.alpha = 150
        for (row in start.second..end.second) {
            val first = if (row == start.second) start.first else sel.left
            val last = if (row == end.second) end.first else sel.right
            canvas.drawRect(first * cellW, row * cellH, (last + 1) * cellW, (row + 1) * cellH, fill)
        }
        fill.alpha = 255
    }

    private fun selectedText(): String {
        val sel = selection ?: return ""
        if (core.handle == 0L) return ""
        val (start, end) = ordered(sel)
        return Native.selectionText(core.handle, start.first, start.second, end.first, end.second, sel.left, sel.right)
    }

    private fun clearSelection() {
        selection = null
        selecting = false
        actionMode?.finish()
        actionMode = null
        invalidate()
    }

    private fun showSelectionMenu() {
        val text = selectedText()
        actionMode = startActionMode(
            object : ActionMode.Callback2() {
                override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
                    if (text.isNotBlank()) menu.add(0, COPY, 0, "Copy")
                    menu.add(0, PASTE, 1, "Paste")
                    return true
                }

                override fun onPrepareActionMode(mode: ActionMode, menu: Menu) = false

                override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
                    val clipboard = context.getSystemService(ClipboardManager::class.java)
                    when (item.itemId) {
                        COPY -> clipboard.setPrimaryClip(ClipData.newPlainText("terminal", text))
                        PASTE -> paste()
                    }
                    mode.finish()
                    return true
                }

                override fun onDestroyActionMode(mode: ActionMode) {
                    actionMode = null
                    selection = null
                    invalidate()
                }

                override fun onGetContentRect(mode: ActionMode, view: View, outRect: Rect) {
                    val sel = selection ?: return
                    val (start, end) = ordered(sel)
                    outRect.set(
                        (sel.left * cellW).toInt(),
                        (start.second * cellH).toInt(),
                        ((sel.right + 1) * cellW).toInt(),
                        ((end.second + 1) * cellH).toInt(),
                    )
                }
            },
            ActionMode.TYPE_FLOATING,
        )
    }

    fun paste() {
        val clip = context.getSystemService(ClipboardManager::class.java).primaryClip ?: return
        if (clip.itemCount == 0 || core.handle == 0L) return
        val text = clip.getItemAt(0).coerceToText(context).toString()
        if (text.isNotEmpty()) Native.paste(core.handle, text)
    }

    companion object {
        const val BOLD = 1
        const val ITALIC = 2
        const val UNDERLINE = 4
        const val STRIKE = 8
        const val COPY = 1
        const val PASTE = 2

        val BACKGROUND = Color.parseColor("#1a1b26")
        val CURSOR = Color.parseColor("#c0caf5")
        val SELECTION = Color.parseColor("#33467c")
        val ACCENT = Color.parseColor("#7aa2f7")
    }
}
