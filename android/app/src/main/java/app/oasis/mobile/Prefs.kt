package app.oasis.mobile

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

data class SavedMachine(val label: String, val target: String, val session: String = "default")

/** What the app remembers: the machines to connect to and the terminal font size. */
class Prefs(context: Context) {
    private val prefs = context.getSharedPreferences("oasis", Context.MODE_PRIVATE)

    var machines: List<SavedMachine>
        get() {
            val text = prefs.getString("machines", null) ?: return DEFAULT_MACHINES
            val array = JSONArray(text)
            return (0 until array.length()).map {
                val m = array.getJSONObject(it)
                SavedMachine(m.getString("label"), m.getString("target"), m.optString("session", "default"))
            }
        }
        set(value) = prefs.edit().putString("machines", machinesJson(value)).apply()

    /** The terminal font size in sp, so the phone's own font scale applies on top. */
    var fontSp: Float
        get() = prefs.getFloat("font_sp", DEFAULT_FONT_SP)
        set(value) = prefs.edit().putFloat("font_sp", value).apply()

    companion object {
        const val DEFAULT_FONT_SP = 14f
        const val MIN_FONT_SP = 8f
        const val MAX_FONT_SP = 32f

        /** None until added in settings: they name private hosts. */
        val DEFAULT_MACHINES = emptyList<SavedMachine>()

        fun machinesJson(machines: List<SavedMachine>): String = JSONArray(
            machines.map {
                JSONObject().put("label", it.label).put("target", it.target).put("session", it.session)
            },
        ).toString()
    }
}
