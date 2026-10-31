package app.oasis.mobile

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The terminal while the keyboard opens: the frame's last row stays on the view's
 * bottom until the server answers, and the size reaches the server once, after the
 * view stops changing, not on every frame of the keyboard's animation.
 */
@RunWith(AndroidJUnit4::class)
class FollowBottomTest {
    @Test
    fun frameThatFitsIsDrawnFromTheTop() {
        assertEquals(0f, frameTop(frameRows = 40, cellH = 20f, height = 800), 0f)
        assertEquals(0f, frameTop(frameRows = 30, cellH = 20f, height = 815), 0f)
    }

    @Test
    fun tallerFrameKeepsItsLastRowOnTheBottom() {
        // 44 rows from before the keyboard, in a view now 28 rows tall.
        val top = frameTop(frameRows = 44, cellH = 42f, height = 28 * 42 + 10)
        assertEquals(-(16 * 42f) + 10f, top, 0f)
        assertEquals(28 * 42f + 10f, top + 44 * 42f, 0f)
    }

    /** A clock the test moves by hand, standing in for the view's postDelayed. */
    private class Clock {
        var now = 0L
        val pending = mutableListOf<Pair<Long, Runnable>>()

        fun schedule(r: Runnable, ms: Long) {
            pending += (now + ms) to r
        }

        fun cancel(r: Runnable) {
            pending.removeAll { it.second === r }
        }

        fun advance(ms: Long) {
            now += ms
            val due = pending.filter { it.first <= now }
            pending.removeAll(due)
            due.forEach { it.second.run() }
        }
    }

    @Test
    fun keyboardAnimationSendsOneResizeAtTheEnd() {
        val clock = Clock()
        var sent = 0
        val settle = Settle(150, clock::schedule, clock::cancel) { sent++ }
        // An IME animation: a new size every frame for 300 ms.
        repeat(18) {
            settle.poke()
            clock.advance(16)
        }
        assertEquals(0, sent)
        clock.advance(149)
        assertEquals(1, sent)
        clock.advance(1000)
        assertEquals(1, sent)
    }

    @Test
    fun nowSendsAtOnceAndDropsTheWait() {
        val clock = Clock()
        var sent = 0
        val settle = Settle(150, clock::schedule, clock::cancel) { sent++ }
        settle.poke()
        settle.now()
        assertEquals(1, sent)
        clock.advance(1000)
        assertEquals(1, sent)
    }
}
