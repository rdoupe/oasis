package app.oasis.mobile

import android.text.Editable
import android.text.Selection
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.InputMethodManager

/** Where the keyboard's text goes: the pane, and the composing text shown at the cursor. */
interface TerminalInputSink {
    /** Text for the pane, as typed. */
    fun text(text: String)

    /** A named key (see [Keys]) for the pane, without armed modifiers. */
    fun key(code: Int)

    /** True while Ctrl or Alt is armed on the extra-keys row. */
    fun modifierArmed(): Boolean

    /** One typed character with the armed modifier, which this disarms. */
    fun modifiedChar(text: String)

    /** The text the keyboard is still composing; empty when none. */
    fun composing(text: String)
}

/**
 * The keyboard's view of the pane's input line, kept as an ordinary editable so
 * every keyboard behaves as in a text field: typing, suggestions, swipe and voice
 * typing (which writes and rewrites composing text, and corrects committed words
 * by replacing them).
 *
 * The pane only ever receives the difference between what it has and what the
 * editable now holds outside the composing region: removed characters as
 * backspaces, added ones as text. So a word sent once is never sent again when
 * the keyboard finishes, replaces or re-composes it. Composing text is shown at
 * the cursor and sent when it is finalized, or when a key is pressed first.
 *
 * Every change is reported back to the keyboard (selection and composing region),
 * which keyboards rely on to know their edits landed.
 */
class TerminalInputConnection(private val view: View, private val sink: TerminalInputSink) :
    BaseInputConnection(view, true) {
    /** The committed text the pane has from this connection. */
    private var sent = ""
    private var batch = 0
    private var reported = intArrayOf(0, 0, -1, -1)
    private val imm: InputMethodManager? = view.context.getSystemService(InputMethodManager::class.java)

    override fun beginBatchEdit(): Boolean {
        batch++
        return true
    }

    override fun endBatchEdit(): Boolean {
        if (batch > 0) batch--
        if (batch == 0) sync()
        return batch > 0
    }

    private fun <T> edit(block: () -> T): T {
        beginBatchEdit()
        try {
            return block()
        } finally {
            endBatchEdit()
        }
    }

    override fun commitText(text: CharSequence, newCursorPosition: Int) =
        edit { super.commitText(text, newCursorPosition) }

    override fun setComposingText(text: CharSequence, newCursorPosition: Int) =
        edit { super.setComposingText(text, newCursorPosition) }

    override fun setComposingRegion(start: Int, end: Int) = edit { super.setComposingRegion(start, end) }

    override fun finishComposingText() = edit { super.finishComposingText() }

    override fun setSelection(start: Int, end: Int) = edit { super.setSelection(start, end) }

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean = edit {
        val e = editable
        val start = e?.let { Selection.getSelectionStart(it) }?.coerceAtLeast(0) ?: 0
        val end = e?.let { Selection.getSelectionEnd(it) }?.coerceAtLeast(0) ?: 0
        val after = (e?.length ?: 0) - end
        // Characters the keyboard deletes past what it was given are the pane's own
        // text before (or after) the cursor: delete them there with keys.
        repeat((beforeLength - start).coerceIn(0, 64)) { sink.key(Keys.BACKSPACE) }
        repeat((afterLength - after).coerceIn(0, 64)) { sink.key(Keys.DELETE) }
        super.deleteSurroundingText(beforeLength, afterLength)
    }

    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int) =
        edit { super.deleteSurroundingTextInCodePoints(beforeLength, afterLength) }

    override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (event.action != KeyEvent.ACTION_DOWN) return true
        if (event.keyCode == KeyEvent.KEYCODE_DEL && !event.isCtrlPressed && !event.isAltPressed) {
            backspace()
            return true
        }
        view.onKeyDown(event.keyCode, event)
        return true
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        reset()
        sink.key(Keys.ENTER)
        return true
    }

    /** A backspace from the keyboard: the last committed character, or the pane's own. */
    private fun backspace() {
        val e = editable
        val cursor = e?.let { Selection.getSelectionEnd(it) } ?: -1
        if (e == null || cursor <= 0 || getComposingSpanStart(e) >= 0 || cursor != e.length) {
            finish()
            sink.key(Keys.BACKSPACE)
            return
        }
        edit { e.delete(e.offsetBefore(cursor), cursor) }
    }

    /** Sends what the keyboard is composing now, e.g. before a key that acts on the line. */
    fun finish() {
        val e = editable ?: return
        if (getComposingSpanStart(e) >= 0) edit { removeComposingSpans(e) }
    }

    /**
     * After a key the line changes in ways the editable can't follow (Enter submits
     * it, arrows move in it): send any composing text, then start a fresh line.
     */
    fun reset() {
        finish()
        val e = editable ?: return
        if (e.isEmpty() && sent.isEmpty()) return
        e.clear()
        sent = ""
        report()
    }

    /** Sends the pane the difference between what it has and the editable's committed text. */
    private fun sync() {
        val e = editable ?: return
        armedKey(e)
        val (start, end) = composingSpan(e)
        val committed = if (start >= 0) e.substring(0, start) + e.substring(end) else e.toString()
        val same = commonPrefix(sent, committed)
        repeat(sent.codePointCount(same, sent.length)) { sink.key(Keys.BACKSPACE) }
        sendLines(committed.substring(same))
        sent = committed
        // Enter submitted everything before it: the next line starts after the last newline.
        val newline = committed.lastIndexOf('\n')
        if (newline >= 0 && (start < 0 || newline < start)) {
            e.delete(0, newline + 1)
            sent = sent.substring(newline + 1)
        }
        trim(e)
        val composing = composingSpan(e).let { (s, t) -> if (s >= 0) e.substring(s, t) else "" }
        sink.composing(composing)
        report()
    }

    /** With Ctrl or Alt armed, one new character is that key chord, not text on the line. */
    private fun armedKey(e: Editable) {
        if (!sink.modifierArmed()) return
        val (start, end) = composingSpan(e)
        val composing = if (start >= 0) e.substring(start, end) else ""
        val committed = if (start >= 0) e.substring(0, start) + e.substring(end) else e.toString()
        if (composing.isNotEmpty() && composing.codePointCount(0, composing.length) == 1) {
            e.delete(start, end)
            sink.modifiedChar(composing)
        } else if (composing.isEmpty() && committed.length > sent.length && committed.startsWith(sent)) {
            val added = committed.substring(sent.length)
            if (added.codePointCount(0, added.length) == 1 && added != "\n") {
                e.delete(sent.length, committed.length)
                sink.modifiedChar(added)
            }
        }
    }

    private fun sendLines(text: String) {
        val lines = text.replace("\r\n", "\n").replace('\r', '\n').split('\n')
        lines.forEachIndexed { i, line ->
            if (i > 0) sink.key(Keys.ENTER)
            if (line.isNotEmpty()) sink.text(line)
        }
    }

    /** Keeps a little text for the keyboard's context, never an unbounded buffer. */
    private fun trim(e: Editable) {
        if (e.length <= MAX_CONTEXT) return
        val (start, _) = composingSpan(e)
        var cut = e.length - KEEP_CONTEXT
        if (start in 0 until cut) cut = start
        cut = cut.coerceAtMost(sent.length)
        if (cut > 0 && Character.isLowSurrogate(e[cut])) cut--
        if (cut <= 0) return
        e.delete(0, cut)
        sent = sent.substring(cut)
    }

    private fun composingSpan(e: Editable): Pair<Int, Int> {
        val a = getComposingSpanStart(e)
        val b = getComposingSpanEnd(e)
        return if (a < 0 || b < 0) Pair(-1, -1) else Pair(minOf(a, b), maxOf(a, b))
    }

    /** Tells the keyboard where the cursor and composing region are now, as a text field does. */
    private fun report() {
        val e = editable ?: return
        val (start, end) = composingSpan(e)
        val now = intArrayOf(Selection.getSelectionStart(e), Selection.getSelectionEnd(e), start, end)
        if (now.contentEquals(reported)) return
        reported = now
        imm?.updateSelection(view, now[0], now[1], now[2], now[3])
    }

    companion object {
        const val MAX_CONTEXT = 200
        const val KEEP_CONTEXT = 100

        /** The length both strings share, not splitting a surrogate pair. */
        fun commonPrefix(a: String, b: String): Int {
            var n = 0
            val max = minOf(a.length, b.length)
            while (n < max && a[n] == b[n]) n++
            if (n > 0 && n < max && Character.isHighSurrogate(a[n - 1])) n--
            return n
        }

        private fun Editable.offsetBefore(index: Int): Int =
            if (index >= 2 && Character.isLowSurrogate(this[index - 1]) && Character.isHighSurrogate(this[index - 2])) index - 2 else index - 1
    }
}
