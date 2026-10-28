package app.oasis.mobile

/** The Rust bridge over the oasis client core (android/core). Main thread only. */
object Native {
    init {
        System.loadLibrary("oasis_android")
    }

    @JvmStatic external fun start(home: String, bin: String, machines: String, cols: Int, rows: Int, cellW: Int, cellH: Int): Long
    @JvmStatic external fun lastError(): String
    @JvmStatic external fun stop(handle: Long)
    @JvmStatic external fun wakeFd(handle: Long): Int
    @JvmStatic external fun drain(handle: Long): String
    @JvmStatic external fun frame(handle: Long): ByteArray
    @JvmStatic external fun openPane(handle: Long, endpoint: String, pane: String)
    @JvmStatic external fun openSpace(handle: Long, endpoint: String, space: String)
    @JvmStatic external fun retry(handle: Long, endpoint: String)
    @JvmStatic external fun request(handle: Long, endpoint: String, method: String, params: String): String?
    @JvmStatic external fun resize(handle: Long, cols: Int, rows: Int, cellW: Int, cellH: Int)
    @JvmStatic external fun setFocus(handle: Long, focused: Boolean)
    @JvmStatic external fun text(handle: Long, text: String)
    @JvmStatic external fun paste(handle: Long, text: String)
    @JvmStatic external fun key(handle: Long, code: Int, modifiers: Int)
    @JvmStatic external fun scroll(handle: Long, lines: Int, col: Int, row: Int)
    @JvmStatic external fun tap(handle: Long, col: Int, row: Int)
    @JvmStatic external fun wordAt(handle: Long, col: Int, row: Int): IntArray
    @JvmStatic external fun selectionText(handle: Long, col0: Int, row0: Int, col1: Int, row1: Int, left: Int, right: Int): String
}

/** Key codes the bridge understands (session::key_code). */
object Keys {
    const val ENTER = 0
    const val TAB = 1
    const val BACKSPACE = 2
    const val ESC = 3
    const val LEFT = 4
    const val RIGHT = 5
    const val UP = 6
    const val DOWN = 7
    const val HOME = 8
    const val END = 9
    const val PAGE_UP = 10
    const val PAGE_DOWN = 11
    const val DELETE = 12
    const val INSERT = 13
    const val BACK_TAB = 14
    fun f(n: Int) = 0x100 + n
    fun char(codepoint: Int) = 0x10000 + codepoint

    const val SHIFT = 1
    const val CTRL = 2
    const val ALT = 4
}
