package app.oasis.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
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

/** One piece of an agent's background work: a Nerd Font icon, its count or schedule, its color. */
data class Activity(val icon: String, val detail: String, val color: Int)

data class Agent(
    val endpoint: String,
    val machine: String,
    val pane: String,
    val space: String,
    val status: String,
    val agent: String?,
    /** The bundled logo for the agent's tool (`logo_<name>`), when there is one. */
    val logo: String?,
    val title: String,
    val detail: String,
    val stale: Boolean,
    val activity: List<Activity>,
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
    /** Whether a VPN, normally Tailscale, is up on this phone. */
    val vpnUp = mutableStateOf(false)
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

    init {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        vpnUp.value = connectivity.allNetworks.any {
            connectivity.getNetworkCapabilities(it)?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
        }
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_VPN)
            .removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
            .build()
        connectivity.registerNetworkCallback(
            request,
            object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    main.post {
                        vpnUp.value = true
                        // Machines that waited for Tailscale connect now, not at the next retry.
                        machines.value.filter { it.status != "online" }.forEach(::retry)
                    }
                }

                override fun onLost(network: Network) {
                    main.post { vpnUp.value = false }
                }
            },
        )
    }

    /** A machine out of reach because it is on the tailnet and no VPN is up. */
    fun waitsForTailscale(machine: Machine): Boolean {
        if (machine.status == "online" || vpnUp.value) return false
        val target = prefs.machines.firstOrNull { it.label == machine.label }?.target ?: return false
        return isTailnetHost(target.substringAfter('@'))
    }

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
        val raw = Native.drain(handle)
        if (BuildConfig.DEBUG && (raw.contains("\"machines\"") || raw.contains("\"agents\""))) Log.d(TAG, raw)
        val out = JSONObject(raw)
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

    /**
     * Sends one API operation, e.g. `("pane.close", {"pane_id": ...})`. The core
     * reports a failed one as a notice; one it cannot send is shown here.
     */
    fun request(endpoint: String, method: String, params: Map<String, Any>) {
        if (handle == 0L) return
        Native.request(handle, endpoint, method, JSONObject(params).toString())?.let { notice.value = it }
    }

    /** The shown machine and its focused pane, which is what the terminal screen shows. */
    fun focusedPane(): Pair<String, String>? {
        if (handle == 0L) return null
        val endpoint = Native.activeMachine(handle) ?: return null
        val pane = Native.focusedPane(handle) ?: return null
        return endpoint to pane
    }

    /** Keys for the pane as they are, without the armed Ctrl or Alt from the key row. */
    fun keys(vararg keys: Pair<Int, Int>) {
        if (handle == 0L) return
        for ((code, modifiers) in keys) Native.key(handle, code, modifiers)
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
        val config = File(home, ".config/oasis")
        config.mkdirs()
        File(config, "config.toml").writeText(SIDEBAR_CONFIG)
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
        const val TAILSCALE = "com.tailscale.ipn"

        /**
         * The agent rows the desktop Oasis config lays out: the state, the agent's
         * mark (a logo here), the space and background work, then the agent's own
         * session title, e.g. Claude's summary of the conversation.
         */
        val SIDEBAR_CONFIG = """
            |# Written by the Oasis app at each start.
            |[ui]
            |status_indicators = "symbols"
            |agent_panel_sort = "spaces"
            |
            |[ui.sidebar.agents]
            |rows = [["state_icon", "machine", "workspace", "background"], ["agent"]]
            |
            |[ui.sidebar.agents.rows_by_agent]
            |claude = [["state_icon", "agent_icon", "workspace", "background"], ["terminal_title_stripped"]]
            |codex = [["state_icon", "agent_icon", "workspace", "background"], ["terminal_title_stripped"]]
            |cursor = [["state_icon", "agent_icon", "workspace", "background"], ["terminal_title_stripped"]]
            |opencode = [["state_icon", "agent_icon", "workspace", "background"], ["terminal_title_stripped"]]
            |""".trimMargin()

        /** A MagicDNS name, or an address in the ranges Tailscale assigns (its CGNAT IPv4 block and its ULA IPv6 prefix). */
        fun isTailnetHost(host: String): Boolean {
            val h = host.trim().trimEnd('.').lowercase().removeSurrounding("[", "]")
            if (h.endsWith(".ts.net")) return true
            if (h.startsWith("fd7a:115c:a1e0:")) return true
            val octets = h.split('.').mapNotNull { it.toIntOrNull() }
            return octets.size == 4 && h.count { it == '.' } == 3 && octets[0] == 100 && octets[1] in 64..127
        }

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
                logo = if (a.isNull("logo")) null else a.getString("logo"),
                title = a.getString("title"),
                detail = a.getString("detail"),
                stale = a.optBoolean("stale"),
                activity = a.optJSONArray("activity")?.let { parts ->
                    (0 until parts.length()).map { i ->
                        val p = parts.getJSONObject(i)
                        Activity(p.getString("icon"), p.getString("detail"), android.graphics.Color.parseColor(p.getString("color")))
                    }
                }.orEmpty(),
            )
        }
    }
}
