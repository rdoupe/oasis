package app.oasis.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.MessageQueue
import android.os.ParcelFileDescriptor
import android.system.Os
import android.util.Log
import androidx.compose.runtime.mutableStateOf
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import kotlin.concurrent.thread

data class Space(val id: String, val name: String, val status: String, val focused: Boolean)

data class Machine(
    val id: String,
    val label: String,
    val status: String,
    val detail: String,
    val spaces: List<Space>,
)

data class Agent(
    val endpoint: String,
    val machine: String,
    val pane: String,
    val space: String,
    val status: String,
    val agent: String?,
    val title: String,
    val detail: String,
    val stale: Boolean,
)

/**
 * The app's connection to its machines: prepares ssh (the bundled binary, a key,
 * known host keys), starts the client core, and drains it whenever its wake fd
 * turns readable, straight from the main looper.
 */
class Core(private val context: Context) {
    val prefs = Prefs(context)
    val machines = mutableStateOf<List<Machine>>(emptyList())
    val agents = mutableStateOf<List<Agent>>(emptyList())
    val notice = mutableStateOf<String?>(null)
    /** Set while ssh is being prepared or the core has not started. */
    val starting = mutableStateOf(true)
    val ctrl = mutableStateOf(false)
    val alt = mutableStateOf(false)

    var handle = 0L
        private set
    var terminal: TerminalView? = null
    private var wake: ParcelFileDescriptor? = null
    private val main = Handler(Looper.getMainLooper())

    private val home = context.filesDir
    private val bin = File(home, "bin")
    private val ssh = File(home, ".ssh")
    val publicKey: String
        get() = File(ssh, "id_ed25519.pub").takeIf { it.exists() }?.readText()?.trim() ?: ""

    /** Prepares ssh off the main thread, then starts the core with this geometry. */
    fun start(geometry: () -> IntArray) {
        starting.value = true
        val saved = prefs.machines
        thread(name = "oasis-ssh-setup") {
            try {
                prepareSsh(saved)
            } catch (e: Exception) {
                Log.w(TAG, "ssh setup", e)
                main.post { notice.value = "ssh setup: ${e.message}" }
            }
            main.post { connect(saved, geometry()) }
        }
    }

    private fun connect(saved: List<SavedMachine>, g: IntArray) {
        handle = Native.start(home.path, bin.path, Prefs.machinesJson(saved), g[0], g[1], g[2], g[3])
        starting.value = false
        if (handle == 0L) {
            notice.value = "could not start: ${Native.lastError()}"
            return
        }
        val fd = ParcelFileDescriptor.fromFd(Native.wakeFd(handle))
        wake = fd
        Looper.getMainLooper().queue.addOnFileDescriptorEventListener(
            fd.fileDescriptor,
            MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT,
        ) { _, _ ->
            drain()
            MessageQueue.OnFileDescriptorEventListener.EVENT_INPUT
        }
        drain()
    }

    fun stop() {
        wake?.let {
            Looper.getMainLooper().queue.removeOnFileDescriptorEventListener(it.fileDescriptor)
            it.close()
        }
        wake = null
        if (handle != 0L) Native.stop(handle)
        handle = 0L
        machines.value = emptyList()
        agents.value = emptyList()
    }

    /** Restarts with the saved machines, e.g. after settings change them. */
    fun restart(geometry: () -> IntArray) {
        stop()
        start(geometry)
    }

    private fun drain() {
        if (handle == 0L) return
        val out = JSONObject(Native.drain(handle))
        if (out.optBoolean("surface")) terminal?.setFrame(Native.frame(handle))
        out.optJSONArray("machines")?.let { machines.value = parseMachines(it) }
        out.optJSONArray("agents")?.let { agents.value = parseAgents(it) }
        out.optJSONArray("notices")?.let { if (it.length() > 0) notice.value = it.getString(it.length() - 1) }
        if (out.has("clipboard")) {
            val clipboard = context.getSystemService(ClipboardManager::class.java)
            clipboard.setPrimaryClip(ClipData.newPlainText("terminal", out.getString("clipboard")))
        }
    }

    fun openAgent(agent: Agent) {
        if (handle == 0L) return
        Native.openPane(handle, agent.endpoint, agent.pane)
        terminal?.setFrame(Native.frame(handle))
    }

    fun openSpace(machine: Machine, space: Space) {
        if (handle == 0L) return
        Native.openSpace(handle, machine.id, space.id)
        terminal?.setFrame(Native.frame(handle))
    }

    /** Retries a machine, recording its host key first in case that was the problem. */
    fun retry(machine: Machine) {
        val saved = prefs.machines.firstOrNull { it.label == machine.label }
        thread(name = "oasis-retry") {
            saved?.let { runCatching { trustHost(it) } }
            main.post { if (handle != 0L) Native.retry(handle, machine.id) }
        }
    }

    // ssh: the APK's libssh.so under the name `ssh` on PATH, a key, and host keys.

    private fun prepareSsh(saved: List<SavedMachine>) {
        val libs = context.applicationInfo.nativeLibraryDir
        bin.mkdirs()
        for ((name, lib) in listOf("ssh" to "libssh.so", "ssh-keygen" to "libssh-keygen.so")) {
            val link = File(bin, name)
            // The library directory moves with every update, so relink each start.
            link.delete()
            Os.symlink("$libs/$lib", link.path)
        }
        ssh.mkdirs()
        File(ssh, "config").writeText(
            """
            |# Written by the Oasis app at each start.
            |Host *
            |    IdentityFile ~/.ssh/id_ed25519
            |    IdentitiesOnly yes
            |    ServerAliveInterval 15
            |""".trimMargin(),
        )
        val key = File(ssh, "id_ed25519")
        if (!key.exists()) {
            run(listOf(File(bin, "ssh-keygen").path, "-q", "-t", "ed25519", "-N", "", "-C", "oasis@${android.os.Build.MODEL.replace(' ', '-')}", "-f", key.path))
        }
        for (machine in saved) runCatching { trustHost(machine) }
    }

    /**
     * Records a machine's host key on first contact. Oasis runs ssh with strict host
     * key checking, and the phone has nowhere to answer ssh's question.
     */
    private fun trustHost(machine: SavedMachine) {
        val host = machine.target.substringAfter('@')
        val known = File(ssh, "known_hosts")
        if (known.exists() && run(listOf(File(bin, "ssh-keygen").path, "-F", host, "-f", known.path)) == 0) return
        run(
            listOf(
                File(bin, "ssh").path, "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new",
                "-o", "ConnectTimeout=8", machine.target, "true",
            ),
        )
    }

    private fun run(command: List<String>): Int {
        val process = ProcessBuilder(command).redirectErrorStream(true).apply {
            environment()["HOME"] = home.path
            environment()["PATH"] = "${bin.path}:/system/bin"
        }.start()
        val output = process.inputStream.bufferedReader().readText()
        val code = process.waitFor()
        if (code != 0) Log.i(TAG, "${command.first().substringAfterLast('/')} exited $code: $output")
        return code
    }

    companion object {
        const val TAG = "oasis"

        fun parseMachines(array: JSONArray) = (0 until array.length()).map {
            val m = array.getJSONObject(it)
            val spaces = m.optJSONArray("spaces") ?: JSONArray()
            Machine(
                id = m.getString("id"),
                label = m.getString("label"),
                status = m.getString("status"),
                detail = m.optString("detail"),
                spaces = (0 until spaces.length()).map { i ->
                    val s = spaces.getJSONObject(i)
                    Space(s.getString("id"), s.getString("name"), s.getString("status"), s.optBoolean("focused"))
                },
            )
        }

        fun parseAgents(array: JSONArray) = (0 until array.length()).map {
            val a = array.getJSONObject(it)
            Agent(
                endpoint = a.getString("endpoint"),
                machine = a.getString("machine"),
                pane = a.getString("pane"),
                space = a.getString("space"),
                status = a.getString("status"),
                agent = if (a.isNull("agent")) null else a.getString("agent"),
                title = a.getString("title"),
                detail = a.getString("detail"),
                stale = a.optBoolean("stale"),
            )
        }
    }
}
