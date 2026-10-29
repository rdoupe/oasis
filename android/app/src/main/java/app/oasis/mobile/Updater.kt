package app.oasis.mobile

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.mutableStateOf
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest
import kotlin.concurrent.thread

/**
 * Updates the app from one of its own machines, over the same ssh it already
 * uses: a machine that builds the app publishes it (android/publish.sh) as
 * `~/.local/share/oasis/android/update.json` beside `oasis.apk`. The newest
 * build any saved machine offers is fetched, checked against its SHA-256 and
 * handed to Android's package installer, which updates in place without asking
 * once this app installed itself before.
 */
class Updater(private val context: Context, private val core: Core) {
    data class Update(val target: String, val versionCode: Long, val versionName: String, val sha256: String)

    val available = mutableStateOf<Update?>(null)
    /** What the updater is doing, for the settings screen. */
    val state = mutableStateOf("")
    private val main = Handler(Looper.getMainLooper())
    private var checking = false
    private var lastCheck = 0L

    /** Checks each saved machine for a newer build; `always` skips the hourly limit. */
    fun check(always: Boolean = false) {
        val now = System.currentTimeMillis()
        if (checking || (!always && now - lastCheck < CHECK_EVERY_MS)) return
        checking = true
        lastCheck = now
        state.value = "checking…"
        val machines = core.prefs.machines
        thread(name = "oasis-update-check") {
            val newest = machines.mapNotNull { offer(it.target) }.maxByOrNull { it.versionCode }
            main.post {
                checking = false
                val newer = newest?.takeIf { it.versionCode > BuildConfig.VERSION_CODE }
                available.value = newer
                state.value = when {
                    newer != null -> "${newer.versionName} is ready"
                    newest == null -> "no machine offers a build"
                    else -> "up to date"
                }
            }
        }
    }

    private fun offer(target: String): Update? = runCatching {
        val text = core.sshOutput(target, "cat $DIR/update.json") ?: return null
        val json = JSONObject(text)
        Update(target, json.getLong("version_code"), json.optString("version_name"), json.getString("sha256").lowercase())
    }.getOrNull()

    /** Fetches the update, checks it, and installs it. */
    fun install(update: Update) {
        state.value = "downloading ${update.versionName}…"
        thread(name = "oasis-update-install") {
            val apk = File(context.cacheDir, "update.apk")
            val failure = try {
                apk.delete()
                if (!core.sshDownload(update.target, "cat $DIR/oasis.apk", apk)) {
                    "could not download the update"
                } else if (sha256(apk) != update.sha256) {
                    "the download did not match its checksum; try again"
                } else {
                    commit(apk)
                    null
                }
            } catch (e: Exception) {
                Log.w(Core.TAG, "update", e)
                "update failed: ${e.message}"
            }
            main.post {
                state.value = failure ?: "installing ${update.versionName}…"
                failure?.let { core.notice.value = it }
            }
        }
    }

    private fun commit(apk: File) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(context.packageName)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
        val id = installer.createSession(params)
        installer.openSession(id).use { session ->
            session.openWrite("oasis.apk", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val intent = Intent(context, InstallResult::class.java)
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
            session.commit(PendingIntent.getBroadcast(context, id, intent, flags).intentSender)
        }
    }

    companion object {
        const val DIR = ".local/share/oasis/android"
        const val CHECK_EVERY_MS = 60 * 60 * 1000L

        fun sha256(file: File): String {
            val digest = MessageDigest.getInstance("SHA-256")
            file.inputStream().use { input ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    digest.update(buffer, 0, n)
                }
            }
            return digest.digest().joinToString("") { "%02x".format(it) }
        }
    }
}

/** The installer's answer: shows Android's confirmation when it needs one. */
class InstallResult : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT) ?: return
                context.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
            PackageInstaller.STATUS_SUCCESS -> Unit
            else -> {
                val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
                Log.w(Core.TAG, "update not installed: $status $message")
                android.widget.Toast.makeText(context, "Update not installed: ${message ?: status}", android.widget.Toast.LENGTH_LONG).show()
            }
        }
    }
}
