package app.oasis.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.os.Bundle
import android.view.ViewGroup
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView

private val Background = Color(0xFF1A1B26)
private val Raised = Color(0xFF24283B)
private val Foreground = Color(0xFFC0CAF5)
private val Dim = Color(0xFF7A82A8)
private val Accent = Color(0xFF7AA2F7)

private fun statusColor(status: String) = when (status) {
    "working" -> Color(0xFFE0AF68)
    "blocked" -> Color(0xFFF7768E)
    "done" -> Color(0xFF9ECE6A)
    "online" -> Color(0xFF9ECE6A)
    "connecting", "reconnecting" -> Color(0xFFE0AF68)
    "attention" -> Color(0xFFF7768E)
    else -> Color(0xFF565F89)
}

private fun statusWord(status: String) = when (status) {
    "working" -> "working"
    "blocked" -> "needs you"
    "done" -> "done"
    "idle" -> "idle"
    else -> ""
}

sealed interface Screen {
    data object Agents : Screen
    data class Terminal(val title: String, val subtitle: String, val agent: Agent?) : Screen
    data object Settings : Screen
}

class MainActivity : ComponentActivity() {
    private lateinit var core: Core
    private lateinit var terminal: TerminalView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        core = Core(applicationContext)
        terminal = TerminalView(this, core)
        core.terminal = terminal
        core.start(::geometry)
        setContent {
            MaterialTheme(
                colorScheme = darkColorScheme(
                    primary = Accent,
                    background = Background,
                    surface = Background,
                    surfaceVariant = Raised,
                    onBackground = Foreground,
                    onSurface = Foreground,
                ),
            ) {
                Surface(Modifier.fillMaxSize(), color = Background) {
                    App(core, terminal) { core.restart(::geometry) }
                }
            }
        }
    }

    /** Before the terminal is laid out: the whole screen less its bars and the key row. */
    private fun geometry(): IntArray {
        if (terminal.cols > 0) return intArrayOf(terminal.cols, terminal.rows, terminal.cellW.toInt(), terminal.cellH.toInt())
        val metrics = resources.displayMetrics
        val chrome = (200 * metrics.density).toInt()
        return terminal.estimate(metrics.widthPixels, metrics.heightPixels - chrome)
    }

    override fun onResume() {
        super.onResume()
        if (core.handle != 0L) Native.setFocus(core.handle, true)
    }

    override fun onPause() {
        super.onPause()
        if (core.handle != 0L) Native.setFocus(core.handle, false)
    }

    override fun onDestroy() {
        core.stop()
        super.onDestroy()
    }
}

@Composable
private fun App(core: Core, terminal: TerminalView, restart: () -> Unit) {
    var screen by remember { mutableStateOf<Screen>(Screen.Agents) }
    val snackbar = remember { SnackbarHostState() }
    val notice by core.notice
    LaunchedEffect(notice) {
        notice?.let {
            snackbar.showSnackbar(it)
            core.notice.value = null
        }
    }
    BackHandler(screen != Screen.Agents) { screen = Screen.Agents }

    fun openAgent(agent: Agent) {
        core.openAgent(agent)
        screen = Screen.Terminal(agent.title, "${agent.machine} · ${spaceName(core, agent)}", agent)
    }

    Box(Modifier.fillMaxSize()) {
        when (val s = screen) {
            Screen.Agents -> AgentsScreen(
                core,
                onAgent = ::openAgent,
                onSpace = { machine, space ->
                    core.openSpace(machine, space)
                    screen = Screen.Terminal(space.name, machine.label, null)
                },
                onSettings = { screen = Screen.Settings },
            )
            is Screen.Terminal -> TerminalScreen(
                core,
                terminal,
                s,
                onBack = { screen = Screen.Agents },
                onSwipe = { step ->
                    val agents = core.agents.value
                    val at = agents.indexOfFirst { it.endpoint == s.agent?.endpoint && it.pane == s.agent.pane }
                    if (agents.isNotEmpty()) openAgent(agents[Math.floorMod(at + step, agents.size)])
                },
            )
            Screen.Settings -> SettingsScreen(core, terminal, onBack = { screen = Screen.Agents }, restart = {
                restart()
                screen = Screen.Agents
            })
        }
        SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter).systemBarsPadding().imePadding())
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AgentsScreen(
    core: Core,
    onAgent: (Agent) -> Unit,
    onSpace: (Machine, Space) -> Unit,
    onSettings: () -> Unit,
) {
    val machines by core.machines
    val agents by core.agents
    val starting by core.starting
    Column(Modifier.fillMaxSize().systemBarsPadding()) {
        TopAppBar(
            title = { Text("Oasis", fontWeight = FontWeight.SemiBold) },
            actions = {
                IconButton(onClick = onSettings) { Icon(Icons.Default.Settings, "Settings") }
            },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Background),
        )
        val offTailnet = machines.filter { core.waitsForTailscale(it) }
        if (offTailnet.isNotEmpty()) TailscaleBanner(offTailnet)
        if (starting || machines.isEmpty()) {
            Text(
                if (starting) "Setting up ssh…" else "Connecting…",
                color = Dim,
                modifier = Modifier.padding(24.dp),
            )
        }
        LazyColumn(Modifier.fillMaxSize(), contentPadding = androidx.compose.foundation.layout.PaddingValues(bottom = 24.dp)) {
            // Every agent on every machine first, in the order the bridge sends them,
            // which is also the order swiping in the terminal follows.
            if (agents.isNotEmpty()) {
                item(key = "agents") { SectionHeader("Agents") }
                items(agents, key = { "a:" + it.endpoint + it.pane }) {
                    AgentCard(it, place = "${it.machine} · ${spaceName(core, it)}".trimEnd(' ', '·')) { onAgent(it) }
                }
            }
            for (machine in machines) {
                item(key = "m:" + machine.id) {
                    MachineHeader(machine, waitsForTailscale = machine in offTailnet) { core.retry(machine) }
                }
                val counts = agents.filter { it.endpoint == machine.id }.groupingBy { it.space }.eachCount()
                items(machine.spaces, key = { "s:" + machine.id + it.id }) { space ->
                    SpaceRow(space, counts[space.id] ?: 0) { onSpace(machine, space) }
                }
            }
        }
    }
}

@Composable
private fun SectionHeader(text: String) {
    Text(
        text.uppercase(),
        color = Dim,
        fontSize = 13.sp,
        fontWeight = FontWeight.Bold,
        letterSpacing = 1.5.sp,
        modifier = Modifier.padding(start = 20.dp, end = 8.dp, top = 18.dp, bottom = 8.dp),
    )
}

@Composable
private fun StatusDot(status: String, size: Int = 12) {
    val pulse = if (status == "working" || status == "connecting" || status == "reconnecting") {
        val transition = rememberInfiniteTransition(label = "pulse")
        transition.animateFloat(0.35f, 1f, infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "alpha").value
    } else {
        1f
    }
    Box(Modifier.size(size.dp).alpha(pulse).clip(CircleShape).background(statusColor(status)))
}

/** Shown while machines on the tailnet are out of reach because Tailscale is off. */
@Composable
private fun TailscaleBanner(machines: List<Machine>) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val names = machines.joinToString(" and ") { it.label }
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Raised)
            .padding(16.dp),
    ) {
        Text("Tailscale is off", color = Foreground, fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
        Text(
            "$names ${if (machines.size == 1) "is" else "are"} on your tailnet. Turn Tailscale on and Oasis reconnects by itself.",
            color = Dim,
            fontSize = 15.sp,
            modifier = Modifier.padding(top = 4.dp, bottom = 10.dp),
        )
        val launch = context.packageManager.getLaunchIntentForPackage(Core.TAILSCALE)
        Button(onClick = {
            context.startActivity(
                launch ?: android.content.Intent(
                    android.content.Intent.ACTION_VIEW,
                    android.net.Uri.parse("market://details?id=${Core.TAILSCALE}"),
                ),
            )
        }) { Text(if (launch != null) "Open Tailscale" else "Get Tailscale") }
    }
}

@Composable
private fun MachineHeader(machine: Machine, waitsForTailscale: Boolean, onRetry: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 18.dp, bottom = 4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            StatusDot(machine.status, 10)
            Spacer(Modifier.width(10.dp))
            Text(machine.label.uppercase(), color = Dim, fontSize = 13.sp, fontWeight = FontWeight.Bold, letterSpacing = 1.5.sp)
            Spacer(Modifier.weight(1f))
            if (!waitsForTailscale && (machine.status == "attention" || machine.status == "reconnecting")) {
                IconButton(onClick = onRetry) { Icon(Icons.Default.Refresh, "Retry", tint = Dim) }
            }
        }
        if (waitsForTailscale) {
            Text("Waiting for Tailscale", color = Dim, fontSize = 13.sp)
        } else if (machine.detail.isNotBlank() && machine.status != "online") {
            Text(machine.detail, color = statusColor(machine.status), fontSize = 13.sp, maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
private fun SpaceRow(space: Space, agents: Int, onClick: () -> Unit) {
    val empty = agents == 0
    Row(
        Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 20.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            space.name.ifBlank { "space" },
            color = if (empty) Dim else Foreground,
            fontSize = 15.sp,
            fontWeight = if (empty) FontWeight.Normal else FontWeight.SemiBold,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Text(if (empty) "open" else if (agents == 1) "1 agent" else "$agents agents", color = Dim, fontSize = 13.sp)
    }
}

/** The name of the space an agent runs in, as its machine reports it. */
private fun spaceName(core: Core, agent: Agent) =
    core.machines.value.firstOrNull { it.id == agent.endpoint }
        ?.spaces?.firstOrNull { it.id == agent.space }?.name.orEmpty()

/** The agent tool's logo, or nothing when the app has none for it. */
@Composable
private fun AgentLogo(agent: Agent, size: Int = 22) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val id = remember(agent.logo) {
        agent.logo?.let { context.resources.getIdentifier("logo_$it", "drawable", context.packageName) } ?: 0
    }
    if (id != 0) {
        androidx.compose.foundation.Image(
            androidx.compose.ui.res.painterResource(id),
            contentDescription = agent.agent,
            modifier = Modifier.size(size.dp),
        )
    }
}

@Composable
private fun AgentCard(agent: Agent, place: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 4.dp)
            .clip(RoundedCornerShape(14.dp))
            .background(Raised)
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .alpha(if (agent.stale) 0.5f else 1f),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(agent.status)
        Spacer(Modifier.width(12.dp))
        AgentLogo(agent)
        if (agent.logo != null) Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    agent.title,
                    color = Foreground,
                    fontSize = 16.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Spacer(Modifier.width(8.dp))
                Text(statusWord(agent.status), color = statusColor(agent.status), fontSize = 13.sp, maxLines = 1)
            }
            if (agent.activity.isNotEmpty() || agent.detail.isNotBlank()) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 2.dp)) {
                    ActivityIcons(agent.activity)
                    if (agent.detail.isNotBlank()) {
                        if (agent.activity.isNotEmpty()) Spacer(Modifier.width(10.dp))
                        Text(agent.detail, color = Dim, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    }
                }
            }
            if (place.isNotBlank()) {
                Text(place, color = Dim, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.padding(top = 2.dp))
            }
        }
    }
}

/** Background work as the desktop shows it: colored Nerd Font icons with counts or schedules. */
@Composable
private fun ActivityIcons(parts: List<Activity>) {
    if (parts.isEmpty()) return
    val nerd = remember { FontFamily(androidx.compose.ui.text.font.Font(R.font.jetbrains_mono_regular)) }
    Text(
        androidx.compose.ui.text.buildAnnotatedString {
            parts.forEachIndexed { index, part ->
                if (index > 0) append("  ")
                withStyle(androidx.compose.ui.text.SpanStyle(color = Color(part.color))) {
                    if (part.icon.isNotEmpty()) {
                        withStyle(androidx.compose.ui.text.SpanStyle(fontFamily = nerd, fontSize = 17.sp)) { append(part.icon) }
                    }
                    if (part.detail.isNotEmpty()) append((if (part.icon.isNotEmpty()) " " else "") + part.detail)
                }
            }
        },
        fontSize = 14.sp,
        maxLines = 1,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun TerminalScreen(
    core: Core,
    terminal: TerminalView,
    screen: Screen.Terminal,
    onBack: () -> Unit,
    onSwipe: (Int) -> Unit,
) {
    terminal.onSwipe = onSwipe
    val live = screen.agent?.let { a -> core.agents.value.firstOrNull { it.endpoint == a.endpoint && it.pane == a.pane } }
    Column(Modifier.fillMaxSize().systemBarsPadding().imePadding()) {
        TopAppBar(
            navigationIcon = {
                IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Agents") }
            },
            title = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    live?.let {
                        StatusDot(it.status, 10)
                        Spacer(Modifier.width(10.dp))
                        AgentLogo(it, 20)
                        if (it.logo != null) Spacer(Modifier.width(10.dp))
                    }
                    Column {
                        Text(
                            live?.title ?: screen.title,
                            fontSize = 17.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Text(
                            live?.let { "${it.machine} · ${spaceName(core, it)}" } ?: screen.subtitle,
                            fontSize = 13.sp,
                            color = Dim,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            },
            actions = {
                IconButton(onClick = { terminal.paste() }) { Icon(Icons.Default.ContentPaste, "Paste") }
                IconButton(onClick = { terminal.toggleKeyboard() }) { Icon(Icons.Default.Keyboard, "Keyboard") }
            },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Background),
            expandedHeight = 56.dp,
        )
        AndroidView(
            factory = {
                (terminal.parent as? ViewGroup)?.removeView(terminal)
                terminal.also { it.post { it.syncSize(); it.requestFocus() } }
            },
            modifier = Modifier.weight(1f).fillMaxWidth(),
        )
        ExtraKeys(core, terminal)
    }
}

/** The keys a phone keyboard lacks, in one scrollable row above it. */
@Composable
private fun ExtraKeys(core: Core, terminal: TerminalView) {
    val ctrl by core.ctrl
    val alt by core.alt
    Row(
        Modifier.fillMaxWidth().background(Raised).horizontalScroll(rememberScrollState()).padding(horizontal = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        ExtraKey("Esc") { terminal.key(Keys.ESC) }
        ExtraKey("Tab") { terminal.key(Keys.TAB) }
        ExtraKey("Ctrl", active = ctrl) { core.ctrl.value = !ctrl }
        ExtraKey("Alt", active = alt) { core.alt.value = !alt }
        ExtraKey("←") { terminal.key(Keys.LEFT) }
        ExtraKey("↓") { terminal.key(Keys.DOWN) }
        ExtraKey("↑") { terminal.key(Keys.UP) }
        ExtraKey("→") { terminal.key(Keys.RIGHT) }
        ExtraKey("⇧Tab") { terminal.key(Keys.BACK_TAB) }
        ExtraKey("|") { terminal.key(Keys.char('|'.code)) }
        ExtraKey("/") { terminal.key(Keys.char('/'.code)) }
        ExtraKey("-") { terminal.key(Keys.char('-'.code)) }
        ExtraKey("~") { terminal.key(Keys.char('~'.code)) }
        ExtraKey("Home") { terminal.key(Keys.HOME) }
        ExtraKey("End") { terminal.key(Keys.END) }
        ExtraKey("PgUp") { terminal.key(Keys.PAGE_UP) }
        ExtraKey("PgDn") { terminal.key(Keys.PAGE_DOWN) }
    }
}

@Composable
private fun ExtraKey(label: String, active: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier
            .height(48.dp)
            .widthIn(min = 48.dp)
            .clip(RoundedCornerShape(8.dp))
            .background(if (active) Accent.copy(alpha = 0.35f) else Color.Transparent)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (active) Foreground else Foreground.copy(alpha = 0.85f), fontSize = 16.sp, fontFamily = FontFamily.Monospace)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScreen(core: Core, terminal: TerminalView, onBack: () -> Unit, restart: () -> Unit) {
    var machines by remember { mutableStateOf(core.prefs.machines) }
    var font by remember { mutableStateOf(core.prefs.fontSp) }
    var label by remember { mutableStateOf("") }
    var target by remember { mutableStateOf("") }
    val context = androidx.compose.ui.platform.LocalContext.current
    Column(Modifier.fillMaxSize().systemBarsPadding().imePadding()) {
        TopAppBar(
            navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            title = { Text("Settings") },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Background),
        )
        LazyColumn(Modifier.fillMaxSize().padding(horizontal = 20.dp)) {
            item {
                Heading("Terminal text size")
                Text("${font} sp  (pinch the terminal to change it too)", color = Dim, fontSize = 14.sp)
                Slider(
                    value = font,
                    onValueChange = { font = Math.round(it * 2) / 2f },
                    onValueChangeFinished = {
                        core.prefs.fontSp = font
                        terminal.setFontSp(font)
                    },
                    valueRange = Prefs.MIN_FONT_SP..Prefs.MAX_FONT_SP,
                )
                Text(
                    "claude ❯ ls ~/Work",
                    fontFamily = FontFamily.Monospace,
                    fontSize = font.sp,
                    color = Foreground,
                    modifier = Modifier.fillMaxWidth().background(Raised).padding(12.dp),
                )
            }
            item { Heading("Machines") }
            items(machines, key = { it.label }) { m ->
                Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(m.label, color = Foreground, fontSize = 16.sp)
                        Text(m.target, color = Dim, fontSize = 13.sp)
                    }
                    IconButton(onClick = { machines = machines - m }) { Icon(Icons.Default.Delete, "Remove", tint = Dim) }
                }
            }
            item {
                OutlinedTextField(label, { label = it }, label = { Text("Name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                OutlinedTextField(target, { target = it }, label = { Text("ssh target, user@host") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                Row(Modifier.padding(vertical = 8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        enabled = label.isNotBlank() && target.isNotBlank() && machines.none { it.label == label.trim() },
                        onClick = {
                            machines = machines + SavedMachine(label.trim(), target.trim())
                            label = ""
                            target = ""
                        },
                    ) { Text("Add") }
                    Button(
                        enabled = machines != core.prefs.machines,
                        onClick = {
                            core.prefs.machines = machines
                            restart()
                        },
                    ) { Text("Save and reconnect") }
                }
                HorizontalDivider(Modifier.padding(vertical = 12.dp))
                Heading("This phone's ssh key")
                Text(
                    "Add it to ~/.ssh/authorized_keys on each machine.",
                    color = Dim,
                    fontSize = 14.sp,
                )
                Text(
                    core.publicKey.ifBlank { "(not made yet)" },
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp,
                    color = Foreground,
                    modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp).background(Raised).padding(10.dp),
                )
                TextButton(onClick = {
                    context.getSystemService(ClipboardManager::class.java)
                        .setPrimaryClip(ClipData.newPlainText("ssh key", core.publicKey))
                }) { Text("Copy key") }
                Spacer(Modifier.height(24.dp))
            }
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(text, color = Accent, fontSize = 14.sp, fontWeight = FontWeight.Bold, modifier = Modifier.padding(top = 20.dp, bottom = 6.dp))
}
