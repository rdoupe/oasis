package app.oasis.mobile

import android.view.KeyEvent
import android.view.View
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Drives the terminal's input connection the way keyboards do (typing with
 * suggestions, and voice typing's composing and corrections) and checks what the
 * pane receives: each piece of text once, corrections as backspaces.
 */
@RunWith(AndroidJUnit4::class)
class TerminalInputConnectionTest {
    private class Recorder : TerminalInputSink {
        val events = mutableListOf<String>()
        var armed = false
        var shown = ""

        override fun text(text: String) {
            events += "text:$text"
        }

        override fun key(code: Int) {
            events += when (code) {
                Keys.BACKSPACE -> "bs"
                Keys.ENTER -> "enter"
                Keys.DELETE -> "del"
                else -> "key:$code"
            }
        }

        override fun modifierArmed() = armed

        override fun modifiedChar(text: String) {
            armed = false
            events += "ctrl:$text"
        }

        override fun composing(text: String) {
            shown = text
        }

        /** The line the pane ends up with, applying text and backspaces. */
        fun line(): String {
            val out = StringBuilder()
            for (e in events) when {
                e.startsWith("text:") -> out.append(e.removePrefix("text:"))
                e == "bs" -> if (out.isNotEmpty()) out.setLength(out.offsetByCodePoints(out.length, -1))
                e == "enter" -> out.append('⏎')
            }
            return out.toString()
        }
    }

    private fun run(block: (TerminalInputConnection, Recorder) -> Unit) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.runOnMainSync {
            val sink = Recorder()
            block(TerminalInputConnection(View(instrumentation.targetContext), sink), sink)
        }
    }

    @Test
    fun voice_composing_then_finish_sends_the_text_once() = run { ic, sink ->
        ic.setComposingText("what", 1)
        ic.setComposingText("what is", 1)
        ic.setComposingText("what is the time", 1)
        assertEquals("what is the time", sink.shown)
        assertEquals(emptyList<String>(), sink.events)
        ic.finishComposingText()
        assertEquals(listOf("text:what is the time"), sink.events)
        assertEquals("", sink.shown)
        // A late second finish, as keyboards send on closing, sends nothing more.
        ic.finishComposingText()
        ic.closeConnection()
        assertEquals(listOf("text:what is the time"), sink.events)
    }

    @Test
    fun voice_composing_then_commit_of_the_final_text_sends_it_once() = run { ic, sink ->
        ic.setComposingText("hi there", 1)
        ic.commitText("Hi there.", 1)
        ic.finishComposingText()
        assertEquals(listOf("text:Hi there."), sink.events)
    }

    @Test
    fun a_second_utterance_after_the_first_adds_only_its_own_text() = run { ic, sink ->
        ic.setComposingText("first part", 1)
        ic.finishComposingText()
        ic.setComposingText(" second", 1)
        ic.setComposingText(" second part", 1)
        ic.finishComposingText()
        assertEquals(listOf("text:first part", "text: second part"), sink.events)
    }

    @Test
    fun typing_with_suggestions_sends_each_word_once() = run { ic, sink ->
        ic.setComposingText("h", 1)
        ic.setComposingText("he", 1)
        ic.setComposingText("hello", 1)
        ic.commitText("hello", 1)
        ic.commitText(" ", 1)
        assertEquals(listOf("text:hello", "text: "), sink.events)
    }

    @Test
    fun recomposing_a_sent_word_and_correcting_it_replaces_it_in_the_pane() = run { ic, sink ->
        ic.commitText("hello wold", 1)
        ic.setComposingRegion(6, 10)
        assertEquals("wold", sink.shown)
        ic.setComposingText("world", 1)
        ic.finishComposingText()
        assertEquals("hello world", sink.line())
    }

    @Test
    fun replacing_a_committed_word_in_one_batch_backspaces_then_types() = run { ic, sink ->
        ic.commitText("hello wold", 1)
        ic.beginBatchEdit()
        ic.setSelection(6, 10)
        ic.commitText("world", 1)
        ic.endBatchEdit()
        // Only what differs is retyped: "wo" stays, "ld" goes, "rld" comes.
        assertEquals(listOf("text:hello wold", "bs", "bs", "text:rld"), sink.events)
        assertEquals("hello world", sink.line())
    }

    @Test
    fun a_key_while_composing_sends_the_composing_text_first_and_starts_a_new_line() = run { ic, sink ->
        ic.setComposingText("ls -la", 1)
        ic.reset()
        sink.events += "enter"
        ic.commitText("pwd", 1)
        assertEquals(listOf("text:ls -la", "enter", "text:pwd"), sink.events)
    }

    @Test
    fun backspace_keys_delete_committed_text_then_the_panes_own() = run { ic, sink ->
        ic.commitText("ab", 1)
        ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL))
        ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL))
        ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL))
        ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL))
        assertEquals(listOf("text:ab", "bs", "bs", "bs"), sink.events)
        ic.commitText("c", 1)
        assertEquals("c", sink.line())
    }

    @Test
    fun deleting_past_the_keyboards_text_deletes_the_panes_text() = run { ic, sink ->
        ic.commitText("a", 1)
        ic.deleteSurroundingText(3, 0)
        assertEquals(listOf("text:a", "bs", "bs", "bs"), sink.events)
    }

    @Test
    fun a_committed_newline_is_enter_and_starts_a_new_line() = run { ic, sink ->
        ic.commitText("echo hi\n", 1)
        ic.commitText("x", 1)
        ic.deleteSurroundingText(1, 0)
        assertEquals(listOf("text:echo hi", "enter", "text:x", "bs"), sink.events)
    }

    @Test
    fun an_armed_ctrl_turns_the_next_character_into_a_chord() = run { ic, sink ->
        sink.armed = true
        ic.setComposingText("c", 1)
        assertEquals(listOf("ctrl:c"), sink.events)
        assertEquals("", sink.shown)
        ic.finishComposingText()
        sink.armed = true
        ic.commitText("d", 1)
        ic.commitText("x", 1)
        assertEquals(listOf("ctrl:c", "ctrl:d", "text:x"), sink.events)
    }

    @Test
    fun long_input_is_trimmed_without_resending() = run { ic, sink ->
        val words = (1..80).map { "word$it " }
        for (w in words) ic.commitText(w, 1)
        assertEquals(words.map { "text:$it" }, sink.events)
        ic.deleteSurroundingText(1, 0)
        assertEquals("bs", sink.events.last())
    }

    @Test
    fun emoji_backspace_is_one_key() = run { ic, sink ->
        ic.commitText("hi 😀", 1)
        ic.sendKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL))
        assertEquals(listOf("text:hi 😀", "bs"), sink.events)
        ic.commitText("😁", 1)
        assertEquals("hi 😁", sink.line())
    }
}
