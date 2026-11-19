package org.lanmesh.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Binder
import android.os.IBinder
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

internal data class GroupView(
    val config: GroupConfig,
    val online: Boolean = false,
    val error: String? = null,
    val members: List<Member> = emptyList()
)

private class GroupRuntime(val config: GroupConfig) {
    var session: MeshSession? = null
    var membersJob: Job? = null
    var membersDirty = false
}

class MeshService : Service() {
    inner class LocalBinder : Binder() { internal val service get() = this@MeshService }
    private val binder = LocalBinder()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val runtimes = mutableMapOf<UUID, GroupRuntime>()
    private val current = MutableStateFlow<Map<UUID, GroupView>>(emptyMap())
    internal val groups = current.asStateFlow()
    private var foreground = false

    override fun onCreate() {
        super.onCreate()
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "群组连接", NotificationManager.IMPORTANCE_LOW)
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // The launcher requests foreground startup only from a visible activity.
        startForeground(NOTICE, notification())
        foreground = true
        val saved = JSONArray(getSharedPreferences(PREFS, MODE_PRIVATE).getString("groups", "[]"))
        for (i in 0 until saved.length()) {
            val config = GroupConfig.from(saved.getJSONObject(i))
            if (config.groupId !in runtimes) attach(config)
        }
        updateNotification()
        return if (runtimes.isEmpty()) START_NOT_STICKY else START_STICKY
    }

    override fun onBind(intent: Intent): IBinder = binder

    internal suspend fun discover(): List<FoundRelay> = MeshSession.discover()

    internal fun join(relay: FoundRelay, nickname: String): UUID {
        val previous = runtimes[relay.group]?.config
        val config = GroupConfig(relay.group, previous?.deviceId ?: UUID.randomUUID(),
            relay.address, relay.name.ifBlank { relay.address }, nickname)
        if (previous != config || current.value[relay.group]?.error != null) {
            detach(relay.group)
            attach(config)
            persist()
        }
        if (!foreground) startForegroundService(Intent(this, MeshService::class.java))
        return relay.group
    }

    internal fun leave(id: UUID) {
        detach(id)
        persist()
        updateNotification()
    }

    private fun attach(config: GroupConfig) {
        val runtime = GroupRuntime(config)
        val id = config.groupId
        runtimes[id] = runtime
        current.value += id to GroupView(config)
        scope.launch {
            // Opening registers a native handle. A close or replacement during open must reclaim it.
            try {
                withContext(NonCancellable) {
                    val session = MeshSession.open(config) { raw ->
                        scope.launch {
                            if (runtimes[id] === runtime) event(runtime, JSONObject(raw))
                        }
                    }
                    if (runtimes[id] !== runtime) session.close() else {
                        runtime.session = session
                        refreshMembers(runtime)
                    }
                }
            } catch (e: Exception) {
                if (runtimes[id] === runtime) change(id) { it.copy(error = e.message) }
            }
        }
    }

    private fun event(runtime: GroupRuntime, event: JSONObject) {
        val id = runtime.config.groupId
        when (event.getString("type")) {
            "connection" -> {
                change(id) { it.copy(online = event.getBoolean("online"), error = null) }
                refreshMembers(runtime)
                updateNotification()
            }
            "members" -> refreshMembers(runtime)
            "warning" -> change(id) { it.copy(error = event.getString("text")) }
        }
    }

    private fun refreshMembers(runtime: GroupRuntime) {
        val session = runtime.session ?: return
        runtime.membersDirty = true
        if (runtime.membersJob?.isActive == true) return
        runtime.membersJob = scope.launch {
            do {
                runtime.membersDirty = false
                val members = try { session.members() } catch (e: Exception) { return@launch }
                if (runtimes[runtime.config.groupId] !== runtime) return@launch
                change(runtime.config.groupId) { it.copy(members = members) }
            } while (runtime.membersDirty)
        }
    }

    private fun change(id: UUID, edit: (GroupView) -> GroupView) {
        val old = current.value[id] ?: return
        current.value += id to edit(old)
    }

    private fun persist() {
        val saved = JSONArray()
        runtimes.values.forEach { saved.put(it.config.json()) }
        getSharedPreferences(PREFS, MODE_PRIVATE).edit().putString("groups", saved.toString()).apply()
    }

    private fun detach(id: UUID) {
        val runtime = runtimes.remove(id) ?: return
        current.value -= id
        runtime.membersJob?.cancel()
        runtime.session?.let { session -> scope.launch(NonCancellable) { session.close() } }
    }

    private fun notification(): Notification {
        val launch = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val online = current.value.values.count { it.online }
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_mesh_status)
            .setContentTitle("LAN Mesh")
            .setContentText("已连接 $online / ${runtimes.size} 个群组")
            .setContentIntent(launch).setOngoing(true).setOnlyAlertOnce(true).build()
    }

    private fun updateNotification() {
        if (!foreground) return
        if (runtimes.isEmpty()) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            foreground = false
            stopSelf()
        } else {
            getSystemService(NotificationManager::class.java).notify(NOTICE, notification())
        }
    }

    override fun onDestroy() {
        runtimes.keys.toList().forEach(::detach)
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "mesh_connections"
        private const val NOTICE = 1
        private const val PREFS = "mesh_sessions"
        internal fun restore(context: Context) {
            val saved = context.getSharedPreferences(PREFS, MODE_PRIVATE).getString("groups", "[]")
            if (saved != "[]") context.startForegroundService(Intent(context, MeshService::class.java))
        }
    }
}
